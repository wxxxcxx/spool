use std::collections::HashSet;
use std::time::Duration;

use bevy::ecs::component::Component;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Without;
use bevy::ecs::system::{Commands, Query, Res, ResMut, SystemParam};
use bevy::time::{Time, Timer, TimerMode};
use tracing::{debug, warn};

use crate::config::Config;
use crate::ecs::focus::{FocusCoordinator, FocusSignal};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    PreviousTiledStrip, RepositionMarker, ResizeMarker, SendMessageTrigger, SpawnWindowTrigger,
};
use crate::events::{DestroySource, Event, ReconcileScope};
use crate::manager::{Application, Window, WindowManager};

const CONFIRMATION_DELAY: Duration = Duration::from_millis(250);

/// The AX window is no longer usable, but CoreGraphics has not yet confirmed
/// that its surface left the screen. Such windows are excluded from normal
/// window queries immediately while retaining enough state to recover.
#[derive(Component, Debug)]
pub(crate) struct WindowUnavailable {
    confirmation: Timer,
}

impl WindowUnavailable {
    fn new() -> Self {
        Self {
            confirmation: Timer::new(CONFIRMATION_DELAY, TimerMode::Once),
        }
    }
}

#[derive(SystemParam)]
pub(super) struct ReconcileState<'w, 's> {
    applications: Query<'w, 's, (bevy::ecs::entity::Entity, &'static Application)>,
    windows: Query<
        'w,
        's,
        (
            bevy::ecs::entity::Entity,
            &'static Window,
            &'static ChildOf,
            Option<&'static mut WindowUnavailable>,
        ),
    >,
    previous_strips: Query<'w, 's, &'static PreviousTiledStrip>,
    workspaces: Query<'w, 's, &'static mut LayoutStrip, Without<Window>>,
    focus: ResMut<'w, FocusCoordinator>,
}

/// Reconciles the macOS window inventory with ECS. Query failures are
/// deliberately fail-open: absence is actionable only when both AX and the
/// full `WindowServer` session inventory succeeded.
pub(super) fn reconcile_windows(
    mut messages: MessageReader<Event>,
    state: ReconcileState,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut commands: Commands,
) {
    let ReconcileState {
        applications,
        mut windows,
        previous_strips,
        mut workspaces,
        mut focus,
    } = state;
    let mut all = false;
    let mut pids = HashSet::new();
    for event in messages.read() {
        let Event::ReconcileWindows { scope } = event else {
            continue;
        };
        match scope {
            ReconcileScope::Application(pid) => {
                pids.insert(*pid);
            }
            ReconcileScope::All => all = true,
        }
    }
    if !all && pids.is_empty() {
        return;
    }

    let Some(window_server) = window_manager
        .windows_in_session()
        .map(HashSet::<_>::from_iter)
    else {
        warn!("window reconciliation skipped: unable to read the WindowServer inventory");
        return;
    };

    for (app_entity, app) in &applications {
        let pid = app.pid();
        if !all && !pids.contains(&pid) {
            continue;
        }
        let Ok(observed) = app.window_ids().inspect_err(|error| {
            warn!(pid, %error, "window reconciliation skipped application");
        }) else {
            continue;
        };
        let observed = HashSet::<_>::from_iter(observed);

        let mut tracked = HashSet::new();
        for (entity, window, parent, mut unavailable) in &mut windows {
            if parent.parent() != app_entity {
                continue;
            }
            let window_id = window.id();
            tracked.insert(window_id);
            if observed.contains(&window_id) {
                if unavailable.is_some() && window.role().is_ok() {
                    restore_window(entity, &previous_strips, &mut workspaces, &mut commands);
                }
                continue;
            }
            if window_server.contains(&window_id) {
                if let Some(unavailable) = unavailable.as_mut() {
                    unavailable.confirmation.reset();
                } else {
                    isolate_window(entity, &mut workspaces, &mut focus, &mut commands);
                }
                continue;
            }

            debug!(
                window_id,
                pid, "AX and WindowServer inventories confirmed missing window"
            );
            commands.trigger(SendMessageTrigger(Event::WindowDestroyed {
                window_id,
                source: DestroySource::Reconciliation,
            }));
        }

        let missing = observed
            .iter()
            .filter(|window_id| !tracked.contains(window_id))
            .copied()
            .collect::<HashSet<_>>();
        if !missing.is_empty() {
            let discovered = app
                .window_list(&config)
                .into_iter()
                .filter(|window| missing.contains(&window.id()))
                .collect::<Vec<_>>();
            if !discovered.is_empty() {
                debug!(
                    pid,
                    count = discovered.len(),
                    "reconciliation found windows"
                );
                commands.trigger(SpawnWindowTrigger(discovered));
            }
        }
    }
}

fn isolate_window(
    entity: bevy::ecs::entity::Entity,
    workspaces: &mut Query<&mut LayoutStrip, Without<Window>>,
    focus: &mut FocusCoordinator,
    commands: &mut Commands,
) {
    focus.observe(FocusSignal::Invalidated { entity });
    for mut strip in workspaces.iter_mut() {
        if !strip.contains(entity) {
            continue;
        }
        let previous = PreviousTiledStrip {
            workspace_id: strip.id(),
            index: strip.index_of(entity).unwrap_or(strip.len()),
        };
        strip.remove(entity);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert((WindowUnavailable::new(), previous));
            entity_commands.remove::<(RepositionMarker, ResizeMarker)>();
        }
        debug!(?entity, "isolated unavailable window from layout and focus");
        return;
    }

    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(WindowUnavailable::new());
        entity_commands.remove::<(RepositionMarker, ResizeMarker)>();
    }
}

fn restore_window(
    entity: bevy::ecs::entity::Entity,
    previous_strips: &Query<&PreviousTiledStrip>,
    workspaces: &mut Query<&mut LayoutStrip, Without<Window>>,
    commands: &mut Commands,
) {
    let previous = previous_strips.get(entity).copied().ok();
    if let Some(previous) = previous
        && let Some(mut strip) = workspaces
            .iter_mut()
            .find(|strip| strip.id() == previous.workspace_id)
    {
        strip.insert_at(previous.index, entity);
    }
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.remove::<(WindowUnavailable, PreviousTiledStrip)>();
    }
    debug!(?entity, "restored available window to layout");
}

/// Rechecks only the owning application after the public CG surface has had a
/// short opportunity to disappear. This avoids a permanent global poll.
pub(super) fn confirm_unavailable_windows(
    time: Res<Time>,
    mut windows: Query<(&mut WindowUnavailable, &ChildOf)>,
    applications: Query<&Application>,
    mut commands: Commands,
) {
    let mut pids = HashSet::new();
    for (mut unavailable, parent) in &mut windows {
        unavailable.confirmation.tick(time.delta());
        if unavailable.confirmation.just_finished()
            && let Ok(app) = applications.get(parent.parent())
        {
            pids.insert(app.pid());
        }
    }
    for pid in pids {
        commands.trigger(SendMessageTrigger(Event::ReconcileWindows {
            scope: ReconcileScope::Application(pid),
        }));
    }
}

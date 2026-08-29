use std::collections::HashMap;
use std::time::Duration;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::{Add, Remove};
use bevy::ecs::observer::On;
use bevy::ecs::query::{Added, Has, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, Populated, Query, Res, ResMut, Single};
use bevy::math::IRect;
use bevy::prelude::Event as BevyEvent;
use bevy::time::common_conditions::on_timer;
use tracing::{Level, debug, error, instrument, trace, warn};

use super::{FocusedMarker, MouseHeldMarker, SystemTheme};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{ActiveDisplay, GlobalState, WindowCtx, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, Position, RaiseWindow, Scrolling, SendMessageTrigger,
    SpawnCommandsExt, StrayFocusEvent,
};
use crate::events::Event;
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, WinID, WorkspaceId};

const REFRESH_WINDOW_CHECK_FREQ_MS: u64 = 1000;

#[derive(Default)]
pub struct TierMemory {
    pub last_tiled: Option<Entity>,
    pub last_floating: Option<Entity>,
}

/// Keyed by `WorkspaceId` so toggling on one Space can't reach a window last
/// focused on another. Cleared on entity despawn (`forget`) so recycled
/// Entity IDs can't resolve to the wrong window, and on workspace despawn
/// (`forget_workspace`) to bound the map.
#[derive(Default, Resource)]
pub struct FocusHistory {
    by_workspace: HashMap<WorkspaceId, TierMemory>,
}

impl FocusHistory {
    pub fn record(
        &mut self,
        workspace: WorkspaceId,
        entity: Entity,
        floating: bool,
        visible: bool,
    ) {
        if !visible {
            return;
        }
        let slot = self.by_workspace.entry(workspace).or_default();
        if floating {
            slot.last_floating = Some(entity);
        } else {
            slot.last_tiled = Some(entity);
        }
    }

    pub fn last_tiled(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace.get(&workspace).and_then(|t| t.last_tiled)
    }

    pub fn last_floating(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)
            .and_then(|t| t.last_floating)
    }

    pub fn forget(&mut self, entity: Entity) {
        for slot in self.by_workspace.values_mut() {
            if slot.last_tiled == Some(entity) {
                slot.last_tiled = None;
            }
            if slot.last_floating == Some(entity) {
                slot.last_floating = None;
            }
        }
    }

    pub fn forget_workspace(&mut self, workspace: WorkspaceId) {
        self.by_workspace.remove(&workspace);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ActualFocus {
    #[default]
    Unknown,
    Pending {
        generation: u64,
        pid: Option<Pid>,
        candidate: Option<WinID>,
    },
    Tracked(WinID),
    Outside,
}

/// Resolves asynchronous macOS focus signals without letting an older query
/// overwrite a newer application transition. `FocusedMarker` remains the
/// immediate navigation target while this resource records the confidence of
/// the OS-facing observation.
#[derive(Default, Resource)]
pub struct FocusResolution {
    generation: u64,
    actual: ActualFocus,
}

impl FocusResolution {
    pub(super) fn begin_pending(&mut self, pid: Option<Pid>, candidate: Option<WinID>) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.actual = ActualFocus::Pending {
            generation: self.generation,
            pid,
            candidate,
        };
        self.generation
    }

    pub(super) fn accept_candidate(
        &mut self,
        generation: Option<u64>,
        pid: Option<Pid>,
        candidate: WinID,
    ) -> Option<u64> {
        let generation = if let Some(generation) = generation {
            if generation != self.generation {
                return None;
            }
            generation
        } else {
            self.begin_pending(pid, Some(candidate))
        };
        self.actual = ActualFocus::Pending {
            generation,
            pid,
            candidate: Some(candidate),
        };
        Some(generation)
    }

    pub(super) fn confirm(&mut self, generation: u64, window_id: WinID) -> bool {
        if generation != self.generation {
            return false;
        }
        self.actual = ActualFocus::Tracked(window_id);
        true
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    pub(super) fn mark_unknown(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.actual = ActualFocus::Unknown;
        true
    }

    pub(super) fn mark_outside(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.actual = ActualFocus::Outside;
        true
    }

    pub(super) fn suppresses_recovery(&self) -> bool {
        matches!(
            self.actual,
            ActualFocus::Pending { .. } | ActualFocus::Outside
        )
    }

    pub(super) fn visual_window_id(&self) -> Option<WinID> {
        match self.actual {
            ActualFocus::Pending { candidate, .. } => candidate,
            ActualFocus::Tracked(window_id) => Some(window_id),
            ActualFocus::Unknown | ActualFocus::Outside => None,
        }
    }
}

pub struct FocusEventsPlugin;

impl Plugin for FocusEventsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FocusHistory>();
        app.init_resource::<FocusResolution>();
        app.add_systems(
            PostUpdate,
            (
                autocenter_window_on_focus.after(super::systems::animate_resize_entities),
                mouse_follows_focus.after(super::systems::animate_resize_entities),
                recover_lost_focus.run_if(on_timer(Duration::from_millis(
                    REFRESH_WINDOW_CHECK_FREQ_MS,
                ))),
            ),
        );
        app.add_observer(dim_remove_window_trigger)
            .add_observer(dim_window_trigger)
            .add_observer(maintain_focus_singleton)
            .add_observer(virtual_strip_activated)
            .add_observer(stray_focus_observer)
            .add_observer(focus_window_trigger)
            .add_observer(raise_window_trigger);
    }
}

#[derive(BevyEvent)]
pub(super) struct FocusWindow {
    pub entity: Entity,
    pub raise: bool,
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn maintain_focus_singleton(
    trigger: On<Add, FocusedMarker>,
    windows: Query<(Entity, Has<FocusedMarker>), With<Window>>,
    mut config: GlobalState,
    mut commands: Commands,
) {
    let focused_entity = trigger.event().entity;

    for (entity, focused) in windows {
        if focused
            && entity != focused_entity
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            debug!("window {entity} lost focus.");
            entity_commands.try_remove::<FocusedMarker>();
        }
    }

    // Check if the reshuffle was caused by a keyboard switch or mouse move.
    // Skip reshuffle if caused by mouse - because then it won't center.
    if config.ffm_flag().is_none() {
        config.set_skip_reshuffle(false);
    }
    config.set_ffm_flag(None);
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn autocenter_window_on_focus(
    focused: Single<Entity, Added<FocusedMarker>>,
    mouse_held: Query<&MouseHeldMarker>,
    global_state: GlobalState,
    active_display: ActiveDisplay,
    mut ctx: WindowCtx,
) {
    let entity = *focused;

    if global_state.skip_reshuffle() || global_state.initializing() || !mouse_held.is_empty() {
        return;
    }
    if active_display.active_strip().tabbed(entity) {
        return;
    }
    if ctx.config.auto_center()
        && ctx
            .windows
            .get_tracked(entity)
            .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
        && let Some(size) = ctx.windows.size(entity)
        && let Some(mut origin) = ctx.windows.origin(entity)
    {
        let center = active_display.bounds().center();
        origin.x = center.x - size.x / 2;
        ctx.commands.reposition_entity(entity, origin);
    }
    ctx.commands.reshuffle_around(entity);
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn mouse_follows_focus(
    focused: Single<Entity, Added<FocusedMarker>>,
    windows: Windows,
    global_state: GlobalState,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
    displays: Query<&Display>,
    workspaces: Query<(
        &LayoutStrip,
        &ChildOf,
        Option<&Scrolling>,
        Has<ActiveWorkspaceMarker>,
    )>,
) {
    let entity = *focused;
    let Some(window) = windows.get(entity) else {
        return;
    };
    if workspaces
        .iter()
        .find_map(|(_, _, scrolling, active)| if active { scrolling } else { None })
        .is_some_and(|scrolling| scrolling.is_user_swiping)
    {
        debug!("Suppressing center mouse due to a swipe");
        return;
    }

    trace!(
        "window {}, skip_reshuffle {}, ffm flag {:?}.",
        window.id(),
        global_state.skip_reshuffle(),
        global_state.ffm_flag()
    );
    if config.mouse_follows_focus()
        && !global_state.skip_reshuffle()
        && global_state.ffm_flag().is_none_or(|id| id != window.id())
        && let Some(frame) = windows.moving_frame(entity)
        && let Some(display_bounds) = workspaces
            .into_iter()
            .find_map(|(strip, child, _, _)| strip.contains(entity).then_some(child))
            .and_then(|child| displays.get(child.parent()).ok())
            .map(Display::bounds)
    {
        let visible = display_bounds.intersect(frame);
        // If the overlap is smaller than 50x50, the window is probably hidden
        // off screen, so do not move the mouse.
        if visible.size().length_squared() > 5000 {
            let origin = visible.center();
            debug!("centering on {} {origin}", window.id());
            window_manager.warp_mouse(origin);
        }
    }
}

fn dim_window_trigger(
    trigger: On<Add, FocusedMarker>,
    windows: Windows,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    theme: Option<Res<SystemTheme>>,
) {
    let Some(window) = windows.get(trigger.event().entity) else {
        return;
    };

    let dark = theme.is_some_and(|theme| theme.is_dark);
    if config.window_dim_ratio(dark).is_some() {
        window_manager.dim_windows(&[window.id()], 0.0);
    }
}

fn dim_remove_window_trigger(
    trigger: On<Remove, FocusedMarker>,
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    theme: Option<Res<SystemTheme>>,
) {
    let Some((window, _, state)) = windows.get_tracked(trigger.event().entity) else {
        return;
    };
    if !state.is_visible() {
        return;
    }

    let active_strip = active_display.active_strip();
    let same_space = if state.is_floating() {
        window_manager
            .windows_in_workspace(active_strip.id())
            .is_ok_and(|ids| ids.contains(&window.id()))
    } else {
        active_strip.contains(trigger.event().entity)
    };
    if !same_space {
        // Do not dim the window losing focus on another display or Space.
        return;
    }

    let dark = theme.is_some_and(|theme| theme.is_dark);
    if let Some(dim_ratio) = config.window_dim_ratio(dark) {
        window_manager.dim_windows(&[window.id()], dim_ratio);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn virtual_strip_activated(
    trigger: On<Add, FocusedMarker>,
    workspaces: Query<(Entity, &LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    mut commands: Commands,
) {
    let owner_strip = workspaces.into_iter().find_map(|(entity, strip, active)| {
        (strip.contains(trigger.entity) && !active).then_some(entity)
    });
    if let Some(entity) = owner_strip
        && let Ok(mut entity_commands) = commands.get_entity(entity)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }
}

fn focus_window_trigger(
    trigger: On<FocusWindow>,
    windows: Windows,
    apps: Query<&Application>,
    mut focus_resolution: ResMut<FocusResolution>,
) {
    let FocusWindow { entity, raise } = *trigger.event();
    let Some(window) = windows.get(entity) else {
        return;
    };
    let Some(psn) = windows.psn(window.id(), &apps) else {
        return;
    };
    focus_resolution.begin_pending(window.pid().ok(), Some(window.id()));
    if !raise
        && let Some((focused_window, _)) = windows.focused()
        && let Some(focused_psn) = windows.psn(focused_window.id(), &apps)
    {
        window.focus_without_raise(psn, focused_window, focused_psn);
    } else {
        window.focus_with_raise(psn);
    }
}

fn raise_window_trigger(
    trigger: On<RaiseWindow>,
    windows: Query<(Entity, &Window, &Position, &Bounds)>,
    active_display: ActiveDisplay,
    config: Res<Config>,
) {
    let RaiseWindow { entity, with_strip } = *trigger.event();

    let Ok((focus, window, _, _)) = windows.get(entity) else {
        return;
    };

    if with_strip {
        let viewport = active_display.actual_bounds(&config);
        let strip = active_display.active_strip();
        strip
            .all_windows()
            .into_iter()
            .filter_map(|entity| {
                if entity == focus {
                    None
                } else {
                    windows.get(entity).ok()
                }
            })
            .filter(|(_, _, origin, size)| {
                let frame = IRect::from_corners(origin.0, origin.0 + size.0);
                viewport.intersect(frame).width() > 50
            })
            .for_each(|(_, window, _, _)| {
                window.raise_without_focus();
            });
    }

    // Raise the focused window last, because raised windows get OS focus events.
    window.raise_without_focus();
}

#[instrument(level = Level::DEBUG, skip_all)]
fn recover_lost_focus(
    windows: Windows,
    active_workspace: Query<&LayoutStrip, With<ActiveWorkspaceMarker>>,
    focus_resolution: Res<FocusResolution>,
    mut commands: Commands,
) {
    if windows.focused().is_some() || focus_resolution.suppresses_recovery() {
        return;
    }
    error!("Lost focus marker, recovering!");
    if let Ok(strip) = active_workspace
        .single()
        .inspect_err(|err| error!("Unable to get current workspace: {err}"))
        && let Some(entity) = strip.first().ok().and_then(|col| col.top())
    {
        commands.focus_entity(entity, false);
    }
}

pub(super) fn stray_focus_observer(
    trigger: On<Add, Window>,
    focus_events: Populated<(Entity, &StrayFocusEvent)>,
    windows: Windows,
    mut commands: Commands,
) {
    let entity = trigger.event().entity;
    let Some(window_id) = windows.get(entity).map(|window| window.id()) else {
        return;
    };

    focus_events
        .iter()
        .filter(|(_, stray_focus)| stray_focus.0 == window_id)
        .for_each(|(timeout_entity, _)| {
            debug!("Re-queueing lost focus event for window id {window_id}.");
            commands.trigger(SendMessageTrigger(Event::window_focused(window_id)));
            if let Ok(mut entity_commands) = commands.get_entity(timeout_entity) {
                entity_commands.try_despawn();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::World;

    #[test]
    fn record_and_read_per_tier() {
        let mut world = World::new();
        let tiled = world.spawn(()).id();
        let floating = world.spawn(()).id();
        let mut history = FocusHistory::default();

        history.record(1, tiled, false, true);
        history.record(1, floating, true, true);

        assert_eq!(history.last_tiled(1), Some(tiled));
        assert_eq!(history.last_floating(1), Some(floating));
    }

    #[test]
    fn record_ignores_minimized_and_hidden() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut history = FocusHistory::default();

        history.record(1, entity, false, false);
        history.record(1, entity, true, false);

        assert_eq!(history.last_tiled(1), None);
        assert_eq!(history.last_floating(1), None);
    }

    #[test]
    fn per_workspace_isolation() {
        let mut world = World::new();
        let a = world.spawn(()).id();
        let b = world.spawn(()).id();
        let mut history = FocusHistory::default();

        history.record(1, a, false, true);
        history.record(2, b, false, true);

        assert_eq!(history.last_tiled(1), Some(a));
        assert_eq!(history.last_tiled(2), Some(b));
    }

    #[test]
    fn forget_clears_entity_across_workspaces() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        let other = world.spawn(()).id();
        let mut history = FocusHistory::default();

        history.record(1, target, false, true);
        history.record(2, target, true, true);
        history.record(2, other, false, true);

        history.forget(target);

        assert_eq!(history.last_tiled(1), None);
        assert_eq!(history.last_floating(2), None);
        assert_eq!(history.last_tiled(2), Some(other));
    }

    #[test]
    fn forget_workspace_drops_entry() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut history = FocusHistory::default();

        history.record(1, entity, false, true);
        history.forget_workspace(1);

        assert_eq!(history.last_tiled(1), None);
    }
}

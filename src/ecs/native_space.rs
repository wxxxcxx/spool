use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Local, Query, Res};
use tracing::{Level, debug, error, instrument, warn};

use crate::commands::{Command, MoveFocus};
use crate::config::Config;
use crate::ecs::SpawnCommandsExt;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::Windows;
use crate::events::Event;
use crate::manager::{Display, NativeSpaceIntent, WindowManager};
use crate::platform::WorkspaceId;
pub use spool_shared_types::state::SpaceKind;

/// The macOS-managed Space represented by a layout strip.
///
/// Every Space owns exactly one `LayoutStrip`; the private platform
/// adapter is the sole source of this session-scoped identity and ordering.
#[derive(Clone, Component, Copy, Debug, Eq, PartialEq)]
pub struct NativeSpace {
    pub id: WorkspaceId,
    pub ordinal: u32,
    pub kind: SpaceKind,
}

impl NativeSpace {
    pub fn new(id: WorkspaceId, ordinal: usize, fullscreen: bool) -> Self {
        Self {
            id,
            ordinal: ordinal.try_into().unwrap_or(u32::MAX),
            kind: if fullscreen {
                SpaceKind::Fullscreen
            } else {
                SpaceKind::User
            },
        }
    }
}

/// Marks the Space currently visible on its physical display.
///
/// Unlike `ActiveWorkspaceMarker`, this is deliberately not a global
/// singleton: with separate Spaces enabled, every connected display has one
/// visible Space at the same time.
#[derive(Component, Debug)]
pub struct VisibleNativeSpaceMarker;

#[derive(Debug)]
struct PendingMove {
    window_ids: Vec<i32>,
    target_space_id: WorkspaceId,
    follow_window_id: Option<i32>,
    submitted: Instant,
}

#[derive(Debug)]
struct PendingFollow {
    window_id: i32,
    target_space_id: WorkspaceId,
    submitted: Instant,
}

#[derive(Default, Resource)]
pub(crate) struct NativeSpaceTransactions {
    moves: Vec<PendingMove>,
    follows: Vec<PendingFollow>,
}

pub(crate) fn handle_focus_window_commands(
    mut messages: MessageReader<Event>,
    windows: Windows,
    spaces: Query<(&LayoutStrip, Has<VisibleNativeSpaceMarker>)>,
    mut commands: Commands,
) {
    for window_id in messages.read().filter_map(|event| match event {
        Event::Command {
            command: Command::FocusWindow { window_id },
        } => Some(*window_id),
        _ => None,
    }) {
        let Some((_, entity)) = windows.find(window_id) else {
            warn!(window_id, "window is not tracked");
            continue;
        };
        if spaces
            .iter()
            .any(|(strip, visible)| strip.contains(entity) && !visible)
        {
            warn!(window_id, "cannot focus a window on an invisible Space");
            continue;
        }
        commands.focus_entity(entity, true);
    }
}

pub(crate) fn handle_native_space_commands(
    mut messages: MessageReader<Event>,
    windows: Windows,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
    mut transactions: bevy::ecs::system::ResMut<NativeSpaceTransactions>,
) {
    for command in messages.read().filter_map(|event| match event {
        Event::Command { command } => Some(command),
        _ => None,
    }) {
        let Command::MoveWindowToSpace {
            window_id,
            space_id,
            move_focus,
        } = command
        else {
            let intent = match command {
                Command::FocusSpace { space_id } => Some(NativeSpaceIntent::Focus {
                    space_id: *space_id,
                    animate: config.space_switch_animation(),
                }),
                Command::CreateSpace { display_id } => Some(NativeSpaceIntent::Create {
                    display_id: *display_id,
                }),
                Command::DeleteSpace { space_id } => Some(NativeSpaceIntent::Delete {
                    space_id: *space_id,
                }),
                _ => None,
            };
            if let Some(intent) = intent {
                if !config.space_control_enabled() {
                    warn!(?command, "Space control is disabled");
                } else if let Err(error) = window_manager.perform_native_space_intent(&intent) {
                    warn!(?command, %error, "Space capability unavailable");
                }
            }
            continue;
        };
        if !config.space_control_enabled() {
            warn!("Space control is disabled; enable experimental_space_control");
            continue;
        }
        let target_is_known = window_manager
            .present_displays()
            .into_iter()
            .flat_map(|(_, spaces)| spaces)
            .any(|candidate| candidate == *space_id);
        if !target_is_known || window_manager.workspace_is_fullscreen(*space_id) {
            warn!(space_id, "target is not a known user Space");
            continue;
        }
        if windows.find(*window_id).is_none() {
            warn!(window_id, "window is not tracked");
            continue;
        }
        let mut window_ids = window_manager.get_associated_windows(*window_id);
        if !window_ids.contains(window_id) {
            window_ids.push(*window_id);
        }
        window_ids.sort_unstable();
        window_ids.dedup();
        let intent = NativeSpaceIntent::MoveWindows {
            window_ids: window_ids.clone(),
            space_id: *space_id,
        };
        match window_manager.perform_native_space_intent(&intent) {
            Ok(()) => transactions.moves.push(PendingMove {
                window_ids,
                target_space_id: *space_id,
                follow_window_id: (*move_focus == MoveFocus::Follow).then_some(*window_id),
                submitted: Instant::now(),
            }),
            Err(error) => warn!(%error, "Space operation rejected"),
        }
    }
}

pub(crate) fn reconcile_native_space_transactions(
    window_manager: Res<WindowManager>,
    windows: Windows,
    config: Res<Config>,
    mut spaces: Query<(&mut LayoutStrip, Has<VisibleNativeSpaceMarker>)>,
    mut commands: Commands,
    mut transactions: bevy::ecs::system::ResMut<NativeSpaceTransactions>,
) {
    const MOVE_TIMEOUT: Duration = Duration::from_secs(2);
    const FOLLOW_TIMEOUT: Duration = Duration::from_secs(5);
    let mut new_follows = Vec::new();
    transactions.moves.retain(|pending| {
        let Ok(members) = window_manager.windows_in_workspace(pending.target_space_id) else {
            return pending.submitted.elapsed() < MOVE_TIMEOUT;
        };
        if pending.window_ids.iter().all(|id| members.contains(id)) {
            if !spaces
                .iter()
                .any(|(strip, _)| strip.id() == pending.target_space_id)
            {
                return pending.submitted.elapsed() < MOVE_TIMEOUT;
            }
            let moved_entities = pending
                .window_ids
                .iter()
                .filter_map(|window_id| windows.find(*window_id).map(|(_, entity)| entity))
                .collect::<Vec<_>>();
            for (mut strip, _) in &mut spaces {
                if strip.id() == pending.target_space_id {
                    strip.append_tab_group(&moved_entities);
                } else {
                    for entity in &moved_entities {
                        strip.remove(*entity);
                    }
                }
            }
            debug!(
                space_id = pending.target_space_id,
                windows = ?pending.window_ids,
                "window-to-Space operation reconciled"
            );
            if let Some(window_id) = pending.follow_window_id {
                let intent = NativeSpaceIntent::Focus {
                    space_id: pending.target_space_id,
                    animate: config.space_switch_animation(),
                };
                match window_manager.perform_native_space_intent(&intent) {
                    Ok(()) => new_follows.push(PendingFollow {
                        window_id,
                        target_space_id: pending.target_space_id,
                        submitted: Instant::now(),
                    }),
                    Err(error) => warn!(
                        window_id,
                        space_id = pending.target_space_id,
                        %error,
                        "unable to follow moved window to Space"
                    ),
                }
            }
            false
        } else if pending.submitted.elapsed() >= MOVE_TIMEOUT {
            warn!(
                space_id = pending.target_space_id,
                windows = ?pending.window_ids,
                "window-to-Space operation timed out without reconciliation"
            );
            false
        } else {
            true
        }
    });
    transactions.follows.extend(new_follows);
    transactions.follows.retain(|pending| {
        let target_visible = spaces
            .iter()
            .any(|(strip, visible)| strip.id() == pending.target_space_id && visible);
        if target_visible {
            if let Some((_, entity)) = windows.find(pending.window_id) {
                commands.focus_entity(entity, true);
            } else {
                warn!(
                    window_id = pending.window_id,
                    "moved window disappeared before Space follow completed"
                );
            }
            false
        } else if pending.submitted.elapsed() >= FOLLOW_TIMEOUT {
            warn!(
                window_id = pending.window_id,
                space_id = pending.target_space_id,
                "Space follow timed out before the target became visible"
            );
            false
        } else {
            true
        }
    });
}

type ObservedNativeSpaces<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LayoutStrip,
        &'static ChildOf,
        Option<&'static mut NativeSpace>,
        Has<VisibleNativeSpaceMarker>,
    ),
>;

/// Reconciles read-only Space topology and per-display visibility from
/// macOS. This system never changes Spaces or moves windows between them.
#[instrument(level = tracing::Level::DEBUG, skip_all)]
pub(crate) fn reconcile_native_spaces(
    mut messages: MessageReader<Event>,
    displays: Query<(&Display, Entity)>,
    mut spaces: ObservedNativeSpaces,
    window_manager: Res<WindowManager>,
    mut generation: Local<u64>,
    mut commands: Commands,
) {
    let triggers = messages
        .read()
        .filter_map(|event| match event {
            Event::SpaceChanged => Some("space-changed"),
            Event::SpaceCreated { .. } => Some("space-created"),
            Event::SpaceDestroyed { .. } => Some("space-destroyed"),
            Event::SystemWoke { .. } => Some("system-woke"),
            Event::DisplayAdded { .. } => Some("display-added"),
            Event::DisplayRemoved { .. } => Some("display-removed"),
            Event::DisplayMoved { .. } => Some("display-moved"),
            Event::DisplayResized { .. } => Some("display-resized"),
            Event::DisplayConfigured { .. } => Some("display-configured"),
            _ => None,
        })
        .collect::<Vec<_>>();
    if triggers.is_empty() {
        return;
    }
    *generation = generation.wrapping_add(1);

    let topology_by_display = window_manager
        .present_displays()
        .into_iter()
        .map(|(display, spaces)| (display.id(), spaces))
        .collect::<HashMap<_, _>>();

    for (display, display_entity) in &displays {
        let display_id = display.id();
        let Ok(visible_id) = window_manager.active_display_space(display_id) else {
            error!(display_id, "unable to read visible Space");
            continue;
        };
        let Some(topology) = topology_by_display.get(&display_id) else {
            warn!(display_id, "Space topology unavailable");
            continue;
        };

        for (entity, strip, child, native, visible) in &mut spaces {
            if child.parent() != display_entity {
                continue;
            }
            let Some(ordinal) = topology.iter().position(|id| *id == strip.id()) else {
                continue;
            };
            let observed = NativeSpace::new(
                strip.id(),
                ordinal,
                window_manager.workspace_is_fullscreen(strip.id()),
            );
            if let Some(mut native) = native {
                *native = observed;
            } else {
                commands.entity(entity).try_insert(observed);
            }
            let should_be_visible = strip.id() == visible_id;
            if should_be_visible && !visible {
                commands.entity(entity).try_insert(VisibleNativeSpaceMarker);
            } else if !should_be_visible && visible {
                commands
                    .entity(entity)
                    .try_remove::<VisibleNativeSpaceMarker>();
            }
        }

        debug!(
            generation = *generation,
            triggers = ?triggers,
            display_id,
            visible_space_id = visible_id,
            spaces = ?topology,
            "observed Space topology"
        );
        if tracing::enabled!(Level::DEBUG) {
            for space_id in topology {
                match window_manager.windows_in_workspace(*space_id) {
                    Ok(window_ids) => debug!(
                        generation = *generation,
                        display_id,
                        space_id,
                        visible = *space_id == visible_id,
                        fullscreen = window_manager.workspace_is_fullscreen(*space_id),
                        windows = ?window_ids,
                        "observed Space membership"
                    ),
                    Err(error) => warn!(
                        generation = *generation,
                        display_id,
                        space_id,
                        %error,
                        "Space membership unavailable"
                    ),
                }
            }
        }
    }
}

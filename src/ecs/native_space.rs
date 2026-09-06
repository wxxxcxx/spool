use std::collections::HashMap;
use std::time::Duration;

use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Local, Query, Res, SystemParam};
use bevy::time::Time;
use tracing::{debug, error, instrument, warn};

use crate::commands::{Action, MoveFocus};
use crate::config::Config;
use crate::ecs::display::FloatingLayer;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::Windows;
use crate::ecs::topology::NativeTopology;
use crate::ecs::workspace::{
    PendingSpaceDestruction, WindowSpaceReassignmentPending, freeze_window_for_space_reassignment,
};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, RefreshWindowSizes, SpawnCommandsExt,
};
use crate::events::Event;
use crate::manager::{Display, NativeSpaceIntent, WindowManager};
use crate::platform::{WinID, WindowIncarnation, WorkspaceId};
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

/// Retains a Space projection while its physical display is disconnected.
/// Only observed topology and membership, never elapsed time, end this state.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct DetachedSpace {
    pub(crate) source_display_id: u32,
}

/// The explicit move transaction owns reconciliation until confirmation or
/// timeout. The shared reassignment marker independently suspends geometry.
#[derive(Component)]
pub(crate) struct NativeMoveOwner;

#[derive(Debug)]
struct PendingMove {
    window_ids: Vec<i32>,
    members: Vec<MoveWindowIdentity>,
    target_space_id: WorkspaceId,
    follow: Option<MoveWindowIdentity>,
    layout: PendingMoveLayout,
    submitted: Duration,
}

#[derive(Clone, Copy, Debug)]
struct MoveWindowIdentity {
    window_id: WinID,
    entity: Entity,
    incarnation: WindowIncarnation,
}

impl MoveWindowIdentity {
    fn is_current(self, windows: &Windows) -> bool {
        windows
            .find_parent_incarnation_any(self.window_id, self.incarnation)
            .is_some_and(|(_, entity, _)| entity == self.entity)
    }

    fn is_available(self, windows: &Windows) -> bool {
        windows
            .get_tracked(self.entity)
            .is_some_and(|(window, _, _)| {
                window.id() == self.window_id && window.incarnation() == self.incarnation
            })
    }
}

#[derive(Debug)]
enum PendingMoveLayout {
    AssociatedWindows,
    Column(Column),
}

#[derive(Debug)]
struct PendingFollow {
    member: MoveWindowIdentity,
    target_space_id: WorkspaceId,
    submitted: Duration,
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
        Event::ActionRequested {
            action: Action::FocusWindow { window_id },
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

#[derive(SystemParam)]
pub(crate) struct NativeSpaceCommandCtx<'w, 's> {
    windows: Windows<'w, 's>,
    spaces: Query<'w, 's, &'static LayoutStrip>,
    config: Res<'w, Config>,
    window_manager: Res<'w, WindowManager>,
    transactions: bevy::ecs::system::ResMut<'w, NativeSpaceTransactions>,
    time: Res<'w, Time>,
    commands: Commands<'w, 's>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_native_space_commands(
    mut messages: MessageReader<Event>,
    ctx: NativeSpaceCommandCtx,
) {
    let NativeSpaceCommandCtx {
        windows,
        spaces,
        config,
        window_manager,
        mut transactions,
        time,
        mut commands,
    } = ctx;
    for action in messages.read().filter_map(|event| match event {
        Event::ActionRequested { action } => Some(action),
        _ => None,
    }) {
        let move_request = match action {
            Action::MoveWindowToSpace {
                window_id,
                space_id,
                move_focus,
            } => Some((*window_id, *space_id, *move_focus, None)),
            Action::MoveColumnToSpace {
                window_id,
                space_id,
                move_focus,
            } => {
                let Some((_, entity)) = windows.find(*window_id) else {
                    warn!(window_id, "window is not tracked");
                    continue;
                };
                let Some((_, _, state)) = windows.get_tracked(entity) else {
                    continue;
                };
                if state.is_floating() {
                    warn!(
                        window_id,
                        "a floating window does not belong to a tiled column"
                    );
                    continue;
                }
                let Some(column) = spaces
                    .iter()
                    .find_map(|strip| strip.column_containing(entity))
                else {
                    warn!(window_id, "window is not in a layout column");
                    continue;
                };
                Some((*window_id, *space_id, *move_focus, Some(column)))
            }
            _ => None,
        };
        let Some((window_id, space_id, move_focus, column)) = move_request else {
            let intent = match action {
                Action::FocusSpace { space_id } => Some(NativeSpaceIntent::Focus {
                    space_id: *space_id,
                    animate: config.space_switch_animation(),
                }),
                Action::CreateSpace { display_id } => Some(NativeSpaceIntent::Create {
                    display_id: *display_id,
                }),
                Action::DeleteSpace { space_id } => Some(NativeSpaceIntent::Delete {
                    space_id: *space_id,
                }),
                _ => None,
            };
            if let Some(intent) = intent {
                if config.space_control_enabled() {
                    if let Err(error) = window_manager.perform_native_space_intent(&intent) {
                        warn!(?action, %error, "Space capability unavailable");
                    }
                } else {
                    warn!(?action, "Space control is disabled");
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
            .any(|candidate| candidate == space_id);
        if !target_is_known || window_manager.workspace_is_fullscreen(space_id) {
            warn!(space_id, "target is not a known user Space");
            continue;
        }
        if windows.find(window_id).is_none() {
            warn!(window_id, "window is not tracked");
            continue;
        }
        let members = column.as_ref().map_or_else(
            || {
                windows
                    .find(window_id)
                    .map(|(_, entity)| vec![entity])
                    .unwrap_or_default()
            },
            |column| column.window_iter().collect(),
        );
        let mut window_ids = Vec::new();
        if members
            .iter()
            .any(|member| windows.get_tracked(*member).is_none())
        {
            warn!(window_id, "column has unavailable members");
            continue;
        }
        for member in members {
            let Some((window, _, _)) = windows.get_tracked(member) else {
                continue;
            };
            let member_id = window.id();
            window_ids.extend(window_manager.get_associated_windows(member_id));
            window_ids.push(member_id);
        }
        window_ids.sort_unstable();
        window_ids.dedup();
        if transactions
            .moves
            .iter()
            .any(|pending| pending.window_ids.iter().any(|id| window_ids.contains(id)))
        {
            warn!(window_id, "window already has a pending native move");
            continue;
        }
        let members = window_ids
            .iter()
            .filter_map(|id| {
                windows
                    .find(*id)
                    .map(|(window, entity)| MoveWindowIdentity {
                        window_id: *id,
                        entity,
                        incarnation: window.incarnation(),
                    })
            })
            .collect::<Vec<_>>();
        let follow = members
            .iter()
            .find(|member| member.window_id == window_id)
            .copied()
            .filter(|_| move_focus == MoveFocus::Follow);
        let intent = NativeSpaceIntent::MoveWindows {
            window_ids: window_ids.clone(),
            space_id,
        };
        match window_manager.perform_native_space_intent(&intent) {
            Ok(()) => {
                for member in &members {
                    let entity = member.entity;
                    let index = spaces
                        .iter()
                        .find_map(|strip| strip.index_of(entity).ok())
                        .unwrap_or_default();
                    freeze_window_for_space_reassignment(entity, index, &mut commands);
                    commands.entity(entity).insert(NativeMoveOwner);
                }
                transactions.moves.push(PendingMove {
                    window_ids,
                    members,
                    target_space_id: space_id,
                    follow,
                    layout: column.map_or(
                        PendingMoveLayout::AssociatedWindows,
                        PendingMoveLayout::Column,
                    ),
                    submitted: time.elapsed(),
                });
            }
            Err(error) => warn!(%error, "Space operation rejected"),
        }
    }
}

#[derive(SystemParam)]
pub(crate) struct NativeSpaceReconciliationCtx<'w, 's> {
    window_manager: Res<'w, WindowManager>,
    windows: Windows<'w, 's>,
    config: Res<'w, Config>,
    spaces: Query<
        'w,
        's,
        (
            &'static mut LayoutStrip,
            Has<VisibleNativeSpaceMarker>,
            Has<PendingSpaceDestruction>,
        ),
    >,
    commands: Commands<'w, 's>,
    transactions: bevy::ecs::system::ResMut<'w, NativeSpaceTransactions>,
    time: Res<'w, Time>,
    topology: Res<'w, NativeTopology>,
}

// A target-only observation cannot distinguish a completed move from an
// overlapping source/target snapshot during a native Space transition.
fn unique_memberships(
    topology: &NativeTopology,
    manager: &WindowManager,
) -> Option<HashMap<WinID, Option<WorkspaceId>>> {
    if !topology.is_complete() {
        return None;
    }
    let mut spaces = topology
        .known_displays()
        .flat_map(|(_, spaces)| spaces.iter().copied())
        .collect::<Vec<_>>();
    spaces.sort_unstable();
    spaces.dedup();
    let mut memberships = HashMap::new();
    for space in spaces {
        for id in manager.windows_in_workspace(space).ok()? {
            memberships
                .entry(id)
                .and_modify(|previous| {
                    if *previous != Some(space) {
                        *previous = None;
                    }
                })
                .or_insert(Some(space));
        }
    }
    Some(memberships)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn reconcile_native_space_transactions(ctx: NativeSpaceReconciliationCtx) {
    const MOVE_TIMEOUT: Duration = Duration::from_secs(2);
    const FOLLOW_TIMEOUT: Duration = Duration::from_secs(5);
    let NativeSpaceReconciliationCtx {
        window_manager,
        windows,
        config,
        mut spaces,
        mut commands,
        mut transactions,
        time,
        topology,
    } = ctx;
    let mut new_follows = Vec::new();
    let mut completed = Vec::new();
    let mut expired = Vec::new();
    if transactions.moves.is_empty() && transactions.follows.is_empty() {
        return;
    }
    let memberships = unique_memberships(&topology, &window_manager);
    let belongs_to = |id, space| {
        memberships
            .as_ref()
            .is_some_and(|members| members.get(&id) == Some(&Some(space)))
    };
    transactions.moves.retain(|pending| {
        let timed_out = time.elapsed().saturating_sub(pending.submitted) >= MOVE_TIMEOUT;
        if pending
            .members
            .iter()
            .any(|member| !member.is_current(&windows))
        {
            expired.extend(pending.members.iter().map(|member| member.entity));
            warn!(
                space_id = pending.target_space_id,
                "native move member identity retired"
            );
            return false;
        }
        let target_ready = !topology.is_fullscreen(pending.target_space_id)
            && spaces
                .iter()
                .any(|(strip, _, retiring)| strip.id() == pending.target_space_id && !retiring);
        if target_ready
            && pending
                .members
                .iter()
                .all(|member| member.is_available(&windows))
            && pending
                .window_ids
                .iter()
                .all(|id| belongs_to(*id, pending.target_space_id))
        {
            let moved_entities = pending
                .members
                .iter()
                .map(|member| member.entity)
                .collect::<Vec<_>>();
            let mut target_anchor = None;
            for (mut strip, _, _) in &mut spaces {
                if strip.id() == pending.target_space_id {
                    match &pending.layout {
                        PendingMoveLayout::AssociatedWindows => {
                            strip.append_tab_group(&moved_entities);
                            target_anchor = moved_entities.first().copied();
                        }
                        PendingMoveLayout::Column(column) => {
                            strip.append_column(column.clone());
                            target_anchor = column.top();
                        }
                    }
                } else {
                    for entity in &moved_entities {
                        strip.remove(*entity);
                    }
                }
            }
            if let Some(anchor) = target_anchor {
                commands.reshuffle_around(anchor);
            }
            completed.extend_from_slice(&moved_entities);
            debug!(
                space_id = pending.target_space_id,
                windows = ?pending.window_ids,
                "window-to-Space operation reconciled"
            );
            if let Some(member) = pending.follow {
                let intent = NativeSpaceIntent::Focus {
                    space_id: pending.target_space_id,
                    animate: config.space_switch_animation(),
                };
                match window_manager.perform_native_space_intent(&intent) {
                    Ok(()) => {
                        new_follows.push(PendingFollow {
                            member,
                            target_space_id: pending.target_space_id,
                            submitted: time.elapsed(),
                        });
                    }
                    Err(error) => warn!(
                        window_id = member.window_id,
                        space_id = pending.target_space_id,
                        %error,
                        "unable to follow moved window to Space"
                    ),
                }
            }
            false
        } else if timed_out {
            expired.extend(pending.members.iter().map(|member| member.entity));
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
    for entity in completed {
        if let Ok(mut entity) = commands.get_entity(entity) {
            entity.try_remove::<(NativeMoveOwner, WindowSpaceReassignmentPending)>();
        }
    }
    // A timeout releases ownership, not the geometry barrier. The existing
    // membership audit resolves the actual destination before resuming writes.
    for entity in expired {
        if let Ok(mut entity) = commands.get_entity(entity) {
            entity.try_remove::<NativeMoveOwner>();
        }
    }
    transactions.follows.extend(new_follows);
    transactions.follows.retain(|pending| {
        if !pending.member.is_current(&windows)
            || time.elapsed().saturating_sub(pending.submitted) >= FOLLOW_TIMEOUT
        {
            return false;
        }
        let target_visible = spaces.iter().any(|(strip, visible, retiring)| {
            strip.id() == pending.target_space_id && visible && !retiring
        });
        if target_visible
            && pending.member.is_available(&windows)
            && belongs_to(pending.member.window_id, pending.target_space_id)
        {
            commands.focus_entity(pending.member.entity, true);
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
        Option<&'static ChildOf>,
        Option<&'static mut NativeSpace>,
        Has<VisibleNativeSpaceMarker>,
        Has<ActiveWorkspaceMarker>,
        Has<PendingSpaceDestruction>,
        Has<FloatingLayer>,
    ),
>;

#[derive(SystemParam)]
pub(crate) struct NativeSpaceObservationCtx<'w, 's> {
    displays: Query<'w, 's, (&'static Display, Entity, Has<ActiveDisplayMarker>)>,
    spaces: ObservedNativeSpaces<'w, 's>,
    topology: Res<'w, super::topology::NativeTopology>,
    generation: Local<'s, u64>,
    commands: Commands<'w, 's>,
}

struct DisplaySpaceProjection<'a> {
    display: &'a Display,
    display_entity: Entity,
    display_active: bool,
    topology: &'a [WorkspaceId],
    visible_id: WorkspaceId,
}

fn reconcile_display_space_projections(
    projection: DisplaySpaceProjection<'_>,
    spaces: &mut ObservedNativeSpaces,
    observation: &super::topology::NativeTopology,
    commands: &mut Commands,
) {
    let DisplaySpaceProjection {
        display,
        display_entity,
        display_active,
        topology,
        visible_id,
    } = projection;
    for (ordinal, space_id) in topology.iter().copied().enumerate() {
        let observed = NativeSpace::new(space_id, ordinal, observation.is_fullscreen(space_id));
        let should_be_visible = space_id == visible_id;
        let should_be_active = display_active && should_be_visible;
        let mut found = false;
        let mut tombstoned = false;

        for (entity, strip, child, native, visible, active, pending, has_layer) in spaces.iter_mut()
        {
            if strip.id() != space_id {
                continue;
            }
            if pending {
                tombstoned = true;
                continue;
            }
            if found {
                continue;
            }
            found = true;

            if let Some(mut native) = native {
                if *native != observed {
                    *native = observed;
                }
            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(observed);
            }
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                if !has_layer {
                    entity_commands.try_insert(FloatingLayer::default());
                }
                if child.is_none_or(|child| child.parent() != display_entity) {
                    entity_commands
                        .try_remove::<DetachedSpace>()
                        .try_insert((ChildOf(display_entity), RefreshWindowSizes::default()));
                }
                if should_be_visible && !visible {
                    entity_commands.try_insert(VisibleNativeSpaceMarker);
                } else if !should_be_visible && visible {
                    entity_commands.try_remove::<VisibleNativeSpaceMarker>();
                }
                if should_be_active && !active {
                    entity_commands.try_insert(ActiveWorkspaceMarker);
                } else if !should_be_active && active {
                    entity_commands.try_remove::<ActiveWorkspaceMarker>();
                }
            }
        }

        if !found && !tombstoned {
            let display_id = display.id();
            debug!(space_id, display_id, "projecting new Space");
            let mut spawned = commands.spawn_layout_strip(
                LayoutStrip::new(space_id),
                display.bounds().min,
                display_entity,
                should_be_active,
            );
            spawned.try_insert((observed, FloatingLayer::default()));
            if should_be_visible {
                spawned.try_insert(VisibleNativeSpaceMarker);
            }
        }
    }
}

/// Reconciles read-only Space topology and per-display visibility from
/// macOS. This system never changes Spaces or moves windows between them.
#[instrument(level = tracing::Level::DEBUG, skip_all)]
pub(crate) fn reconcile_native_spaces(ctx: NativeSpaceObservationCtx) {
    let NativeSpaceObservationCtx {
        displays,
        mut spaces,
        topology: observation,
        mut generation,
        mut commands,
    } = ctx;
    if *generation == observation.generation() {
        return;
    }
    *generation = observation.generation();

    let topology_by_display = observation
        .known_displays()
        .map(|(display, spaces)| (display.id(), spaces))
        .collect::<HashMap<_, _>>();

    for (display, display_entity, display_active) in &displays {
        let display_id = display.id();
        let Some(visible_id) = observation.visible_space(display_id) else {
            error!(display_id, "unable to read visible Space");
            continue;
        };
        let Some(topology) = topology_by_display.get(&display_id) else {
            warn!(display_id, "Space topology unavailable");
            continue;
        };
        reconcile_display_space_projections(
            DisplaySpaceProjection {
                display,
                display_entity,
                display_active,
                topology,
                visible_id,
            },
            &mut spaces,
            &observation,
            &mut commands,
        );

        debug!(
            generation = *generation,
            display_id,
            visible_space_id = visible_id,
            spaces = ?topology,
            "observed Space topology"
        );
    }
}

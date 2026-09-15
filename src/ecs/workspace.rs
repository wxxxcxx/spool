use bevy::app::{App, Last, Plugin, PreUpdate, Update};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut as _, Ref};
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::Add;
use bevy::ecs::message::MessageReader;
use bevy::ecs::observer::On;
use bevy::ecs::query::{Added, Has, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::schedule::common_conditions::{not, resource_exists};
use bevy::ecs::system::{Commands, Local, Populated, Query, Res, ResMut, Single, SystemParam};
use bevy::time::common_conditions::on_timer;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{Level, debug, instrument, warn};

use super::{ActiveDisplayMarker, SpawnWindowTrigger};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{self, VisibleNativeSpaceMarker};
use crate::ecs::params::{WindowCtx, Windows};
use crate::ecs::reconcile::WindowUnavailable;
use crate::ecs::topology::WindowMemberships;
use crate::ecs::{
    ActiveWorkspaceMarker, DockPosition, EnsureVisibleMarker, Floating, FullscreenDefaultsDeferred,
    Initializing, NativeFullscreenMarker, PreviousTiledStrip, RefreshWindowSizes, RepositionMarker,
    ReshuffleAroundMarker, ResizeMarker, SpawnCommandsExt, VerifyWindowPosition, WindowFrameMotion,
    WindowVisibility,
};
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{WinID, WorkspaceId};

pub struct WorkspaceEventsPlugin;

/// A native Space disappeared, but its windows have not all been observed on
/// their surviving Spaces yet. Keep the source strip intact until macOS's live
/// membership snapshot identifies every destination.
#[derive(Clone, Component, Copy, Debug)]
pub(crate) struct PendingSpaceDestruction {
    pub(crate) workspace_id: WorkspaceId,
    source_display_id: u32,
    was_active: bool,
    explicit: bool,
}

/// Freezes geometry while native Space membership is changing.
/// The `WindowServer` membership snapshot, rather than notification order, ends
/// this state.
#[derive(Component, Debug)]
pub(crate) struct WindowSpaceReassignmentPending {
    index: usize,
}

impl WindowSpaceReassignmentPending {
    pub(crate) fn source_index(&self) -> usize {
        self.index
    }
}

impl Plugin for WorkspaceEventsPlugin {
    fn build(&self, app: &mut App) {
        const REFRESH_WINDOW_CHECK_FREQ_MS: u64 = 1000;

        app.init_resource::<native_space::NativeSpaceTransactions>();
        app.add_systems(
            PreUpdate,
            (
                super::topology::refresh_topology,
                invalidate_missing_workspaces,
            )
                .chain()
                .after(crate::commands::dispatch_actions),
        );
        app.add_systems(
            Update,
            (
                native_space::reconcile_native_spaces
                    .after(super::display::reconcile_displays)
                    .before(reconcile_fullscreen_spaces)
                    .before(super::systems::finish_setup),
                reconcile_fullscreen_spaces
                    .after(super::reconcile::reconcile_windows)
                    .before(detect_moved_windows),
                detect_moved_windows.run_if(not(resource_exists::<Initializing>)),
                reconcile_workspace_refresh.run_if(on_timer(Duration::from_millis(
                    REFRESH_WINDOW_CHECK_FREQ_MS,
                ))),
            ),
        );
        app.add_systems(
            Last,
            (
                reconcile_destroyed_workspace_membership,
                native_space::reconcile_native_space_transactions,
                // Declared membership is reconciled after the layout has followed
                // the facts, so a repair names the Space the window is actually
                // in rather than one the transaction is about to leave.
                native_space::reconcile_declared_space,
            )
                .chain(),
        );
        app.add_observer(cleanup_active_workspace_marker);
        app.add_observer(restore_focus_on_space_activation);
    }
}

type FullscreenWindows<'w, 's> = (
    Windows<'w, 's>,
    Query<'w, 's, (), Added<Window>>,
    Query<'w, 's, Entity, With<FullscreenDefaultsDeferred>>,
);

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn reconcile_fullscreen_spaces(
    mut messages: MessageReader<Event>,
    (windows, discovered, deferred): FullscreenWindows,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<NativeFullscreenMarker>)>,
    topology: Res<super::topology::NativeTopology>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
    mut generation: Local<u64>,
) {
    let signaled = messages
        .read()
        .any(|event| matches!(event, Event::SpaceChanged));
    if !signaled && discovered.is_empty() && *generation == topology.generation() {
        return;
    }
    *generation = topology.generation();
    let detached = deferred
        .iter()
        .filter(|entity| !workspaces.iter().any(|(strip, ..)| strip.contains(*entity)))
        .collect::<Vec<_>>();
    // Unknown capabilities can keep a startup fullscreen window outside every
    // strip. Such a window has no source-strip destruction to resume its defaults.
    if !detached.is_empty()
        && let Ok(memberships) = topology.observe_memberships(&window_manager)
    {
        for entity in detached {
            if let Some(window) = windows.get(entity)
                && memberships
                    .unique_space(window.id())
                    .is_some_and(|space| !topology.is_fullscreen(space))
                && matches!(window.try_is_full_screen(), Ok(false))
            {
                commands
                    .entity(entity)
                    .remove::<FullscreenDefaultsDeferred>();
            }
        }
    }
    let targets = workspaces
        .iter()
        .filter_map(|(strip, entity, marked)| {
            (topology.is_fullscreen(strip.id()) && !marked).then_some((entity, strip.id()))
        })
        .collect::<Vec<_>>();
    for (target, workspace_id) in targets {
        let Ok(members) = window_manager.windows_in_workspace(workspace_id) else {
            continue;
        };
        let mut candidates = members
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|id| windows.find_tiled(id).map(|(_, entity)| entity));
        let Some(window) = candidates.next() else {
            continue;
        };
        if candidates.next().is_some() {
            warn!(
                workspace_id,
                "fullscreen membership has multiple tracked windows; deferring migration"
            );
            continue;
        }
        let source = workspaces.iter().find_map(|(strip, entity, _)| {
            (entity != target)
                .then(|| {
                    strip
                        .index_of(window)
                        .ok()
                        .map(|index| (entity, strip.id(), index))
                })
                .flatten()
        });
        // AX may first expose an inactive fullscreen window after startup.
        // It needs a native strip even when there is no previous tiled slot.
        if let Some((source, source_id, index)) = source {
            if let Ok((mut strip, _, _)) = workspaces.get_mut(source) {
                strip.remove(window);
            }
            commands.entity(target).insert(NativeFullscreenMarker {
                layout_strip: source,
                workspace_id: source_id,
                index,
            });
        }
        if let Ok((mut strip, _, _)) = workspaces.get_mut(target)
            && (!strip.is_fullscreen() || !strip.contains(window))
        {
            *strip = LayoutStrip::fullscreen(workspace_id, window);
        }
        commands.entity(window).remove::<(
            RepositionMarker,
            ResizeMarker,
            WindowFrameMotion,
            VerifyWindowPosition,
        )>();
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn detect_moved_windows(
    activated_workspace: Single<
        (Entity, Option<&native_space::NativeSpace>),
        Added<ActiveWorkspaceMarker>,
    >,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<NativeFullscreenMarker>)>,
    pending_windows: Query<(), With<WindowSpaceReassignmentPending>>,
    apps: Query<&mut Application>,
    window_manager: Res<WindowManager>,
    mut ignored_windows: Local<HashSet<WinID>>,
    mut ctx: WindowCtx,
) {
    let (activated_workspace, native) = *activated_workspace;
    if native.is_some_and(|space| space.kind == native_space::SpaceKind::Fullscreen) {
        // Fullscreen ownership and source-index capture belong to its reconciler,
        // including while its membership read is temporarily unavailable.
        return;
    }
    let Ok(workspace_id) = workspaces
        .get(activated_workspace)
        .map(|strip| strip.0.id())
    else {
        return;
    };
    debug!("workspace {workspace_id}");

    let strips = workspaces
        .iter()
        .filter_map(|strip| (strip.0.id() == workspace_id).then_some(strip.0))
        .collect::<Vec<_>>();
    let find_window = |window_id| {
        ctx.windows
            .find_tiled(window_id)
            .map(|(_, entity)| entity)
            .filter(|entity| pending_windows.get(*entity).is_err())
    };
    let Ok((moved_windows, mut unresolved)) =
        windows_not_in_strips(workspace_id, find_window, &strips, &window_manager).inspect_err(
            |err| {
                warn!("unable to get windows in the current workspace: {err}");
            },
        )
    else {
        return;
    };
    // Skip windows that Spool deliberately does not track.
    unresolved.retain(|window_id| {
        !ignored_windows.contains(window_id) && ctx.windows.find(*window_id).is_none()
    });

    if !unresolved.is_empty() {
        let unresolved_ids = unresolved.iter().copied().collect::<HashSet<_>>();
        // Retry unresolved window IDs: during startup bruteforce, windows on
        // inactive workspaces may have stale AX attributes (e.g. AXGroup instead
        // of AXWindow).  Now that this workspace is active, re-query each app's
        // window list — the AX data should be correct.
        let retry_windows = apps
            .into_iter()
            .flat_map(|app| {
                app.window_list(&ctx.config)
                    .into_iter()
                    .filter(|window| unresolved_ids.contains(&window.id()))
            })
            .collect::<Vec<_>>();
        if retry_windows.is_empty() {
            for id in unresolved_ids {
                ignored_windows.insert(id);
            }
        } else {
            debug!(
                "retrying unresolved windows: {}",
                retry_windows
                    .iter()
                    .map(|window| format!("{}", window.id()))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            ctx.commands.trigger(SpawnWindowTrigger::new(retry_windows));
        }
    }

    for entity in moved_windows {
        if workspaces
            .iter()
            .any(|(strip, _, fullscreen)| fullscreen && strip.contains(entity))
        {
            // Do not relocate fullscreen windows, this will happen
            // during the destructino of their workspace.
            continue;
        }

        debug!("Window {entity} moved to workspace {workspace_id}.");
        let moving_entities = workspaces
            .iter()
            .find_map(|(strip, _, _)| strip.tab_group(entity))
            .unwrap_or_else(|| vec![entity]);
        for (mut strip, strip_entity, _) in &mut workspaces {
            if strip_entity == activated_workspace {
                strip.append_tab_group(&moving_entities);
            } else {
                for moving_entity in &moving_entities {
                    strip.remove(*moving_entity);
                }
            }
        }
    }
}

type InvalidatedWorkspaces<'w, 's> = Populated<
    'w,
    's,
    (
        &'static LayoutStrip,
        Entity,
        Option<&'static ChildOf>,
        Option<&'static PendingSpaceDestruction>,
        Option<&'static NativeFullscreenMarker>,
        Has<ActiveWorkspaceMarker>,
        Option<&'static native_space::DetachedSpace>,
    ),
>;

type DetachedPreviousWindows<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static PreviousTiledStrip,
        Has<WindowSpaceReassignmentPending>,
    ),
    With<Window>,
>;

#[derive(SystemParam)]
struct MissingWorkspaceCtx<'w, 's> {
    workspaces: InvalidatedWorkspaces<'w, 's>,
    detached_windows: DetachedPreviousWindows<'w, 's>,
    displays: Query<'w, 's, &'static Display>,
    topology: Res<'w, super::topology::NativeTopology>,
    focus: ResMut<'w, FocusCoordinator>,
    commands: Commands<'w, 's>,
}

pub(crate) fn freeze_window_for_space_reassignment(
    window: Entity,
    index: usize,
    commands: &mut Commands,
) {
    if let Ok(mut entity_commands) = commands.get_entity(window) {
        entity_commands
            .try_insert(WindowSpaceReassignmentPending { index })
            .try_remove::<(
                RepositionMarker,
                ResizeMarker,
                WindowFrameMotion,
                super::window_frame::WindowFrameCorrection,
                VerifyWindowPosition,
                ReshuffleAroundMarker,
                EnsureVisibleMarker,
            )>();
    }
}

fn freeze_workspace_windows(
    strip: &LayoutStrip,
    fullscreen: Option<&NativeFullscreenMarker>,
    detached_windows: &DetachedPreviousWindows,
    attached_windows: &HashSet<Entity>,
    commands: &mut Commands,
) {
    for window in strip.all_windows() {
        let index = fullscreen
            .filter(|_| strip.first().ok().and_then(|column| column.top()) == Some(window))
            .map_or_else(
                || strip.index_of(window).unwrap_or(strip.len()),
                |marker| marker.index,
            );
        freeze_window_for_space_reassignment(window, index, commands);
    }
    for (window, previous, pending) in detached_windows {
        if !pending && previous.workspace_id == strip.id() && !attached_windows.contains(&window) {
            freeze_window_for_space_reassignment(window, previous.index, commands);
        }
    }
}

/// Treats native Space notifications as invalidation hints. Explicit destroy
/// events are applied immediately; every topology event also detects a missed
/// destroy by comparing the strip IDs with the complete topology for its
/// display.
#[instrument(level = Level::DEBUG, skip_all)]
fn invalidate_missing_workspaces(
    mut messages: MessageReader<Event>,
    mut generation: Local<u64>,
    ctx: MissingWorkspaceCtx,
) {
    let MissingWorkspaceCtx {
        workspaces,
        detached_windows,
        displays,
        topology,
        mut focus,
        mut commands,
    } = ctx;
    let mut destroyed = HashSet::new();
    for event in messages.read() {
        if let Event::SpaceDestroyed { space_id } = event {
            destroyed.insert(*space_id);
        }
    }
    if destroyed.is_empty() && *generation == topology.generation() {
        return;
    }
    *generation = topology.generation();
    let complete = topology.is_complete();
    let topology = topology.known_displays().collect::<Vec<_>>();
    let present_spaces = topology
        .iter()
        .flat_map(|(_, spaces)| spaces.iter().copied())
        .collect::<HashSet<_>>();
    let observed_displays = topology
        .iter()
        .map(|(display, _)| display.id())
        .collect::<HashSet<_>>();
    let attached_windows = workspaces
        .iter()
        .flat_map(|(strip, ..)| strip.all_windows())
        .collect::<HashSet<_>>();

    for (strip, entity, child, pending, fullscreen, active, detached) in &workspaces {
        let source_display_id = pending
            .map(|pending| pending.source_display_id)
            .or_else(|| detached.map(|detached| detached.source_display_id))
            .or_else(|| child.and_then(|child| displays.get(child.parent()).ok().map(Display::id)));
        let display_topology_available =
            source_display_id.is_some_and(|id| observed_displays.contains(&id));
        let missing = destroyed.contains(&strip.id())
            || ((display_topology_available || complete) && !present_spaces.contains(&strip.id()));
        if !missing {
            if pending.is_some_and(|pending| !pending.explicit)
                && present_spaces.contains(&strip.id())
            {
                // Space presence cannot confirm where its former windows went.
                // Their barriers remain owned by membership reconciliation.
                commands.entity(entity).remove::<PendingSpaceDestruction>();
            }
            continue;
        }
        let Some(source_display_id) = source_display_id else {
            warn!(
                workspace_id = strip.id(),
                ?entity,
                "cannot reconcile destroyed Space without its source display"
            );
            continue;
        };

        focus.forget_workspace(strip.id());
        let was_active = active || pending.is_some_and(|pending| pending.was_active);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            debug!(
                workspace_id = strip.id(),
                ?entity,
                "Space destruction pending"
            );
            entity_commands
                .try_remove::<(ActiveWorkspaceMarker, VisibleNativeSpaceMarker)>()
                .try_insert(PendingSpaceDestruction {
                    workspace_id: strip.id(),
                    source_display_id,
                    was_active,
                    explicit: destroyed.contains(&strip.id())
                        || pending.is_some_and(|pending| pending.explicit),
                });
        }
        freeze_workspace_windows(
            strip,
            fullscreen,
            &detached_windows,
            &attached_windows,
            &mut commands,
        );
    }
}

#[derive(Clone, Copy)]
struct SurvivingWorkspace {
    entity: Entity,
    workspace_id: WorkspaceId,
}

struct PendingWorkspaceSnapshot {
    entity: Entity,
    workspace_id: WorkspaceId,
    source_display_id: u32,
    windows: Vec<Entity>,
    fullscreen: Option<NativeFullscreenMarker>,
    was_active: bool,
    changed: bool,
}

struct LiveSpaceSnapshot {
    surviving: Vec<SurvivingWorkspace>,
    memberships: Option<WindowMemberships>,
    active_target: Option<SurvivingWorkspace>,
    observed_display_ids: HashSet<u32>,
    topology_by_display: HashMap<u32, HashSet<WorkspaceId>>,
}

#[derive(Default)]
struct SpaceReconciliationBackoff {
    attempts: u8,
    skip_frames: u8,
}

type DestructionWorkspaces<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut LayoutStrip,
        Entity,
        Option<&'static NativeFullscreenMarker>,
        Option<Ref<'static, PendingSpaceDestruction>>,
        Has<ActiveWorkspaceMarker>,
        Has<VisibleNativeSpaceMarker>,
    ),
>;

fn pending_workspace_snapshot(workspaces: &DestructionWorkspaces) -> Vec<PendingWorkspaceSnapshot> {
    workspaces
        .iter()
        .filter_map(|(strip, entity, fullscreen, pending, _, _)| {
            pending.map(|pending| PendingWorkspaceSnapshot {
                entity,
                workspace_id: pending.workspace_id,
                source_display_id: pending.source_display_id,
                windows: strip.all_windows(),
                fullscreen: fullscreen.cloned(),
                was_active: pending.was_active,
                changed: pending.is_changed(),
            })
        })
        .collect()
}

fn live_space_snapshot(
    workspaces: &DestructionWorkspaces,
    window_manager: &WindowManager,
    active_workspace_id: Option<WorkspaceId>,
    observation: &super::topology::NativeTopology,
) -> LiveSpaceSnapshot {
    let destroyed_spaces = workspaces
        .iter()
        .filter_map(|(_, _, _, pending, _, _)| pending.map(|pending| pending.workspace_id))
        .collect::<HashSet<_>>();
    let topology = observation.known_displays().collect::<Vec<_>>();
    let observed_display_ids = topology
        .iter()
        .map(|(display, _)| display.id())
        .collect::<HashSet<_>>();
    let topology_by_display = topology
        .iter()
        .map(|(display, spaces)| (display.id(), spaces.iter().copied().collect()))
        .collect::<HashMap<_, _>>();
    let present_spaces = topology
        .into_iter()
        .flat_map(|(_, spaces)| spaces)
        .copied()
        .collect::<HashSet<_>>();
    let mut candidates = workspaces
        .iter()
        .filter_map(|(strip, entity, _, pending, active, visible)| {
            (pending.is_none()
                && !destroyed_spaces.contains(&strip.id())
                && present_spaces.contains(&strip.id())
                && !observation.is_fullscreen(strip.id()))
            .then_some((
                SurvivingWorkspace {
                    entity,
                    workspace_id: strip.id(),
                },
                active || active_workspace_id == Some(strip.id()),
                visible,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(workspace, active, visible)| {
        (
            workspace.workspace_id,
            !*active,
            !*visible,
            workspace.entity,
        )
    });
    candidates.dedup_by_key(|(workspace, _, _)| workspace.workspace_id);
    let surviving = candidates
        .into_iter()
        .map(|(workspace, _, _)| workspace)
        .collect::<Vec<_>>();

    let memberships = observation
        .observe_memberships(window_manager)
        .inspect_err(|error| debug!(%error, "Space membership snapshot incomplete"))
        .ok();
    let active_target = active_workspace_id.and_then(|workspace_id| {
        surviving
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
            .copied()
    });
    LiveSpaceSnapshot {
        surviving,
        memberships,
        active_target,
        observed_display_ids,
        topology_by_display,
    }
}

type ReassignmentWindows<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Window,
        Has<Floating>,
        Option<&'static WindowVisibility>,
        &'static WindowSpaceReassignmentPending,
    ),
    (
        Without<WindowUnavailable>,
        Without<native_space::NativeMoveOwner>,
    ),
>;

fn unique_membership(snapshot: &LiveSpaceSnapshot, window_id: WinID) -> Option<WorkspaceId> {
    snapshot.memberships.as_ref()?.unique_space(window_id)
}

fn target_workspace(
    snapshot: &LiveSpaceSnapshot,
    workspace_id: WorkspaceId,
) -> Option<SurvivingWorkspace> {
    snapshot
        .surviving
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .copied()
}

fn remove_windows_from_strips(entities: &[Entity], workspaces: &mut DestructionWorkspaces) {
    for (mut strip, ..) in workspaces.iter_mut() {
        for entity in entities {
            strip.remove(*entity);
        }
    }
}

fn move_fullscreen_window(
    source: &PendingWorkspaceSnapshot,
    entity: Entity,
    target: SurvivingWorkspace,
    workspaces: &mut DestructionWorkspaces,
) -> bool {
    if !workspaces
        .get(source.entity)
        .is_ok_and(|(strip, ..)| strip.contains(entity))
    {
        return false;
    }
    remove_windows_from_strips(&[entity], workspaces);
    let Ok((mut target_strip, ..)) = workspaces.get_mut(target.entity) else {
        return false;
    };
    if let Some(index) = source
        .fullscreen
        .as_ref()
        .and_then(|marker| (marker.workspace_id == target.workspace_id).then_some(marker.index))
    {
        target_strip.insert_at(index, entity);
    } else {
        target_strip.append(entity);
    }
    true
}

fn move_layout_group(
    source: Entity,
    target: SurvivingWorkspace,
    entities: &[Entity],
    workspaces: &mut DestructionWorkspaces,
) -> Vec<Entity> {
    let selected = entities
        .iter()
        .copied()
        .filter(|entity| {
            workspaces
                .get(source)
                .is_ok_and(|(strip, ..)| strip.contains(*entity))
        })
        .collect::<HashSet<_>>();
    if selected.is_empty() {
        return Vec::new();
    }
    let mut extracted = {
        let Ok((mut source_strip, ..)) = workspaces.get_mut(source) else {
            return Vec::new();
        };
        source_strip.take_windows_preserving_layout(&selected)
    };

    let moved = extracted.all_windows();
    remove_windows_from_strips(&moved, workspaces);
    let Ok((mut target_strip, ..)) = workspaces.get_mut(target.entity) else {
        return Vec::new();
    };
    target_strip.append_strip(&mut extracted);
    moved
}

fn release_reassignment_barriers(windows: &[Entity], commands: &mut Commands) {
    for entity in windows {
        if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            entity_commands
                .try_remove::<(WindowSpaceReassignmentPending, FullscreenDefaultsDeferred)>();
        }
    }
}

fn finish_rehomed_windows(windows: &[Entity], commands: &mut Commands) {
    release_reassignment_barriers(windows, commands);
    if let Some(entity) = windows.first() {
        commands.reshuffle_around(*entity);
    }
}

fn rehome_pending_workspace(
    source: &PendingWorkspaceSnapshot,
    snapshot: &LiveSpaceSnapshot,
    workspaces: &mut DestructionWorkspaces,
    windows: &ReassignmentWindows,
    commands: &mut Commands,
) {
    let mut tiled_by_target: HashMap<WorkspaceId, Vec<Entity>> = HashMap::new();
    let mut floating = Vec::new();
    let mut hidden = Vec::new();
    for entity in &source.windows {
        if !workspaces
            .get(source.entity)
            .is_ok_and(|(strip, ..)| strip.contains(*entity))
        {
            continue;
        }
        let Ok((_, window, is_floating, visibility, pending)) = windows.get(*entity) else {
            continue;
        };
        let Some(workspace_id) = unique_membership(snapshot, window.id()) else {
            continue;
        };
        if is_floating {
            floating.push(*entity);
        } else if visibility.is_some() {
            hidden.push((*entity, workspace_id, pending.index));
        } else {
            tiled_by_target
                .entry(workspace_id)
                .or_default()
                .push(*entity);
        }
    }

    remove_windows_from_strips(&floating, workspaces);
    finish_rehomed_windows(&floating, commands);
    for (entity, workspace_id, index) in hidden {
        remove_windows_from_strips(&[entity], workspaces);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands
                .try_insert(PreviousTiledStrip {
                    workspace_id,
                    index,
                })
                .try_remove::<WindowSpaceReassignmentPending>();
        }
    }

    let fullscreen_window = source
        .fullscreen
        .as_ref()
        .and_then(|_| source.windows.first());
    for (workspace_id, mut entities) in tiled_by_target {
        let Some(target) = target_workspace(snapshot, workspace_id) else {
            continue;
        };
        if let Some(fullscreen_window) = fullscreen_window
            && let Some(index) = entities
                .iter()
                .position(|entity| entity == fullscreen_window)
        {
            let entity = entities.remove(index);
            if move_fullscreen_window(source, entity, target, workspaces) {
                finish_rehomed_windows(&[entity], commands);
            }
        }
        let moved = move_layout_group(source.entity, target, &entities, workspaces);
        finish_rehomed_windows(&moved, commands);
        if !moved.is_empty() {
            debug!(
                source = source.workspace_id,
                target = workspace_id,
                windows = ?moved,
                "reconciled windows from destroyed Space"
            );
        }
    }
}

fn rehome_detached_pending_windows(
    snapshot: &LiveSpaceSnapshot,
    workspaces: &mut DestructionWorkspaces,
    windows: &ReassignmentWindows,
    commands: &mut Commands,
) -> bool {
    let mut unresolved = false;
    let mut tiled_by_route: HashMap<(Entity, WorkspaceId), Vec<Entity>> = HashMap::new();
    for (entity, window, floating, visibility, pending) in windows {
        let source = workspaces
            .iter()
            .find_map(|(strip, source, _, retiring, _, _)| {
                strip
                    .contains(entity)
                    .then_some((source, retiring.is_some()))
            });
        if source.is_some_and(|(_, retiring)| retiring) {
            continue;
        }
        let Some(workspace_id) = unique_membership(snapshot, window.id()) else {
            unresolved = true;
            continue;
        };
        let Some(target) = target_workspace(snapshot, workspace_id) else {
            unresolved = true;
            continue;
        };

        if !floating
            && visibility.is_none()
            && let Some((source, _)) = source
        {
            tiled_by_route
                .entry((source, workspace_id))
                .or_default()
                .push(entity);
            continue;
        }
        remove_windows_from_strips(&[entity], workspaces);
        if floating {
            finish_rehomed_windows(&[entity], commands);
            continue;
        }
        if visibility.is_some() {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands
                    .try_insert(PreviousTiledStrip {
                        workspace_id,
                        index: pending.index,
                    })
                    .try_remove::<WindowSpaceReassignmentPending>();
            }
            continue;
        }
        if let Ok((mut target_strip, ..)) = workspaces.get_mut(target.entity) {
            target_strip.insert_at(pending.index, entity);
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<PreviousTiledStrip>();
            }
            finish_rehomed_windows(&[entity], commands);
        } else {
            unresolved = true;
        }
    }
    for ((source, workspace_id), entities) in tiled_by_route {
        let Some(target) = target_workspace(snapshot, workspace_id) else {
            unresolved = true;
            continue;
        };
        if source == target.entity {
            // An unchanged membership must preserve columns, tabs and scrolling.
            if let Ok((mut strip, ..)) = workspaces.get_mut(source) {
                strip.set_changed();
            }
            release_reassignment_barriers(&entities, commands);
        } else {
            let moved = move_layout_group(source, target, &entities, workspaces);
            unresolved |= moved.len() != entities.len();
            finish_rehomed_windows(&moved, commands);
        }
    }
    unresolved
}

fn defer_reconciliation(backoff: &mut SpaceReconciliationBackoff, reason: &'static str) {
    backoff.attempts = backoff.attempts.saturating_add(1);
    let shift = backoff.attempts.min(5);
    backoff.skip_frames = (1_u8 << shift).saturating_sub(1);
    if backoff.attempts >= 8 && backoff.attempts.is_power_of_two() {
        warn!(
            attempts = backoff.attempts,
            retry_frames = backoff.skip_frames,
            reason,
            "destroyed Space reconciliation deferred"
        );
    } else {
        debug!(
            attempts = backoff.attempts,
            retry_frames = backoff.skip_frames,
            reason,
            "destroyed Space reconciliation deferred"
        );
    }
}

/// Rehomes windows from destroyed Spaces using current `WindowServer` membership.
/// Space notifications are only invalidation hints: their producers can race,
/// and membership may settle a frame later than the destruction notification.
#[instrument(level = Level::DEBUG, skip_all)]
fn reconcile_destroyed_workspace_membership(
    mut workspaces: DestructionWorkspaces,
    windows: ReassignmentWindows,
    active_display: Query<&Display, With<ActiveDisplayMarker>>,
    window_manager: Res<WindowManager>,
    topology: Res<super::topology::NativeTopology>,
    mut backoff: Local<SpaceReconciliationBackoff>,
    mut commands: Commands,
) {
    let pending = pending_workspace_snapshot(&workspaces);
    if pending.is_empty() && windows.iter().next().is_none() {
        *backoff = SpaceReconciliationBackoff::default();
        return;
    }
    if pending.iter().any(|workspace| workspace.changed) {
        *backoff = SpaceReconciliationBackoff::default();
    } else if backoff.skip_frames > 0 {
        backoff.skip_frames -= 1;
        return;
    }

    let active_workspace_id = active_display
        .single()
        .ok()
        .and_then(|display| topology.visible_space(display.id()));
    let snapshot =
        live_space_snapshot(&workspaces, &window_manager, active_workspace_id, &topology);
    if pending.iter().any(|workspace| workspace.was_active)
        && let Some(active_target) = snapshot.active_target
        && let Ok(mut entity_commands) = commands.get_entity(active_target.entity)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }

    if snapshot.memberships.is_none() {
        defer_reconciliation(&mut backoff, "membership snapshot incomplete");
        return;
    }

    let mut unresolved =
        rehome_detached_pending_windows(&snapshot, &mut workspaces, &windows, &mut commands);
    for source in &pending {
        rehome_pending_workspace(source, &snapshot, &mut workspaces, &windows, &mut commands);
    }

    for source in pending {
        let active_state_reconciled = !source.was_active || snapshot.active_target.is_some();
        let source_empty = workspaces
            .get(source.entity)
            .is_ok_and(|(strip, ..)| strip.len() == 0);
        let topology_reconciled = snapshot
            .observed_display_ids
            .contains(&source.source_display_id)
            && snapshot
                .topology_by_display
                .get(&source.source_display_id)
                .is_some_and(|spaces| !spaces.contains(&source.workspace_id));
        if topology_reconciled
            && active_state_reconciled
            && source_empty
            && let Ok(mut entity_commands) = commands.get_entity(source.entity)
        {
            debug!(source = ?source.entity, "destroyed Space membership reconciled");
            entity_commands.try_despawn();
        } else {
            unresolved = true;
        }
    }
    if unresolved {
        defer_reconciliation(&mut backoff, "membership or topology is not settled");
    } else {
        *backoff = SpaceReconciliationBackoff::default();
    }
}

fn windows_not_in_strips<F: Fn(WinID) -> Option<Entity>>(
    workspace_id: WorkspaceId,
    find_window: F,
    strips: &[&LayoutStrip],
    window_manager: &WindowManager,
) -> Result<(Vec<Entity>, Vec<WinID>)> {
    window_manager
        .windows_in_workspace(workspace_id)
        .map(|ids| {
            let mut moved = Vec::new();
            let mut unresolved = Vec::new();
            for id in ids {
                if let Some(entity) = find_window(id) {
                    // If window exists in any of the active workspace rows.
                    if strips.iter().any(|strip| strip.contains(entity)) {
                        continue;
                    }
                    moved.push(entity);
                } else {
                    unresolved.push(id);
                }
            }
            (moved, unresolved)
        })
}

/// Retained geometry intent only; a viewport refresh reads window identity and
/// floating ownership, never a storable tiled size.
type RefreshableWindows<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static mut Window, Has<Floating>),
    (
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

fn reconcile_workspace_refresh(
    layout_strip: Populated<
        (&RefreshWindowSizes, &LayoutStrip, Entity, &ChildOf),
        Without<PendingSpaceDestruction>,
    >,
    mut windows: RefreshableWindows,
    displays: Query<(&Display, Option<&DockPosition>)>,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut commands: Commands,
) {
    for (_, strip, strip_entity, child) in
        layout_strip.into_iter().filter(|marker| marker.0.ready())
    {
        debug!("refreshing workspace {} sizes", strip.id());
        let Ok((display, dock)) = displays.get(child.parent()) else {
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);

        let Ok(mut in_workspace) =
            window_manager
                .windows_in_workspace(strip.id())
                .inspect_err(|err| {
                    warn!("getting windows in workspace: {err}");
                })
        else {
            continue;
        };

        // Retained tiled geometry is derived by the layout projection from the
        // accepted width/height intent, so a viewport change must not write a
        // size here: a direct write would bypass declarative intent and can
        // revive stale frames. Only external inventory is reconciled below.
        for entity in strip.all_windows() {
            let Ok((_, window, _)) = windows.get_mut(entity) else {
                continue;
            };
            in_workspace.retain(|window_id| *window_id != window.id());
        }

        // Find remaining windows which are outside of the strip.                                                  ...
        let floating = in_workspace
            .into_iter()
            .filter_map(|window_id| {
                windows.iter().find_map(|(entity, window, floating)| {
                    (window_id == window.id() && floating).then_some(entity)
                })
            })
            .collect::<Vec<_>>();
        for window_entity in floating {
            let Ok((_, mut window, _)) = windows.get_mut(window_entity) else {
                continue;
            };
            let Ok(frame) = window.update_frame() else {
                continue;
            };
            let last_origin = (viewport.max - frame.size()).max(viewport.min);
            let origin = frame.min.clamp(viewport.min, last_origin);
            if origin != frame.min {
                commands.reposition_entity(window_entity, origin);
            }
        }

        if let Ok(mut cmds) = commands.get_entity(strip_entity) {
            cmds.try_remove::<RefreshWindowSizes>();
        }
    }
}

/// Removes previuos `ActiveWorkspaceMarker`'s when a new one is inserted.
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn cleanup_active_workspace_marker(
    trigger: On<Add, ActiveWorkspaceMarker>,
    workspaces: Query<(Entity, Has<ActiveWorkspaceMarker>), With<LayoutStrip>>,
    mut commands: Commands,
) {
    workspaces.iter().for_each(|(entity, marker)| {
        if entity != trigger.entity
            && marker
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            // Remove the active marker from any other workspace.
            entity_commands.try_remove::<ActiveWorkspaceMarker>();
        }
    });
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn restore_focus_on_space_activation(
    trigger: On<Add, ActiveWorkspaceMarker>,
    workspaces: Query<&LayoutStrip>,
    windows: Windows,
    window_manager: Res<WindowManager>,
    focus: Res<FocusCoordinator>,
    mut commands: Commands,
) {
    let Ok(strip) = workspaces.get(trigger.entity) else {
        return;
    };
    let workspace_id = strip.id();
    let workspace_window_ids = window_manager.windows_in_workspace(workspace_id).ok();
    let eligible = |entity| {
        let Some((window, _, state)) = windows.get_tracked(entity) else {
            return false;
        };
        if !state.is_visible() {
            return false;
        }
        if state.is_floating() {
            workspace_window_ids
                .as_ref()
                .is_some_and(|window_ids| window_ids.contains(&window.id()))
        } else {
            strip.contains(entity)
        }
    };

    let snapshot = focus.snapshot();
    if snapshot.requested_entity().is_some() || snapshot.confirmed_entity().is_some_and(eligible) {
        return;
    }

    if let Some(entity) = focus.restoration_entity(workspace_id, eligible) {
        debug!(
            workspace_id,
            ?entity,
            "restoring the Space's previous focus"
        );
        commands.restore_focus_entity(entity, true);
    }
}

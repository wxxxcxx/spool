use bevy::app::{App, Last, Plugin, PreUpdate, Update};
use bevy::ecs::change_detection::{DetectChanges, Ref};
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
use bevy::time::Time;
use bevy::time::common_conditions::on_timer;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{Level, debug, error, instrument, warn};

use super::{ActiveDisplayMarker, SpawnWindowTrigger};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{self, VisibleNativeSpaceMarker};
use crate::ecs::params::{WindowCtx, Windows};
use crate::ecs::reconcile::WindowUnavailable;
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DockPosition, EnsureVisibleMarker, Floating,
    FullscreenDefaultsDeferred, Initializing, NativeFullscreenMarker, Position, PreviousTiledStrip,
    RefreshWindowSizes, RepositionMarker, ReshuffleAroundMarker, ResizeMarker, SpawnCommandsExt,
    Timeout, VerifyWindowPosition, WindowFrameMotion, WindowVisibility,
};
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Application, Display, Size, Window, WindowManager};
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
}

/// Freezes a window while macOS is moving it away from a disappearing Space.
/// The `WindowServer` membership snapshot, rather than notification order, ends
/// this state.
#[derive(Component, Debug)]
pub(crate) struct WindowSpaceReassignmentPending {
    index: usize,
}

impl Plugin for WorkspaceEventsPlugin {
    fn build(&self, app: &mut App) {
        const REFRESH_WINDOW_CHECK_FREQ_MS: u64 = 1000;
        const DISPLAY_CHANGE_CHECK_FREQ_MS: u64 = 1000;

        app.init_resource::<native_space::NativeSpaceTransactions>();
        app.add_systems(
            PreUpdate,
            (
                native_space::handle_focus_window_commands,
                native_space::handle_native_space_commands,
                invalidate_missing_workspaces.after(super::systems::pump_events),
            ),
        );
        app.add_systems(
            Update,
            (
                workspace_change_handler,
                detect_moved_windows.run_if(not(resource_exists::<Initializing>)),
                refresh_workspace_window_sizes.run_if(on_timer(Duration::from_millis(
                    REFRESH_WINDOW_CHECK_FREQ_MS,
                ))),
                find_orphaned_workspaces
                    .after(crate::ecs::display::reconcile_displays)
                    .run_if(on_timer(Duration::from_millis(
                        DISPLAY_CHANGE_CHECK_FREQ_MS,
                    ))),
            ),
        );
        app.add_systems(
            Last,
            (
                native_space::reconcile_native_spaces,
                reconcile_destroyed_workspace_membership,
                native_space::reconcile_native_space_transactions,
            )
                .chain(),
        );
        app.add_observer(cleanup_active_workspace_marker);
        app.add_observer(restore_focus_on_space_activation);
    }
}

fn fullscreen_window_in_strip(
    workspace_id: WorkspaceId,
    strip: &LayoutStrip,
    windows: &Windows,
    window_manager: &WindowManager,
) -> Option<Entity> {
    window_manager
        .windows_in_workspace(workspace_id)
        .ok()
        .and_then(|window_ids| {
            window_ids.into_iter().find_map(|window_id| {
                windows
                    .find_tiled(window_id)
                    .map(|(_, entity)| entity)
                    .filter(|entity| strip.contains(*entity))
            })
        })
        .or_else(|| {
            windows.tiled_iter().find_map(|(window, entity, _)| {
                (strip.contains(entity) && window.is_full_screen()).then_some(entity)
            })
        })
        .or_else(|| {
            windows
                .focused()
                .map(|(_, entity)| entity)
                .filter(|entity| strip.contains(*entity))
        })
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn workspace_change_handler(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<ActiveWorkspaceMarker>)>,
    active_display: Single<(&Display, Entity), With<ActiveDisplayMarker>>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !messages
        .read()
        .any(|event| matches!(event, Event::SpaceChanged))
    {
        return;
    }
    let (active_display, display_entity) = *active_display;

    let Ok(workspace_id) = window_manager.active_display_space(active_display.id()) else {
        error!("Unable to get active workspace id!");
        return;
    };

    let mut remove_from = None;
    let mut insert_into = None;
    for (strip, entity, active) in &workspaces {
        if active && strip.id() == workspace_id {
            debug!("Workspace id {} already active", strip.id());
            return;
        }
        if active && strip.id() != workspace_id {
            debug!("Workspace id {} no longer active", strip.id());
            remove_from = Some(entity);
        }
        if !active && strip.id() == workspace_id {
            debug!("Workspace id {} is active", strip.id());
            insert_into = Some(entity);
        }
    }

    if insert_into.is_none() {
        // The Space may have been observed before its display association was
        // updated; find its sole strip by stable ID.
        insert_into = workspaces
            .iter()
            .find(|(strip, _, _)| strip.id() == workspace_id)
            .map(|(_, entity, _)| entity);
    }

    if insert_into.is_none()
        && let Some(old_space) = remove_from
        && window_manager.is_fullscreen_space(active_display.id())
        && let Ok((mut old_strip, old_strip_entity, _)) = workspaces.get_mut(old_space)
        && let Some(fullscreen_window) =
            fullscreen_window_in_strip(workspace_id, &old_strip, &windows, &window_manager)
        && let Ok(original_index) = old_strip.index_of(fullscreen_window)
    {
        debug!("workspace_change: space={workspace_id} fullscreen");

        let fullscreen_marker = NativeFullscreenMarker {
            layout_strip: old_strip_entity,
            workspace_id: old_strip.id(),
            index: original_index,
        };
        old_strip.remove(fullscreen_window);
        if let Ok(mut entity_commands) = commands.get_entity(fullscreen_window) {
            entity_commands.remove::<(
                RepositionMarker,
                ResizeMarker,
                WindowFrameMotion,
                VerifyWindowPosition,
            )>();
        }

        let fullscreen_strip = LayoutStrip::fullscreen(workspace_id, fullscreen_window);
        let entity = commands
            .spawn((
                Position(active_display.bounds().min),
                fullscreen_marker,
                fullscreen_strip,
                ChildOf(display_entity),
            ))
            .id();
        insert_into = Some(entity);
    }

    if let Some(into) = insert_into
        && let Ok(mut entity_commands) = commands.get_entity(into)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn detect_moved_windows(
    activated_workspace: Single<Entity, Added<ActiveWorkspaceMarker>>,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<NativeFullscreenMarker>)>,
    pending_windows: Query<(), With<WindowSpaceReassignmentPending>>,
    apps: Query<&mut Application>,
    window_manager: Res<WindowManager>,
    mut ignored_windows: Local<HashSet<WinID>>,
    mut ctx: WindowCtx,
) {
    let Ok(workspace_id) = workspaces
        .get(*activated_workspace)
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
            if strip_entity == *activated_workspace {
                strip.append_tab_group(&moving_entities);
            } else {
                for moving_entity in &moving_entities {
                    strip.remove(*moving_entity);
                }
            }
        }
    }
}

fn topology_event(event: &Event) -> bool {
    matches!(
        event,
        Event::SpaceChanged
            | Event::SpaceCreated { .. }
            | Event::SpaceDestroyed { .. }
            | Event::SystemWoke { .. }
            | Event::DisplayAdded { .. }
            | Event::DisplayRemoved { .. }
            | Event::DisplayMoved { .. }
            | Event::DisplayResized { .. }
            | Event::DisplayConfigured { .. }
    )
}

type InvalidatedWorkspaces<'w, 's> = Populated<
    'w,
    's,
    (
        &'static LayoutStrip,
        Entity,
        &'static ChildOf,
        Option<&'static PendingSpaceDestruction>,
        Option<&'static NativeFullscreenMarker>,
        Has<ActiveWorkspaceMarker>,
    ),
>;

type DetachedPreviousWindows<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static PreviousTiledStrip),
    (With<Window>, Without<WindowSpaceReassignmentPending>),
>;

#[derive(SystemParam)]
struct MissingWorkspaceCtx<'w, 's> {
    workspaces: InvalidatedWorkspaces<'w, 's>,
    detached_windows: DetachedPreviousWindows<'w, 's>,
    displays: Query<'w, 's, &'static Display>,
    window_manager: Res<'w, WindowManager>,
    focus: ResMut<'w, FocusCoordinator>,
    commands: Commands<'w, 's>,
}

fn freeze_window_for_space_reassignment(window: Entity, index: usize, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(window) {
        entity_commands
            .try_insert(WindowSpaceReassignmentPending { index })
            .try_remove::<(
                RepositionMarker,
                ResizeMarker,
                WindowFrameMotion,
                VerifyWindowPosition,
                ReshuffleAroundMarker,
                EnsureVisibleMarker,
            )>();
    }
}

/// Treats native Space notifications as invalidation hints. Explicit destroy
/// events are applied immediately; every topology event also detects a missed
/// destroy by comparing the strip IDs with the complete topology for its
/// display.
#[instrument(level = Level::DEBUG, skip_all)]
fn invalidate_missing_workspaces(
    mut messages: MessageReader<Event>,
    time: Res<Time>,
    mut since_audit: Local<Duration>,
    ctx: MissingWorkspaceCtx,
) {
    let MissingWorkspaceCtx {
        workspaces,
        detached_windows,
        displays,
        window_manager,
        mut focus,
        mut commands,
    } = ctx;
    const TOPOLOGY_HEARTBEAT: Duration = Duration::from_secs(1);

    let mut destroyed = HashSet::new();
    let mut topology_changed = false;
    for event in messages.read() {
        topology_changed |= topology_event(event);
        if let Event::SpaceDestroyed { space_id } = event {
            destroyed.insert(*space_id);
        }
    }
    *since_audit = since_audit.saturating_add(time.delta());
    let heartbeat = *since_audit >= TOPOLOGY_HEARTBEAT;
    if !topology_changed && !heartbeat {
        return;
    }
    *since_audit = Duration::ZERO;

    let topology = window_manager.present_displays();
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

    for (strip, entity, child, pending, fullscreen, active) in &workspaces {
        let source_display_id = pending
            .map(|pending| pending.source_display_id)
            .or_else(|| displays.get(child.parent()).ok().map(Display::id));
        let display_topology_available =
            source_display_id.is_some_and(|id| observed_displays.contains(&id));
        let missing = destroyed.contains(&strip.id())
            || (display_topology_available && !present_spaces.contains(&strip.id()));
        if !missing {
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
                });
        }
        for window in strip.all_windows() {
            let index = fullscreen
                .filter(|_| strip.first().ok().and_then(|column| column.top()) == Some(window))
                .map_or_else(
                    || strip.index_of(window).unwrap_or(strip.len()),
                    |marker| marker.index,
                );
            freeze_window_for_space_reassignment(window, index, &mut commands);
        }
        for (window, previous) in &detached_windows {
            if previous.workspace_id != strip.id() || attached_windows.contains(&window) {
                continue;
            }
            freeze_window_for_space_reassignment(window, previous.index, &mut commands);
        }
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
    memberships: HashMap<WorkspaceId, Vec<WinID>>,
    active_target: Option<SurvivingWorkspace>,
    observed_display_ids: HashSet<u32>,
    topology_by_display: HashMap<u32, HashSet<WorkspaceId>>,
    complete: bool,
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
) -> LiveSpaceSnapshot {
    let destroyed_spaces = workspaces
        .iter()
        .filter_map(|(_, _, _, pending, _, _)| pending.map(|pending| pending.workspace_id))
        .collect::<HashSet<_>>();
    let topology = window_manager.present_displays();
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
        .collect::<HashSet<_>>();
    let mut candidates = workspaces
        .iter()
        .filter_map(|(strip, entity, _, pending, active, visible)| {
            (pending.is_none()
                && !destroyed_spaces.contains(&strip.id())
                && present_spaces.contains(&strip.id())
                && !window_manager.workspace_is_fullscreen(strip.id()))
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

    let mut membership_spaces = present_spaces
        .iter()
        .copied()
        .filter(|workspace_id| {
            !destroyed_spaces.contains(workspace_id)
                && !window_manager.workspace_is_fullscreen(*workspace_id)
        })
        .collect::<Vec<_>>();
    membership_spaces.sort_unstable();
    let mut memberships = HashMap::new();
    let mut complete = true;
    for workspace_id in membership_spaces {
        match window_manager.windows_in_workspace(workspace_id) {
            Ok(window_ids) => {
                memberships.insert(workspace_id, window_ids);
            }
            Err(error) => {
                complete = false;
                debug!(
                    workspace_id,
                    %error,
                    "destroyed Space membership snapshot incomplete"
                );
            }
        }
    }
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
        complete,
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
    Without<WindowUnavailable>,
>;

fn unique_membership(snapshot: &LiveSpaceSnapshot, window_id: WinID) -> Option<WorkspaceId> {
    let mut matches = snapshot
        .memberships
        .iter()
        .filter_map(|(workspace_id, ids)| ids.contains(&window_id).then_some(*workspace_id));
    let target = matches.next()?;
    matches.next().is_none().then_some(target)
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

fn finish_rehomed_windows(windows: &[Entity], commands: &mut Commands) {
    for entity in windows {
        if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            entity_commands
                .try_remove::<(WindowSpaceReassignmentPending, FullscreenDefaultsDeferred)>();
        }
    }
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
    for (entity, window, floating, visibility, pending) in windows {
        let still_in_source = workspaces
            .iter()
            .any(|(strip, _, _, source, _, _)| source.is_some() && strip.contains(entity));
        if still_in_source {
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
        .and_then(|display| window_manager.active_display_space(display.id()).ok());
    let snapshot = live_space_snapshot(&workspaces, &window_manager, active_workspace_id);
    if pending.iter().any(|workspace| workspace.was_active)
        && let Some(active_target) = snapshot.active_target
        && let Ok(mut entity_commands) = commands.get_entity(active_target.entity)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }

    if !snapshot.complete {
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

#[instrument(level = Level::DEBUG, skip_all)]
fn find_orphaned_workspaces(
    orphans: Populated<(&LayoutStrip, Entity, &Timeout, Option<&ChildOf>), With<Timeout>>,
    displays: Populated<(&Display, Entity)>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    let present = window_manager.present_displays();

    for (orphan, orphan_entity, timeout, child) in orphans {
        if orphan.len() == 0 {
            if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
                cmd.try_despawn();
            }
            debug!("despawning empty orphan workspace {}", orphan.id());
            continue;
        }
        if child.is_some() {
            // Was reparented, remove timer.
            if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
                cmd.try_remove::<Timeout>();
                cmd.insert(RefreshWindowSizes::default());
            }
            debug!(
                "layout strip {} was re-parented, removing timeout.",
                orphan.id()
            );
            continue;
        }

        if timeout.timer.is_finished() {
            // Rescue windows from orphaned strips before despawning by floating them.
            debug!("Rescue windows from timed out orphan {}.", orphan.id());
            for lost_window in orphan.all_windows() {
                if let Ok(mut cmd) = commands.get_entity(lost_window) {
                    cmd.try_insert(Floating);
                }
            }
            continue;
        }

        // Find which display now owns this space ID.
        let target = present.iter().find_map(|(present_display, spaces)| {
            if spaces.iter().any(|&id| id == orphan.id()) {
                displays
                    .iter()
                    .find(|(d, _)| d.id() == present_display.id())
            } else {
                None
            }
        });
        let Some((target_display, target_entity)) = target else {
            continue; // No display owns this space yet; wait for next tick.
        };

        debug!(
            "Re-parenting orphaned strip {} to display {}",
            orphan.id(),
            target_display.id(),
        );

        if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
            cmd.try_remove::<Timeout>()
                .insert(ChildOf(target_entity))
                .insert(RefreshWindowSizes::default());
        }
    }
}

fn refresh_workspace_window_sizes(
    layout_strip: Populated<
        (&RefreshWindowSizes, &LayoutStrip, Entity, &ChildOf),
        Without<PendingSpaceDestruction>,
    >,
    mut windows: Query<(Entity, &mut Window, &mut Bounds, Has<Floating>)>,
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

        let mut in_workspace = window_manager
            .windows_in_workspace(strip.id())
            .inspect_err(|err| {
                warn!("getting windows in workspace: {err}");
            })
            .unwrap_or_default();

        // Resize windows for the new display dimensions.
        for entity in strip.all_windows() {
            let Ok((_, ref mut window, ref mut bounds, _)) = windows.get_mut(entity) else {
                continue;
            };
            if bounds.x > viewport.width() || bounds.y > viewport.height() {
                let clamped_size = Size::new(
                    bounds.x.clamp(0, viewport.width()),
                    bounds.y.clamp(0, viewport.height()),
                );
                debug!("refreshing window {} size to {clamped_size}", window.id());
                commands.resize_entity(entity, clamped_size);
            }

            in_workspace.retain(|window_id| *window_id != window.id());
        }

        // Find remaining windows which are outside of the strip.                                                  ...
        let floating = in_workspace.into_iter().filter_map(|window_id| {
            windows.iter().find_map(|(entity, window, _, floating)| {
                (window_id == window.id() && floating).then_some(entity)
            })
        });
        for window_entity in floating {
            debug!("repositioning floating window {window_entity}");
            commands.reposition_entity(window_entity, viewport.min);
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
    if snapshot.requested_entity().is_some_and(eligible)
        || snapshot.confirmed_entity().is_some_and(eligible)
    {
        return;
    }

    if let Some(entity) = focus.restoration_entity(workspace_id, eligible) {
        debug!(
            workspace_id,
            ?entity,
            "restoring the Space's previous focus"
        );
        commands.focus_entity(entity, true);
    }
}

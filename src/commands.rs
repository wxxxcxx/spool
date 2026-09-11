use bevy::app::PreUpdate;
use bevy::ecs::entity::{Entity, EntityHashSet};
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, Or, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, In, Query, Res, ResMut};
use bevy::math::IRect;
use bevy::time::Time;
use tracing::{Level, instrument};
use tracing::{debug, error, info};

mod display_navigation;
mod query;

use display_navigation::{DisplayTarget, display_at, select_display, visible_frame};

use crate::config::Config;
use crate::ecs::display::FloatingLayer;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{
    Column, LayoutStrip, StackItem, centered_origin_in_viewport, clamp_origin_to_viewport,
};
use crate::ecs::native_space::{
    DisplayMovePlan, NativeMoveOwner, NativeSpaceTransactions, VisibleNativeSpaceMarker,
};
use crate::ecs::params::{ActiveDisplay, ActiveDisplayMut, Windows};
use crate::ecs::topology::NativeTopology;
use crate::ecs::window_frame::{checked_frame_size, checked_window_frame};
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, Floating, FocusedMarker, FullWidthMarker,
    NativeFullscreenMarker, RaiseWindow, RetilePending, RetileWindow, SendMessageTrigger,
    SpawnCommandsExt,
};
use crate::events::{Event, ReconcileScope};
use crate::manager::{Application, Display, Origin, Size, Window, WindowManager, origin_from};
use crate::platform::WorkspaceId;
use crate::util::round_px;

// The action vocabulary itself lives in `spool-shared-types`, shared with the
// Lua module so every host dispatches the same types.
pub use spool_shared_types::commands::{
    Action, Direction, FocusStep, MouseMove, MoveFocus, Operation, ResizeAxis, ResizeDirection,
};

/// The visible Space on every display except the focused one.
type OffscreenStrips<'w, 's> = Query<
    'w,
    's,
    (&'static mut LayoutStrip, &'static ChildOf),
    (
        With<VisibleNativeSpaceMarker>,
        Without<ActiveWorkspaceMarker>,
        Without<PendingSpaceDestruction>,
        Without<NativeFullscreenMarker>,
    ),
>;

type DisplayTransferBlockedWindows<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<NativeMoveOwner>,
        With<WindowSpaceReassignmentPending>,
        With<crate::ecs::WindowDefaultsPending>,
    )>,
>;

/// Every strip alongside whether it is focused and visible on its display.
type StripsWithVisibility<'w, 's> = Query<
    'w,
    's,
    (
        &'static ChildOf,
        &'static LayoutStrip,
        Entity,
        Has<ActiveWorkspaceMarker>,
        Has<VisibleNativeSpaceMarker>,
    ),
>;

const MIN_RESIZABLE_WINDOW_SIZE: i32 = 100;

pub fn register_commands(app: &mut bevy::app::App) {
    query::register_query_commands(app);
    // Empty store so the mock harness and saveless runs still have one to
    // answer from; the real app overwrites it from disk.
    app.init_resource::<crate::ecs::script_state::ScriptStateStore>();
    app.add_systems(PreUpdate, crate::ecs::script_state::script_state_handler);
    app.add_systems(
        PreUpdate,
        dispatch_actions.after(crate::ecs::systems::pump_events),
    );
}

/// A single reader owns action order. Cached systems flush their deferred
/// changes and observers before the next action, including nested layout ops.
pub(crate) fn dispatch_actions(mut messages: MessageReader<Event>, mut commands: Commands) {
    use crate::ecs::native_space::apply_native_space_command;
    use crate::platform::mission_control::SystemOverview;

    for event in messages.read() {
        let Event::ActionRequested { action } = event else {
            if matches!(event, Event::LayoutSpaceRequested { .. }) {
                commands.run_system_cached_with(apply_native_space_command, event.clone());
            }
            continue;
        };
        match action {
            Action::Window(operation) => match operation {
                Operation::Focus(_) | Operation::FocusStep(_) => {
                    commands.run_system_cached_with(command_move_focus, operation.clone());
                }
                Operation::Move(direction) => {
                    commands.run_system_cached_with(command_move_window, direction.clone());
                }
                Operation::Resize { .. } | Operation::SetWidth(_) => {
                    commands.run_system_cached_with(resize_window, operation.clone());
                }
                Operation::ToNextDisplay(move_focus) => {
                    commands.run_system_cached_with(
                        move_to_display,
                        (*move_focus, DisplayTarget::Next),
                    );
                }
                Operation::Center => commands.run_system_cached(command_center_window),
                Operation::Maximize => commands.run_system_cached(maximize_window),
                Operation::Equalize => commands.run_system_cached(equalize_column),
                Operation::Balance => commands.run_system_cached(balance_strip),
                Operation::ToggleFloating => commands.run_system_cached(toggle_floating_window),
                Operation::ToggleStack => commands.run_system_cached(toggle_stack_handler),
                Operation::Snap => commands.run_system_cached(snap_window),
                Operation::FocusFloating => commands.run_system_cached(command_focus_floating),
                Operation::FocusTiled => commands.run_system_cached(command_focus_tiled),
                Operation::FocusOtherLayer => commands.run_system_cached(command_focus_other_layer),
                Operation::ToggleTiledVisibility => {
                    commands.run_system_cached(crate::ecs::tiled_visibility::toggle);
                }
            },
            Action::Mouse(MouseMove::ToNextDisplay) => {
                commands.run_system_cached_with(focus_other_display, (None, DisplayTarget::Next));
            }
            Action::FocusWindow { .. }
            | Action::FocusSpace { .. }
            | Action::MoveWindowToSpace { .. }
            | Action::MoveColumnToSpace { .. }
            | Action::CreateSpace { .. }
            | Action::DeleteSpace { .. } => {
                commands.run_system_cached_with(apply_native_space_command, event.clone());
            }
            #[cfg(feature = "lua")]
            Action::Layout(plan) => {
                commands.run_system_cached_with(
                    crate::ecs::layout_ops::apply_layout_plan,
                    plan.clone(),
                );
            }
            #[cfg(not(feature = "lua"))]
            Action::Layout(_) => {}
            Action::ReorderColumn { .. } => {
                commands.run_system_cached_with(command_reorder_column, action.clone());
            }
            Action::ReconcileWindows => commands.run_system_cached(reconcile_windows_handler),
            Action::PrintState => commands.run_system_cached(print_internal_state_handler),
            Action::Quit => commands.run_system_cached(command_quit_handler),
            Action::Restart => commands.run_system_cached(command_restart_handler),
            Action::MissionControl => {
                commands.run_system_cached_with(
                    command_system_overview,
                    SystemOverview::MissionControl,
                );
            }
            Action::ShowDesktop => {
                commands
                    .run_system_cached_with(command_system_overview, SystemOverview::ShowDesktop);
            }
            // Lua execution is asynchronous; its returned plan enters this queue later.
            Action::Lua(_) => {}
        }
    }
}

/// Applies the exact insertion produced by a Bar drag. Both identifiers may
/// name any member of their columns; the complete source column moves.
fn command_reorder_column(
    In(action): In<Action>,
    windows: Windows,
    mut strips: Query<&mut LayoutStrip>,
    mut commands: Commands,
) {
    let Action::ReorderColumn {
        window_id,
        anchor_window_id,
        placement,
    } = action
    else {
        return;
    };
    let Some((_, entity)) = windows.find(window_id) else {
        debug!(window_id, "dragged window is no longer tracked");
        return;
    };
    let Some((_, anchor)) = windows.find(anchor_window_id) else {
        debug!(anchor_window_id, "drop target window is no longer tracked");
        return;
    };
    let Some(mut strip) = strips
        .iter_mut()
        .find(|strip| strip.contains(entity) && strip.contains(anchor))
    else {
        debug!(
            window_id,
            anchor_window_id, "dragged columns share no Space"
        );
        return;
    };
    if !writable_column_range(&windows, &strip, entity, anchor) {
        return;
    }
    if strip.move_column_relative(entity, anchor, placement) {
        commands.reshuffle_around(entity);
        commands.ensure_visible(entity);
    }
}

fn reconcile_windows_handler(mut commands: Commands) {
    commands.trigger(SendMessageTrigger(Event::ReconcileWindows {
        scope: ReconcileScope::All,
    }));
}

fn command_system_overview(
    In(overview): In<crate::platform::mission_control::SystemOverview>,
    manager: Res<WindowManager>,
) {
    if let Err(error) = manager.perform_system_overview(overview) {
        error!(?overview, %error, "unable to request system overview");
    }
}

/// Pending focus owns subsequent commands; an invalid target must not redirect
/// a destructive action back to the previously confirmed window.
fn command_entity(windows: &Windows, focus: &FocusCoordinator) -> Option<Entity> {
    let entity = focus
        .snapshot()
        .requested_entity()
        .or_else(|| windows.focused().map(|(_, entity)| entity))?;
    windows
        .get_tracked(entity)
        .filter(|(_, _, state)| state.is_visible())
        .map(|(_, entity, _)| entity)
}

fn active_command_entity(
    windows: &Windows,
    focus: &FocusCoordinator,
    strip: &LayoutStrip,
    manager: &WindowManager,
) -> Option<Entity> {
    let entity = command_entity(windows, focus)?;
    if !windows.layout_is_writable(entity) {
        return None;
    }
    let (window, _, state) = windows.get_tracked(entity)?;
    let belongs = if state.is_tiled() {
        strip.contains(entity)
    } else {
        manager
            .windows_in_workspace(strip.id())
            .ok()?
            .contains(&window.id())
    };
    belongs.then_some(entity)
}

fn writable_column_range(windows: &Windows, strip: &LayoutStrip, a: Entity, b: Entity) -> bool {
    let (Ok(a), Ok(b)) = (strip.index_of(a), strip.index_of(b)) else {
        return false;
    };
    strip
        .columns()
        .skip(a.min(b))
        .take(a.abs_diff(b) + 1)
        .flat_map(Column::window_iter)
        .all(|entity| windows.layout_is_writable(entity))
}

/// Retrieves a window `Entity` in a specified direction relative to a `current_window_id` within a `LayoutStrip`.
///
/// # Arguments
///
/// * `direction` - The direction (e.g., `West`, `East`, `First`, `Last`, `North`, `South`).
/// * `current_window_id` - The `Entity` of the current window.
/// * `strip` - A reference to the `LayoutStrip` to search within.
///
/// # Returns
///
/// `Some(Entity)` with the found window's entity, otherwise `None`.
#[instrument(level = Level::DEBUG, ret)]
fn get_window_in_direction(
    direction: &Direction,
    entity: Entity,
    strip: &LayoutStrip,
) -> Option<Entity> {
    let index = strip.index_of(entity).ok()?;

    match direction {
        Direction::West => strip.left_neighbour(entity),
        Direction::East => strip.right_neighbour(entity),

        Direction::First => strip.first().ok().and_then(|column| column.top()),

        Direction::Last => strip.last().ok().and_then(|column| column.top()),

        Direction::Nth(index) => strip.get(*index).ok().and_then(|column| column.top()),

        Direction::North => match strip.get(index).ok()? {
            Column::Single(_) | Column::Tabs(_) | Column::Fullscren(_) => None,
            Column::Stack(stack) => stack
                .iter()
                .enumerate()
                .find(|(_, item)| item.contains(entity))
                .and_then(|(index, _)| (index > 0).then(|| stack.get(index - 1)).flatten())
                .and_then(StackItem::top),
        },

        Direction::South => match strip.get(index).ok()? {
            Column::Single(_) | Column::Tabs(_) | Column::Fullscren(_) => None,
            Column::Stack(stack) => stack
                .iter()
                .enumerate()
                .find(|(_, item)| item.contains(entity))
                .and_then(|(index, _)| {
                    (index < stack.len() - 1)
                        .then(|| stack.get(index + 1))
                        .flatten()
                })
                .and_then(StackItem::top),
        },
    }
}

/// 45° direction cone, closest by squared Euclidean distance.
/// `First` / `Last` are strip-only and return `None`.
fn pick_nearest_in_direction(
    direction: &Direction,
    focused_center: bevy::math::IVec2,
    candidates: impl IntoIterator<Item = (Entity, bevy::math::IVec2)>,
) -> Option<Entity> {
    candidates
        .into_iter()
        .filter_map(|(entity, center)| {
            let dx = center.x - focused_center.x;
            let dy = center.y - focused_center.y;
            let in_direction = match direction {
                Direction::East => dx > 0 && dy.abs() <= dx.abs(),
                Direction::West => dx < 0 && dy.abs() <= dx.abs(),
                Direction::North => dy < 0 && dx.abs() <= dy.abs(),
                Direction::South => dy > 0 && dx.abs() <= dy.abs(),
                Direction::First | Direction::Last | Direction::Nth(_) => return None,
            };
            in_direction.then_some((entity, dx * dx + dy * dy))
        })
        .min_by_key(|(_, dist_sq)| *dist_sq)
        .map(|(entity, _)| entity)
}

fn visible_floating_entities(
    windows: &Windows,
    window_manager: &WindowManager,
    workspace_id: WorkspaceId,
    display_bounds: IRect,
) -> Vec<Entity> {
    let workspace_window_ids: std::collections::HashSet<_> = window_manager
        .windows_in_workspace(workspace_id)
        .ok()
        .map(|ids| ids.into_iter().collect())
        .unwrap_or_default();

    let mut visible = windows
        .iter()
        .filter_map(|(_, entity)| {
            let (window, _, state) = windows.get_tracked(entity)?;
            if !state.is_floating() || !state.is_visible() {
                return None;
            }
            if !workspace_window_ids.contains(&window.id()) {
                return None;
            }
            let frame = windows.frame(entity)?;
            (!display_bounds.intersect(frame).is_empty()).then_some((window.id(), entity))
        })
        .collect::<Vec<_>>();
    visible.sort_unstable_by_key(|(window_id, _)| *window_id);
    visible.into_iter().map(|(_, entity)| entity).collect()
}

fn nearest_float_in_direction(
    direction: &Direction,
    focused_entity: Entity,
    windows: &Windows,
    window_manager: &WindowManager,
    workspace_id: WorkspaceId,
    display_bounds: IRect,
) -> Option<Entity> {
    let focused_center = windows.frame(focused_entity)?.center();

    let candidates =
        visible_floating_entities(windows, window_manager, workspace_id, display_bounds)
            .into_iter()
            .filter(|entity| *entity != focused_entity)
            .filter_map(|entity| windows.frame(entity).map(|frame| (entity, frame.center())));

    pick_nearest_in_direction(direction, focused_center, candidates)
}

fn floating_focus_target(
    direction: &Direction,
    focused_entity: Entity,
    windows: &Windows,
    window_manager: &WindowManager,
    active_display: &ActiveDisplay,
) -> Option<Entity> {
    let visible = visible_floating_entities(
        windows,
        window_manager,
        active_display.active_strip().id(),
        active_display.bounds(),
    );
    match direction {
        Direction::First => visible.first().copied(),
        Direction::Last => visible.last().copied(),
        Direction::Nth(_) => None,
        Direction::West | Direction::East | Direction::North | Direction::South => {
            nearest_float_in_direction(
                direction,
                focused_entity,
                windows,
                window_manager,
                active_display.active_strip().id(),
                active_display.bounds(),
            )
        }
    }
}

fn focus_step_target(step: FocusStep, current: Entity, ordered: &[Entity]) -> Option<Entity> {
    if ordered.len() < 2 {
        return None;
    }

    let index = ordered.iter().position(|candidate| *candidate == current);
    let target = match (step, index) {
        (FocusStep::Next, Some(index)) => (index + 1) % ordered.len(),
        (FocusStep::Previous, Some(0) | None) => ordered.len() - 1,
        (FocusStep::Previous, Some(index)) => index - 1,
        (FocusStep::Next, None) => 0,
    };
    Some(ordered[target])
}

fn focus_by_step(
    step: FocusStep,
    focused_entity: Entity,
    windows: &Windows,
    window_manager: &WindowManager,
    active_display: &ActiveDisplay,
    commands: &mut Commands,
) {
    let active_strip = active_display.active_strip();
    let floating = windows
        .get_tracked(focused_entity)
        .is_some_and(|(_, _, state)| state.is_floating() && state.is_visible());
    let ordered = if floating {
        visible_floating_entities(
            windows,
            window_manager,
            active_strip.id(),
            active_display.bounds(),
        )
    } else {
        windows.navigable_strip(active_strip).all_windows()
    };

    if let Some(entity) = focus_step_target(step, focused_entity, &ordered) {
        commands.focus_entity(entity, true);
        if !floating {
            commands.ensure_visible(entity);
        }
    }
}

fn focus_from_native_fullscreen(
    direction: Option<&Direction>,
    windows: &Windows,
    active_display: &ActiveDisplay,
    workspaces: &Query<(&LayoutStrip, Entity, Option<&NativeFullscreenMarker>)>,
    commands: &mut Commands,
) -> bool {
    let Some(NativeFullscreenMarker {
        layout_strip,
        workspace_id,
        index: _,
    }) = active_display.fullscreen()
    else {
        return false;
    };
    if !matches!(direction, Some(Direction::West)) {
        return false;
    }

    let strip = workspaces
        .iter()
        .find_map(|(strip, entity, _)| (entity == *layout_strip).then_some(strip))
        .or_else(|| {
            workspaces
                .iter()
                .find_map(|(strip, _, _)| (strip.id() == *workspace_id).then_some(strip))
        });
    if let Some(entity) = strip.and_then(|strip| {
        windows
            .navigable_strip(strip)
            .last()
            .ok()
            .and_then(|col| col.top())
    }) {
        debug!("fullscreen: swap raising {entity}");
        commands.focus_entity(entity, true);
    }
    true
}

fn entry_focus_target(direction: &Direction, strip: &LayoutStrip) -> Option<Entity> {
    match direction {
        Direction::East | Direction::First => strip.first().ok().and_then(|column| column.top()),
        Direction::West | Direction::Last => strip.last().ok().and_then(|column| column.top()),
        Direction::Nth(index) => strip.get(*index).ok().and_then(|column| column.top()),
        Direction::North | Direction::South => None,
    }
}

fn log_focus_navigation(
    operation: &Operation,
    focus: &FocusCoordinator,
    origin: Entity,
    retained: &LayoutStrip,
    navigable: &LayoutStrip,
    windows: &Windows,
) {
    debug!(target: "spool::navigation", ?operation, space_id = navigable.id(),
        focus = ?focus.snapshot(), ?origin,
        retained = ?retained.all_windows().into_iter().map(|entity| {
            (entity, windows.get_parent_any(entity).map(|(window, _, _)| window.id()))
        }).collect::<Vec<_>>(),
        navigable = ?navigable.all_windows().into_iter().filter_map(|entity| {
            windows.get(entity).map(|window| (entity, window.id()))
        }).collect::<Vec<_>>(),
        "focus navigation projection");
}

/// Handles the "focus" command, moving focus to a window in a specified direction.
///
/// # Arguments
///
/// * `direction` - The `Direction` to move focus (e.g., `Direction::East`).
/// * `current_window` - The `Entity` of the currently focused `Window`.
/// * `strip` - A reference to the active `LayoutStrip`.
/// * `windows` - A query for all `Window` components.
///
/// # Returns
///
/// `Some(Entity)` with the entity of the newly focused window, otherwise `None`.
fn command_move_focus(
    In(operation): In<Operation>,
    windows: Windows,
    workspaces: Query<(&LayoutStrip, Entity, Option<&NativeFullscreenMarker>)>,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    focus: Res<FocusCoordinator>,
    mut commands: Commands,
) {
    let operation = &operation;

    let direction = match operation {
        Operation::Focus(direction) => Some(direction),
        Operation::FocusStep(_) => None,
        _ => return,
    };

    let navigation_strip = windows.navigable_strip(active_display.active_strip());
    let active_strip = &navigation_strip;

    // On a fullscreen space, west returns to the last column in the workspace.
    if focus_from_native_fullscreen(
        direction,
        &windows,
        &active_display,
        &workspaces,
        &mut commands,
    ) {
        return;
    }

    let Some(focused_entity) = focus
        .navigation_entity(active_strip.id())
        .or_else(|| windows.focused().map(|(_, entity)| entity))
    else {
        return;
    };

    log_focus_navigation(
        operation,
        &focus,
        focused_entity,
        active_display.active_strip(),
        active_strip,
        &windows,
    );

    if let Operation::FocusStep(step) = operation {
        focus_by_step(
            *step,
            focused_entity,
            &windows,
            &window_manager,
            &active_display,
            &mut commands,
        );
        return;
    }

    let Some(direction) = direction else {
        return;
    };

    if windows
        .get_tracked(focused_entity)
        .is_some_and(|(_, _, state)| state.is_floating() && state.is_visible())
        // Numeric focus is an absolute tiled-column address, even when focus
        // currently sits in the floating layer. This preserves the public
        // 1-based column contract and provides a direct way back into tiling.
        && !matches!(direction, Direction::Nth(_))
    {
        if let Some(entity) = floating_focus_target(
            direction,
            focused_entity,
            &windows,
            &window_manager,
            &active_display,
        ) {
            commands.focus_entity(entity, true);
        }
        return;
    }

    // If focus is on a window that no longer lives in the active strip
    // (e.g. it just became floating, was minimised on another row, or
    // the OS handed focus to a window we don't track on this strip),
    // `get_window_in_direction` would return None and the user would
    // be unable to leave that window. Enter the active strip from the
    // appropriate side so subsequent presses behave normally.
    let candidate = if active_strip.contains(focused_entity) {
        get_window_in_direction(direction, focused_entity, active_strip).or_else(|| {
            // At the right edge going East, enter the fullscreen workspaces.
            (matches!(direction, Direction::East)
                && active_strip.right_neighbour(focused_entity).is_none())
            .then(|| {
                workspaces
                    .iter()
                    .find(|(strip, _, fullscreen)| {
                        fullscreen.is_some() && strip.id() != active_strip.id()
                    })
                    .and_then(|(strip, _, _)| {
                        windows
                            .navigable_strip(strip)
                            .get(0)
                            .ok()
                            .and_then(|col| col.top())
                    })
            })
            .flatten()
        })
    } else {
        entry_focus_target(direction, active_strip)
    };

    if let Some(entity) = candidate {
        debug!(target: "spool::navigation", ?operation,
            target_window = ?windows.get(entity).map(|window| window.id()),
            "focus navigation target");
        commands.focus_entity(entity, true);
        // Requested focus is already authoritative navigation state. Project
        // its target into the layout immediately so a delayed or dropped AX
        // focus confirmation cannot leave the requested window off-screen.
        // `ensure_visible` is idempotent and only moves the strip by the
        // missing amount; the confirmed-focus path remains responsible for
        // border/dim state and any configured auto-centering.
        commands.ensure_visible(entity);
        return;
    }

    if let Some(target) = DisplayTarget::from_direction(direction) {
        commands.run_system_cached_with(
            focus_other_display,
            (Some(active_display.display().id()), target),
        );
    }
}

pub(crate) fn command_focus_floating(
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    focus: Res<FocusCoordinator>,
    mut commands: Commands,
) {
    let display_bounds = active_display.bounds();
    let workspace_id = active_display.active_strip().id();
    let visible_floats =
        visible_floating_entities(&windows, &window_manager, workspace_id, display_bounds);
    let is_visible_float = |entity: Entity| -> bool { visible_floats.contains(&entity) };

    let target = focus
        .last_floating(workspace_id)
        .filter(|entity| is_visible_float(*entity))
        .or_else(|| visible_floats.into_iter().next());

    if let Some(entity) = target {
        commands.focus_entity(entity, true);
    }
}

fn command_focus_tiled(
    active_display: ActiveDisplay,
    windows: Windows,
    focus: Res<FocusCoordinator>,
    mut commands: Commands,
) {
    let active_strip = windows.navigable_strip(active_display.active_strip());
    let workspace_id = active_strip.id();

    let target = focus
        .last_tiled(workspace_id)
        .filter(|entity| active_strip.contains(*entity))
        .or_else(|| active_strip.all_columns().into_iter().next());

    if let Some(entity) = target {
        commands.focus_entity(entity, true);
        commands.reshuffle_around(entity);
    }
}

/// Focus-and-raise are deliberately coupled here: macOS AX raise can't lift a
/// window above another app's frontmost window, so the target's app must be
/// made frontmost. Other windows in the new top tier are raised within their
/// own apps' stacks as a best-effort.
fn command_focus_other_layer(
    active_display: ActiveDisplay,
    mut floating_layers: Query<&mut FloatingLayer>,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    windows: Windows,
    mut commands: Commands,
) {
    let display_bounds = active_display.bounds();
    let active_strip = &windows.navigable_strip(active_display.active_strip());
    let workspace_id = active_strip.id();

    let visible_floats =
        visible_floating_entities(&windows, &window_manager, workspace_id, display_bounds);
    let visible_float = |entity: Entity| -> bool {
        visible_floats.contains(&entity) && !active_strip.contains(entity)
    };

    let focus_floating = !command_entity(&windows, &focus)
        .and_then(|entity| windows.get_tracked(entity))
        .is_some_and(|(_, _, state)| state.is_floating());
    let target = if focus_floating {
        focus
            .last_floating(workspace_id)
            .filter(|entity| visible_float(*entity))
            .or_else(|| visible_floats.iter().copied().find(|e| visible_float(*e)))
    } else {
        focus
            .last_tiled(workspace_id)
            .filter(|entity| active_strip.contains(*entity))
            .or_else(|| active_strip.all_columns().into_iter().next())
    };
    let Some(target) = target else {
        return;
    };

    if let Ok(mut layer) = floating_layers.get_mut(active_display.active_strip_entity()) {
        layer.front = focus_floating;
    } else {
        let layer = FloatingLayer {
            front: focus_floating,
        };
        commands
            .entity(active_display.active_strip_entity())
            .insert(layer);
    }

    if focus_floating {
        windows
            .iter()
            .filter_map(|(_, e)| visible_float(e).then_some(e))
            .for_each(|entity| {
                commands.trigger(RaiseWindow {
                    entity,
                    with_strip: false,
                });
            });
    } else {
        commands.trigger(RaiseWindow {
            entity: target,
            with_strip: true,
        });
        commands.ensure_visible(target);
    }

    commands.focus_entity(target, true);
    debug!("focused other layer: floating={focus_floating}");
}

/// Moves the focused window. Tiled windows are reordered in the strip;
/// floating windows move geometrically by the configured pixel step.
#[instrument(level = Level::DEBUG, skip_all)]
fn command_move_window(
    In(direction): In<Direction>,
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    mut active_display: ActiveDisplayMut,
    config: Res<Config>,
    mut commands: Commands,
) {
    let direction = &direction;

    let Some((_, current, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip_ref(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity)) else {
        return;
    };

    if state.is_floating() {
        let Some(frame) = windows.requested_frame(current) else {
            return;
        };
        let step = config.floating_window_move_step();
        let delta = match direction {
            Direction::West => bevy::math::IVec2::new(-step, 0),
            Direction::East => bevy::math::IVec2::new(step, 0),
            Direction::North => bevy::math::IVec2::new(0, -step),
            Direction::South => bevy::math::IVec2::new(0, step),
            Direction::First | Direction::Last | Direction::Nth(_) => return,
        };
        let viewport = active_display.actual_bounds(&config);
        if viewport.width() <= 0 || viewport.height() <= 0 {
            return;
        }
        let requested_origin = Origin::new(
            frame.min.x.saturating_add(delta.x),
            frame.min.y.saturating_add(delta.y),
        );
        let origin = clamp_origin_to_viewport(requested_origin, frame.size(), viewport);
        commands.reposition_entity(current, origin);
        return;
    }

    if let Some(other) =
        get_window_in_direction(direction, current, active_display.active_strip_ref())
        && !writable_column_range(&windows, active_display.active_strip_ref(), current, other)
    {
        return;
    }
    let active_strip = active_display.active_strip();
    let mut handler = || {
        let index = active_strip.index_of(current).ok()?;
        let other_window = get_window_in_direction(direction, current, active_strip)?;
        let new_index = active_strip.index_of(other_window).ok()?;
        debug!(
            "move {direction:?}: current={current} idx={index}, other={other_window} idx={new_index}, strip_len={}",
            active_strip.len()
        );

        if index == new_index
            && let Some(Column::Stack(stack)) = active_strip.get_column_mut(index)
        {
            let pos_a = stack.iter().position(|i| i.contains(current))?;
            let pos_b = stack.iter().position(|i| i.contains(other_window))?;
            stack.swap(pos_a, pos_b);
        } else if index < new_index {
            (index..new_index).for_each(|idx| active_strip.swap(idx, idx + 1));
        } else {
            (new_index..index)
                .rev()
                .for_each(|idx| active_strip.swap(idx, idx + 1));
        }
        Some(current)
    };

    // Keep the focused window on-screen, but don't anchor it: if its new
    // layout slot is already visible with the strip where it is, the strip
    // stays put and per-window animation slides the window into the slot.
    // Only when the slot would fall off the edge does the strip scroll —
    // and only by the shortfall.
    if let Some(window) = handler() {
        commands.ensure_visible(window);
        return;
    }
    debug!(
        "move {direction:?}: handler returned None (focused={:?}, strip_len={})",
        current,
        active_strip.len()
    );

    if get_window_in_direction(direction, current, active_strip).is_none()
        && let Some(target) = DisplayTarget::from_direction(direction)
    {
        commands.run_system_cached_with(move_to_display, (MoveFocus::Follow, target));
    }
}

/// Centers the focused window on the active display.
fn command_center_window(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if let Some((_, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity))
        && let Some(frame) = windows.requested_frame(entity)
    {
        let size = frame.size();
        let mut origin = frame.min;
        let viewport = active_display.actual_bounds(&config);
        if viewport.width() <= 0 || viewport.height() <= 0 {
            return;
        }

        if state.is_tiled()
            && active_display.active_strip().contains(entity)
            && let Some(layout_position) = windows.layout_position(entity)
        {
            origin.x = active_display.bounds().center().x - size.x / 2;
            // Directly reposition the strip (bypasses hidden_ratio check).
            let strip_position = origin - layout_position.0;
            commands.reposition_entity(active_display.active_strip_entity(), strip_position);
        } else {
            origin.x = viewport.center().x - size.x / 2;
            origin.y = viewport.center().y - size.y / 2;
            origin = clamp_origin_to_viewport(origin, size, viewport);
            commands.reposition_entity(entity, origin);
        }

        window_manager.warp_mouse(viewport.center());
    }
}

fn resized_dimension(current: i32, delta: i32, available: i32) -> Option<i32> {
    if available <= 0 {
        return None;
    }
    Some(
        current
            .saturating_add(delta)
            .clamp(MIN_RESIZABLE_WINDOW_SIZE.min(available), available),
    )
}

fn floating_resize_frame(
    operation: &Operation,
    frame: IRect,
    viewport: IRect,
    config: &Config,
) -> Option<IRect> {
    let (axis, direction) = match operation {
        Operation::Resize { axis, direction } => (*axis, *direction),
        Operation::SetWidth(_) => (ResizeAxis::Width, ResizeDirection::Grow),
        _ => return None,
    };
    let delta = match direction {
        ResizeDirection::Grow => config.floating_window_resize_step(),
        ResizeDirection::Shrink => -config.floating_window_resize_step(),
    };
    let mut size = frame.size();
    match (operation, axis) {
        (Operation::SetWidth(ratio), ResizeAxis::Width) => {
            size.x = checked_ratio_width(*ratio, viewport.width())?;
        }
        (Operation::SetWidth(_), _) => return None,
        (_, ResizeAxis::Width) => {
            size.x = resized_dimension(size.x, delta, viewport.width())?;
        }
        (_, ResizeAxis::Height) => {
            size.y = resized_dimension(size.y, delta, viewport.height())?;
        }
    }
    checked_window_frame(centered_origin_in_viewport(frame, size, viewport), size)
}

fn checked_ratio_width(ratio: f64, available: i32) -> Option<i32> {
    if available <= 0 {
        return None;
    }
    let width = (ratio * f64::from(available)).round();
    (1.0..=f64::from(i32::MAX))
        .contains(&width)
        .then(|| round_px(width))
}

/// Plans every member before a width change can clear restoration state.
fn column_resize_plan(
    windows: &Windows,
    strip: &LayoutStrip,
    entity: Entity,
    target: IRect,
) -> Option<Vec<(Entity, Size)>> {
    let size = checked_frame_size(target)?;
    if !windows.layout_column_is_writable(strip, entity) {
        return None;
    }
    let column = strip.column_containing(entity)?;
    if matches!(column, Column::Fullscren(_))
        || !strip.accepts_column_width(entity, size.x, |member| windows.requested_frame(member))
    {
        return None;
    }
    column
        .window_iter()
        .map(|member| {
            windows.get(member)?;
            let frame = windows.requested_frame(member)?;
            let height = if member == entity || matches!(column, Column::Tabs(_)) {
                size.y
            } else {
                frame.height()
            };
            let size = Size::new(size.x, height);
            let origin = if member == entity {
                target.min
            } else {
                frame.min
            };
            checked_window_frame(origin, size)?;
            Some((member, size))
        })
        .collect()
}

fn apply_column_sizes(sizes: Vec<(Entity, Size)>, commands: &mut Commands) {
    for (entity, size) in sizes {
        if let Ok(mut entry) = commands.get_entity(entity) {
            entry.try_remove::<FullWidthMarker>();
        }
        commands.resize_entity(entity, size);
    }
}

fn resize_tiled_height(
    entity: Entity,
    frame: IRect,
    viewport: IRect,
    direction: ResizeDirection,
    strip: &LayoutStrip,
    config: &Config,
    commands: &mut Commands,
) -> bool {
    let in_stack = strip
        .index_of(entity)
        .ok()
        .and_then(|index| strip.get(index).ok())
        .is_some_and(|column| matches!(column, Column::Stack(_)));
    if !in_stack {
        return false;
    }

    let delta = match direction {
        ResizeDirection::Grow => config.floating_window_resize_step(),
        ResizeDirection::Shrink => -config.floating_window_resize_step(),
    };
    let Some(height) = resized_dimension(frame.height(), delta, viewport.height()) else {
        return false;
    };
    if height == frame.height()
        || checked_window_frame(frame.min, Size::new(frame.width(), height)).is_none()
    {
        return false;
    }
    commands.resize_entity(entity, Size::new(frame.width(), height));
    commands.reshuffle_around(entity);
    true
}

fn tiled_width_ratio(operation: &Operation, current: f64, config: &Config) -> Option<f64> {
    let widths = config.preset_column_widths();
    let fallback = *widths.first().unwrap_or(&0.5);
    let cycle = config.window_resize_cycle();
    match operation {
        Operation::SetWidth(ratio) if ratio.is_finite() && *ratio > 0.0 => Some(*ratio),
        Operation::Resize {
            direction: ResizeDirection::Grow,
            ..
        } => Some(
            widths
                .iter()
                .copied()
                .find(|&ratio| ratio > current + 0.05)
                .unwrap_or_else(|| {
                    if cycle {
                        fallback
                    } else {
                        *widths.last().unwrap_or(&fallback)
                    }
                }),
        ),
        Operation::Resize {
            direction: ResizeDirection::Shrink,
            ..
        } => Some(
            widths
                .iter()
                .rev()
                .copied()
                .find(|&ratio| ratio < current - 0.05)
                .unwrap_or_else(|| {
                    if cycle {
                        *widths.last().unwrap_or(&fallback)
                    } else {
                        fallback
                    }
                }),
        ),
        _ => None,
    }
}

/// Resizes the focused window using behavior selected by its tiled/floating state.
fn resize_window(
    In(operation): In<Operation>,
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    let operation = &operation;

    let viewport = active_display.actual_bounds(&config);
    if checked_frame_size(viewport).is_none() {
        return;
    }
    if let Operation::SetWidth(ratio) = operation
        && checked_ratio_width(*ratio, viewport.width()).is_none()
    {
        return;
    }

    let Some((_, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity)) else {
        return;
    };
    let Some(frame) = windows.requested_frame(entity) else {
        return;
    };
    if state.is_tiled() && !windows.layout_column_is_writable(active_display.active_strip(), entity)
    {
        return;
    }
    let (axis, direction) = match operation {
        Operation::Resize { axis, direction } => (*axis, *direction),
        Operation::SetWidth(_) => (ResizeAxis::Width, ResizeDirection::Grow),
        _ => return,
    };
    let next_tiled_width = if state.is_tiled() && axis == ResizeAxis::Width {
        let current = f64::from(frame.width()) / f64::from(viewport.width());
        tiled_width_ratio(operation, current, &config)
            .and_then(|ratio| checked_ratio_width(ratio, viewport.width()))
    } else {
        None
    };
    if let Some(width) = next_tiled_width
        && !active_display
            .active_strip()
            .accepts_column_width(entity, width, |member| windows.requested_frame(member))
    {
        return;
    }
    if state.is_tiled() && axis == ResizeAxis::Height {
        if resize_tiled_height(
            entity,
            frame,
            viewport,
            direction,
            active_display.active_strip(),
            &config,
            &mut commands,
        ) && let Ok(mut cmds) = commands.get_entity(entity)
        {
            cmds.try_remove::<FullWidthMarker>();
        }
        return;
    }

    if state.is_floating() {
        let Some(target) = floating_resize_frame(operation, frame, viewport, &config)
            .filter(|target| *target != frame)
        else {
            return;
        };
        if let Ok(mut cmds) = commands.get_entity(entity) {
            cmds.try_remove::<FullWidthMarker>();
        }
        commands.reposition_entity(entity, target.min);
        commands.resize_entity(entity, target.size());
        return;
    }

    let Some(new_width) = next_tiled_width else {
        return;
    };
    let size = Size::new(new_width, frame.height());

    let origin = centered_origin_in_viewport(frame, size, viewport);
    let Some(target) = checked_window_frame(origin, size) else {
        return;
    };
    let Some(sizes) = column_resize_plan(&windows, active_display.active_strip(), entity, target)
    else {
        return;
    };
    apply_column_sizes(sizes, &mut commands);
    commands.reposition_entity(entity, target.min);
    commands.reshuffle_around(entity);
}

fn maximize_window(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    mut active_display: ActiveDisplayMut,
    config: Res<Config>,
    mut commands: Commands,
) {
    let Some((_, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip_ref(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity)) else {
        return;
    };

    let viewport = active_display.actual_bounds(&config);
    let Some(viewport_size) = checked_frame_size(viewport) else {
        return;
    };
    let Some(current) = windows.requested_frame(entity) else {
        return;
    };
    let strip = active_display.active_strip_ref();
    if state.is_tiled() && !windows.layout_column_is_writable(strip, entity) {
        return;
    }
    if state.is_tiled()
        && strip
            .column_containing(entity)
            .is_none_or(|column| matches!(column, Column::Fullscren(_)))
    {
        return;
    }
    // A native tab group's width belongs to the whole entry, even after focus
    // selects a different member than the one that originally maximized it.
    let marker = windows.full_width(entity).or_else(|| {
        state
            .is_tiled()
            .then(|| strip.tab_group(entity))
            .flatten()
            .and_then(|members| {
                members
                    .into_iter()
                    .find_map(|member| windows.full_width(member))
            })
    });
    let restoring = marker.is_some();
    let target = if let Some(marker) = marker {
        if let Some(frame) = marker.floating_frame {
            if checked_frame_size(frame).is_none() {
                return;
            }
            frame
        } else {
            let Some(width) = checked_ratio_width(marker.width_ratio, viewport_size.x) else {
                return;
            };
            let Some(frame) = checked_window_frame(current.min, viewport_size.with_x(width)) else {
                return;
            };
            frame
        }
    } else {
        viewport
    };
    let mut proposed = strip.clone();
    let layout_changed = if state.is_tiled() && !restoring {
        match proposed.unstack(entity) {
            Ok(changed) => changed,
            Err(error) => {
                debug!(%error, "skipping maximize: no eligible layout entry");
                return;
            }
        }
    } else {
        false
    };
    let sizes = if state.is_floating() {
        vec![(entity, target.size())]
    } else {
        let Some(sizes) = column_resize_plan(&windows, &proposed, entity, target) else {
            return;
        };
        sizes
    };
    if layout_changed {
        *active_display.active_strip() = proposed;
    }
    apply_column_sizes(sizes, &mut commands);
    if !restoring && let Ok(mut entry) = commands.get_entity(entity) {
        entry.try_insert(FullWidthMarker {
            width_ratio: f64::from(current.width()) / f64::from(viewport_size.x),
            floating_frame: state.is_floating().then_some(current),
        });
    }
    commands.reposition_entity(entity, target.min);
    if state.is_tiled() {
        commands.reshuffle_around(entity);
    }
}

/// Toggles the focused window between the tiling layout and floating mode.
fn toggle_floating_window(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    pending_retiles: Query<(), With<RetilePending>>,
    mut commands: Commands,
) {
    let Some((window, entity, state)) =
        command_entity(&windows, &focus).and_then(|entity| windows.get_tracked(entity))
    else {
        return;
    };
    debug!(
        "window: {} {entity} floating: {}.",
        window.id(),
        state.is_floating()
    );
    if !state.is_visible() {
        return;
    }
    let was_floating = state.is_floating();
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        if pending_retiles.contains(entity) {
            // A second toggle cancels the outstanding tile intent.
            entity_commands.try_remove::<RetilePending>();
        } else if was_floating {
            commands.trigger(RetileWindow(entity));
        } else {
            entity_commands.try_insert(Floating);
        }
    }
}

/// Plans the complete transfer before publishing layout or geometry changes.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn move_to_display(
    In((move_focus, target)): In<(MoveFocus, DisplayTarget)>,
    windows: Windows,
    focus: Res<FocusCoordinator>,
    active_display: ActiveDisplayMut,
    mut other_workspaces: OffscreenStrips,
    mut topology: ResMut<NativeTopology>,
    moving: DisplayTransferBlockedWindows,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut transactions: ResMut<NativeSpaceTransactions>,
    time: Res<Time>,
    mut commands: Commands,
) {
    let Some((window, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip_ref(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity)) else {
        return;
    };
    let source_id = active_display.active_strip_ref().id();
    if topology.observe_visible_window_space(&window_manager, window.id()) != Some(source_id)
        || topology.visible_display_for_space(source_id) != Some(active_display.display().id())
        || topology.is_fullscreen(source_id)
    {
        return;
    }
    let Some(target_display_id) = select_display(
        active_display.display().id(),
        target,
        topology.known_displays().map(|(display, _)| display),
    ) else {
        return;
    };
    let Some((display_entity, other, dock)) = active_display
        .other_contexts()
        .find(|(_, display, _)| display.id() == target_display_id)
    else {
        debug!("no other display to move window to.");
        return;
    };
    let Some(target_space_id) = topology.visible_space(target_display_id) else {
        return;
    };
    if topology.visible_display_for_space(target_space_id) != Some(target_display_id)
        || topology.is_fullscreen(target_space_id)
    {
        return;
    }
    // A fresh native sample cannot authorize use of stale display geometry.
    for display in [active_display.display(), other] {
        let Some((observed, _)) = topology
            .known_displays()
            .find(|(native, _)| native.id() == display.id())
        else {
            return;
        };
        if display.clone().update_geometry(observed) {
            return;
        }
    }
    let Some(source_viewport) = active_display
        .display()
        .checked_actual_display_bounds(active_display.dock(), &config)
    else {
        return;
    };
    let Some(target_viewport) = other.checked_actual_display_bounds(dock, &config) else {
        return;
    };
    let mut matches = other_workspaces
        .iter_mut()
        .filter(|(strip, _)| strip.id() == target_space_id);
    let Some((target_strip, child)) = matches.next() else {
        return;
    };
    if matches.next().is_some() || child.parent() != display_entity {
        return;
    }
    let Some(frame) = windows.requested_frame(entity) else {
        return;
    };
    let size = if state.is_tiled() {
        let ratio = f64::from(frame.width()) / f64::from(source_viewport.width());
        let Some(width) = checked_ratio_width(ratio, target_viewport.width()) else {
            return;
        };
        Size::new(width, target_viewport.height())
    } else {
        frame.size()
    };
    let origin = centered_origin_in_viewport(target_viewport, size, target_viewport)
        .with_y(target_viewport.min.y);
    let Some(target) = checked_window_frame(origin, size) else {
        return;
    };
    let members = if state.is_tiled() {
        active_display
            .active_strip_ref()
            .tab_group(entity)
            .unwrap_or_else(|| vec![entity])
    } else {
        vec![entity]
    };
    let Ok(memberships) = topology.observe_memberships(&window_manager) else {
        return;
    };
    if members.iter().any(|member| {
        moving.contains(*member)
            || windows
                .get_tracked(*member)
                .is_none_or(|(window, _, sibling)| {
                    !sibling.is_visible()
                        || sibling.is_tiled() != state.is_tiled()
                        || memberships.unique_space(window.id()) != Some(source_id)
                })
    }) {
        return;
    }
    let mut source = active_display.active_strip_ref().clone();
    let mut destination = target_strip.clone();
    if state.is_tiled() {
        for member in &members {
            source.remove(*member);
        }
        destination.append_tab_group(&members);
        if !destination.accepts_column_widths(|_, column| {
            column.width(&|member| {
                if members.contains(&member) {
                    Some(target)
                } else {
                    windows.requested_frame(member)
                }
            })
        }) {
            return;
        }
    }
    let eligible = |candidate| {
        source.contains(candidate)
            && !moving.contains(candidate)
            && windows
                .get_tracked(candidate)
                .is_some_and(|(window, _, state)| {
                    state.is_visible() && memberships.unique_space(window.id()) == Some(source_id)
                })
    };
    let source_neighbour = focus.restoration_entity(source_id, eligible).or_else(|| {
        source
            .all_columns()
            .into_iter()
            .find(|candidate| eligible(*candidate))
    });

    transactions.submit_display_move(
        DisplayMovePlan {
            members,
            target,
            viewport: target_viewport,
            target_space_id,
            source_display_id: active_display.display().id(),
            target_display_id,
            follow: (move_focus == MoveFocus::Follow).then_some(entity),
            source_neighbour,
            tiled: state.is_tiled(),
        },
        &windows,
        active_display.active_strip_ref(),
        &mut commands,
        time.elapsed(),
    );
}

/// Moves the mouse pointer to the next available display.
#[instrument(level = Level::DEBUG, skip_all)]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn focus_other_display(
    In((source, target)): In<(Option<u32>, DisplayTarget)>,
    windows: Windows,
    layout_strips: Query<(&LayoutStrip, &ChildOf), Without<PendingSpaceDestruction>>,
    displays: Query<(Entity, &Display, Option<&crate::ecs::DockPosition>)>,
    mut topology: ResMut<NativeTopology>,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut commands: Commands,
) {
    if !topology.refresh_for_command(&window_manager) {
        return;
    }
    if source.is_some_and(|id| topology.active_display() != Some(id)) {
        return;
    }
    let Some(source_id) = source.or_else(|| {
        display_at(
            window_manager.cursor_position().map(origin_from)?,
            topology.known_displays().map(|(display, _)| display),
        )
    }) else {
        return;
    };
    let Some(target_id) = select_display(
        source_id,
        target,
        topology.known_displays().map(|(display, _)| display),
    ) else {
        return;
    };
    for id in [source_id, target_id] {
        let mut matches = displays.iter().filter(|(_, display, _)| display.id() == id);
        let Some((_, display, _)) = matches.next() else {
            return;
        };
        if matches.next().is_some()
            || !topology
                .known_displays()
                .any(|(native, _)| native.id() == id && !display.clone().update_geometry(native))
            || topology
                .visible_space(id)
                .is_none_or(|space| topology.visible_display_for_space(space) != Some(id))
        {
            return;
        }
    }
    let Some((display_entity, other, dock)) = displays
        .iter()
        .find(|(_, display, _)| display.id() == target_id)
    else {
        return;
    };
    let Some(viewport) = other.checked_actual_display_bounds(dock, &config) else {
        return;
    };
    let Some(space_id) = topology.visible_space(target_id) else {
        return;
    };
    let mut matches = layout_strips
        .iter()
        .filter(|(strip, _)| strip.id() == space_id);
    let Some((other_strip, child)) = matches.next() else {
        return;
    };
    if matches.next().is_some() || child.parent() != display_entity {
        return;
    }
    let Ok(memberships) = topology.observe_memberships(&window_manager) else {
        return;
    };
    let candidate = other_strip
        .all_windows()
        .into_iter()
        .filter_map(|entity| {
            let (window, _, state) = windows.get_tracked(entity)?;
            if !state.is_visible() || memberships.unique_space(window.id()) != Some(space_id) {
                return None;
            }
            Some((
                entity,
                visible_frame(viewport, windows.frame(entity)?)?,
                window.id(),
            ))
        })
        .max_by_key(|(_, frame, id)| (frame.width(), frame.height(), std::cmp::Reverse(*id)));
    let destination = candidate.map_or(viewport, |(_, frame, _)| frame);
    window_manager.warp_mouse(Origin::new(
        destination.min.x.midpoint(destination.max.x),
        destination.min.y.midpoint(destination.max.y),
    ));
    if let Some((entity, _, _)) = candidate {
        commands.focus_entity(entity, true);
    }
}

/// Distributes heights equally among all windows in the currently focused stack.
fn equalize_column(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    let Some(entity) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip(),
        &window_manager,
    ) else {
        return;
    };
    let active_strip = active_display.active_strip();
    let Ok(column) = active_strip
        .index_of(entity)
        .and_then(|index| active_strip.get(index))
    else {
        return;
    };

    if let Column::Stack(stack) = column {
        let viewport = active_display.actual_bounds(&config);
        let Some(equal_height) = i32::try_from(stack.len())
            .ok()
            .and_then(|count| viewport.height().checked_div(count))
            .filter(|height| *height > 0 && viewport.width() > 0)
        else {
            return;
        };
        let sizes = stack
            .iter()
            .flat_map(StackItem::window_iter)
            .map(|entity| {
                windows.layout_is_writable(entity).then_some(())?;
                let frame = windows.requested_frame(entity)?;
                let size = frame.size().with_y(equal_height);
                checked_window_frame(frame.min, size)?;
                Some((entity, size))
            })
            .collect::<Option<Vec<_>>>();
        let Some(sizes) = sizes else {
            return;
        };
        for (entity, size) in sizes {
            commands.resize_entity(entity, size);
        }
    }
}

/// Makes all columns in the active strip the same width as the focused window.
fn balance_strip(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    active_display: ActiveDisplay,
    mut commands: Commands,
) {
    let Some(focused_entity) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip(),
        &window_manager,
    ) else {
        return;
    };
    let Some(focused_width) = windows
        .requested_frame(focused_entity)
        .map(|frame| frame.width())
    else {
        return;
    };

    let strip = active_display.active_strip();
    if !strip.accepts_column_widths(|_, column| {
        if matches!(column, Column::Fullscren(_)) {
            column.width(&|member| windows.requested_frame(member))
        } else {
            Some(focused_width)
        }
    }) {
        return;
    }
    let sizes = strip
        .columns()
        .filter(|column| !matches!(column, Column::Fullscren(_)))
        .flat_map(Column::window_iter)
        .map(|entity| {
            windows.layout_is_writable(entity).then_some(())?;
            let frame = windows.requested_frame(entity)?;
            let size = frame.size().with_x(focused_width);
            checked_window_frame(frame.min, size)?;
            Some((entity, size))
        })
        .collect::<Option<Vec<_>>>();
    let Some(sizes) = sizes.filter(|sizes| !sizes.is_empty()) else {
        return;
    };
    for (entity, size) in sizes {
        if let Ok(mut cmds) = commands.get_entity(entity) {
            cmds.try_remove::<FullWidthMarker>();
        }
        commands.resize_entity(entity, size);
    }

    commands.reshuffle_around(focused_entity);
}

/// Slides the strip so the focused window is fully visible, snapping to the
/// nearest edge: left-aligned when the window overflows left, right-aligned
/// when it overflows right. No resize — the window keeps its current size.
/// Bypasses the lazy-viewport check since the user explicitly asked to reveal.
fn snap_window(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    let Some((_, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity)) else {
        return;
    };
    let Some(mut frame) = windows.moving_frame(entity) else {
        return;
    };

    let display_bounds = active_display.actual_bounds(&config);
    if checked_frame_size(display_bounds).is_none() {
        return;
    }

    // Clamp the frame into the display and reposition the *strip* (not the
    // window) so the layout stays consistent.
    let size = frame.size();
    frame.min = clamp_origin_to_viewport(frame.min, size, display_bounds);
    frame.max = frame.min + size;

    if state.is_floating() {
        commands.reposition_entity(entity, frame.min);
        return;
    }

    if active_display
        .active_strip()
        .column_containing(entity)
        .is_none_or(|column| matches!(column, Column::Fullscren(_)))
    {
        return;
    }

    let Some(layout_position) = windows.layout_position(entity) else {
        return;
    };

    let Some(x) = frame.min.x.checked_sub(layout_position.0.x) else {
        return;
    };
    let Some(y) = frame.min.y.checked_sub(layout_position.0.y) else {
        return;
    };
    let strip_position = Origin::new(x, y);
    commands.reposition_entity(active_display.active_strip_entity(), strip_position);
}

#[instrument(level = Level::DEBUG, skip_all)]
fn toggle_stack_handler(
    windows: Windows,
    focus: Res<FocusCoordinator>,
    window_manager: Res<WindowManager>,
    mut active_display: ActiveDisplayMut,
    mut commands: Commands,
) {
    if let Some((_, entity, state)) = active_command_entity(
        &windows,
        &focus,
        active_display.active_strip_ref(),
        &window_manager,
    )
    .and_then(|entity| windows.get_tracked(entity))
        && state.is_tiled()
        && state.is_visible()
    {
        let original = active_display.active_strip_ref();
        if !windows.layout_column_is_writable(original, entity) {
            return;
        }
        let mut proposed = original.clone();
        let is_stacked = proposed
            .index_of(entity)
            .ok()
            .and_then(|index| proposed.get(index).ok())
            .is_some_and(|column| matches!(column, Column::Stack(_)));
        let changed = if is_stacked {
            proposed.unstack(entity)
        } else {
            let Ok(index) = original.index_of(entity) else {
                return;
            };
            if index > 0
                && original.get(index - 1).is_ok_and(|column| {
                    column
                        .window_iter()
                        .any(|member| !windows.layout_is_writable(member))
                })
            {
                return;
            }
            proposed.stack(entity)
        };
        match changed {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                debug!(%error, "skipping stack toggle: no eligible layout item");
                return;
            }
        }
        if !proposed.accepts_column_widths(|_, column| {
            column.width(&|member| windows.requested_frame(member))
        }) {
            return;
        }
        *active_display.active_strip() = proposed;
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<FullWidthMarker>();
        }

        // Stacking/unstacking moves the focused window to a new column slot
        // (onto the left master, or out to its own column on the right).
        // Reshuffle around it so it is brought fully back into view; the
        // edge-clamp in reshuffle_layout_strip keeps the strip pinned so the
        // leftmost window touches the left edge and the rightmost the right.
        commands.reshuffle_around(entity);
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
fn command_quit_handler(window_manager: Res<WindowManager>) {
    _ = window_manager.quit();
}

#[instrument(level = Level::DEBUG, skip_all)]
fn command_restart_handler() {
    if let Err(err) = crate::platform::service::Service::request_restart() {
        error!("failed to restart service: {err}");
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
fn print_internal_state_handler(
    focused: Query<(&Window, Entity), With<FocusedMarker>>,
    windows: Query<(&Window, Entity, &ChildOf, Has<Floating>)>,
    apps: Query<&Application>,
    workspaces: StripsWithVisibility,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
) {
    let focused = focused.single().ok();
    let print_window = |(window, entity, child, floating): (&Window, Entity, &ChildOf, bool)| {
        let bundle_id = apps
            .get(child.parent())
            .ok()
            .and_then(|app| app.bundle_id())
            .unwrap_or_default();
        format!(
            "\tid: {}, {entity}, {}:{}, {}x{}{}{}, bundle: {}, role: {}, subrole: {}, title: '{:.70}'",
            window.id(),
            window.frame().min.x,
            window.frame().min.y,
            window.frame().width(),
            window.frame().height(),
            if focused.is_some_and(|(_, focus)| focus == entity) {
                ", focused"
            } else {
                ""
            },
            if floating { ", Floating" } else { "" },
            bundle_id,
            window.role().unwrap_or_default(),
            window.subrole().unwrap_or_default(),
            window.title().unwrap_or_default()
        )
    };

    let mut seen = EntityHashSet::new();

    for (display, display_entity, active) in displays {
        for (_, strip, strip_entity, active_workspace, visible) in workspaces
            .iter()
            .filter(|child| child.0.parent() == display_entity)
        {
            let windows = strip
                .all_windows()
                .iter()
                .filter_map(|entity| windows.get(*entity).ok())
                .inspect(|(_, entity, _, _)| {
                    seen.insert(*entity);
                })
                .map(print_window)
                .collect::<Vec<_>>();

            let display_id = display.id();
            info!(
                "Display {display_id}{}, space id {} ({strip_entity}){}{}: {strip}:\n{}",
                if active { ", active" } else { "" },
                strip.id(),
                if active_workspace { ", active" } else { "" },
                if visible { ", visible" } else { "" },
                windows.join("\n")
            );
        }
    }

    let remaining = windows
        .iter()
        .filter(|entity| !seen.contains(&entity.1))
        .map(print_window)
        .collect::<Vec<_>>();
    info!("Remaining:\n{}", remaining.join("\n"));

    if let Some(pool) = bevy::tasks::ComputeTaskPool::try_get() {
        info!("Running with {} threads", pool.thread_num());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MainOptions;
    use crate::ecs::Bounds;
    use crate::events::Event;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;

    #[test]
    fn maximize_restores_pending_floating_geometry() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::find_window_entity;
        let mut harness =
            floating_geometry_harness(MainOptions::default(), IRect::new(0, 0, 1024, 768));
        let entity = find_window_entity(0, harness.world());
        let world = harness.world();
        let pending = IRect::new(250, 150, 850, 450);
        world
            .entity_mut(entity)
            .insert((RepositionMarker(pending.min), ResizeMarker(pending.size())));
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        assert_eq!(
            world.get::<FullWidthMarker>(entity).unwrap().floating_frame,
            Some(pending)
        );
        assert_eq!(world.get::<ResizeMarker>(entity).unwrap().0.x, 1024);
        world.resource_mut::<Messages<Event>>().clear();
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        assert!(world.get::<FullWidthMarker>(entity).is_none());
        assert_eq!(world.get::<ResizeMarker>(entity).unwrap().0, pending.size());
        assert_eq!(
            world.get::<RepositionMarker>(entity).unwrap().0,
            pending.min
        );
    }

    #[test]
    fn maximize_preserves_invalid_floating_restore_frames_without_writes() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::find_window_entity;
        for frame in [
            IRect::new(100, 100, 100, 300),
            IRect {
                min: IVec2::new(400, 100),
                max: IVec2::new(100, 300),
            },
            IRect::new(i32::MIN, 100, i32::MAX, 300),
        ] {
            let mut harness =
                floating_geometry_harness(MainOptions::default(), IRect::new(0, 0, 1024, 768));
            let entity = find_window_entity(0, harness.world());
            let world = harness.world();
            world.entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: Some(frame),
            });
            world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
            world.run_system_once(dispatch_actions).unwrap();
            assert_eq!(
                world.get::<FullWidthMarker>(entity).unwrap().floating_frame,
                Some(frame)
            );
            assert!(world.get::<ResizeMarker>(entity).is_none());
            assert!(world.get::<RepositionMarker>(entity).is_none());
        }
    }

    #[test]
    fn native_tabs_share_width_and_restore_after_focus_changes() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(5);
        let first = find_window_entity(0, harness.world());
        let second = find_window_entity(1, harness.world());
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .convert_to_tabs(first, second)
            .unwrap();
        world.write_message(Event::action_requested(Action::Window(
            Operation::SetWidth(0.75),
        )));
        world.run_system_once(dispatch_actions).unwrap();
        for member in [first, second] {
            assert_eq!(
                world.get::<ResizeMarker>(member).unwrap().0,
                Size::new(768, 748)
            );
        }
        world.resource_mut::<Messages<Event>>().clear();
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        assert!(
            (world.get::<FullWidthMarker>(first).unwrap().width_ratio - 0.75).abs() < f64::EPSILON
        );
        for member in [first, second] {
            assert_eq!(world.get::<ResizeMarker>(member).unwrap().0.x, 1024);
        }
        world.entity_mut(first).remove::<FocusedMarker>();
        world.entity_mut(second).insert(FocusedMarker);
        world.resource_mut::<Messages<Event>>().clear();
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        for member in [first, second] {
            assert_eq!(world.get::<ResizeMarker>(member).unwrap().0.x, 768);
            assert!(world.get::<FullWidthMarker>(member).is_none());
        }
        assert!(world.get::<RepositionMarker>(second).is_some());
        assert_eq!(
            world.query::<&LayoutStrip>().single(world).unwrap().len(),
            1
        );
    }

    #[test]
    fn maximize_checks_split_width_budget_before_changing_layout() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let members = [0, 1, 2].map(|id| find_window_entity(id, harness.world()));
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .stack(members[1])
            .unwrap();
        for (member, width) in members
            .into_iter()
            .zip([i32::MAX - 1024, i32::MAX - 1024, 1])
        {
            world.entity_mut(member).insert((
                RepositionMarker(Origin::ZERO),
                ResizeMarker(Size::new(width, 300)),
            ));
        }
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        let strip = world.query::<&LayoutStrip>().single(world).unwrap();
        assert_eq!(strip.len(), 2);
        assert_eq!(
            strip.get(0).unwrap().window_iter().collect::<Vec<_>>(),
            members[..2]
        );
        assert_eq!(
            world.get::<ResizeMarker>(members[0]).unwrap().0.x,
            i32::MAX - 1024
        );
        assert!(world.get::<FullWidthMarker>(members[0]).is_none());

        world
            .entity_mut(members[1])
            .insert(ResizeMarker(Size::new(400, 300)));
        world.resource_mut::<Messages<Event>>().clear();
        world.write_message(Event::action_requested(Action::Window(Operation::Maximize)));
        world.run_system_once(dispatch_actions).unwrap();
        assert_eq!(
            world.query::<&LayoutStrip>().single(world).unwrap().len(),
            3
        );
        assert_eq!(world.get::<ResizeMarker>(members[0]).unwrap().0.x, 1024);
        assert!(world.get::<FullWidthMarker>(members[0]).is_some());
    }

    #[test]
    fn resize_rejects_the_whole_column_if_a_sibling_frame_would_overflow() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::{TestHarness, find_window_entity};
        for tabs in [false, true] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(5);
            let first = find_window_entity(0, harness.world());
            let second = find_window_entity(1, harness.world());
            let world = harness.world();
            let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
            if tabs {
                strip.convert_to_tabs(first, second).unwrap();
            } else {
                strip.stack(second).unwrap();
            }
            for (member, origin, size) in [
                (first, Origin::new(0, 20), Size::new(600, 300)),
                (second, Origin::new(i32::MAX - 100, 20), Size::new(100, 400)),
            ] {
                world.entity_mut(member).insert((
                    RepositionMarker(origin),
                    ResizeMarker(size),
                    FullWidthMarker {
                        width_ratio: 0.5,
                        floating_frame: None,
                    },
                ));
            }
            world.write_message(Event::action_requested(Action::Window(
                Operation::SetWidth(0.75),
            )));
            world.run_system_once(dispatch_actions).unwrap();
            assert_eq!(
                world.get::<ResizeMarker>(first).unwrap().0,
                Size::new(600, 300)
            );
            assert_eq!(
                world.get::<ResizeMarker>(second).unwrap().0,
                Size::new(100, 400)
            );
            assert_eq!(
                world.get::<RepositionMarker>(first).unwrap().0,
                Origin::new(0, 20)
            );
            for member in [first, second] {
                assert!(world.get::<FullWidthMarker>(member).is_some());
            }
        }
    }

    #[test]
    fn snap_uses_pending_geometry_instead_of_the_previous_desired_frame() {
        use crate::ecs::{DesiredWindowFrame, RepositionMarker, ResizeMarker};
        use crate::tests::find_window_entity;
        let mut harness =
            floating_geometry_harness(MainOptions::default(), IRect::new(0, 0, 1024, 768));
        let entity = find_window_entity(0, harness.world());
        let world = harness.world();
        world.entity_mut(entity).insert((
            DesiredWindowFrame(IRect::new(100, 100, 400, 300)),
            RepositionMarker(Origin::new(900, 100)),
            ResizeMarker(Size::new(600, 300)),
        ));
        world.write_message(Event::action_requested(Action::Window(Operation::Snap)));
        world.run_system_once(dispatch_actions).unwrap();
        assert_eq!(
            world.get::<RepositionMarker>(entity).unwrap().0,
            Origin::new(424, 100)
        );
        assert_eq!(
            world.get::<ResizeMarker>(entity).unwrap().0,
            Size::new(600, 300)
        );
    }

    #[test]
    fn geometry_commands_ignore_native_fullscreen_columns() {
        use crate::ecs::{RepositionMarker, ResizeMarker};
        use crate::tests::{TestHarness, find_window_entity};
        for operation in [
            Operation::SetWidth(0.75),
            Operation::Maximize,
            Operation::Snap,
        ] {
            let mut harness = TestHarness::new().with_windows(1);
            harness.pump_frames(5);
            let entity = find_window_entity(0, harness.world());
            let world = harness.world();
            *world
                .query::<&mut LayoutStrip>()
                .single_mut(world)
                .unwrap()
                .get_column_mut(0)
                .unwrap() = Column::Fullscren(entity);
            world.entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
            world.write_message(Event::action_requested(Action::Window(operation)));
            world.run_system_once(dispatch_actions).unwrap();
            assert!(world.get::<FullWidthMarker>(entity).is_some());
            assert_eq!(world.query::<&ResizeMarker>().iter(world).count(), 0);
            assert_eq!(world.query::<&RepositionMarker>().iter(world).count(), 0);
        }
    }

    #[test]
    fn resize_height_accumulates_on_pending_dimensions() {
        use crate::tests::{TestHarness, find_window_entity};
        for floating in [false, true] {
            let mut harness = if floating {
                floating_geometry_harness(MainOptions::default(), IRect::new(0, 0, 1024, 768))
            } else {
                let mut harness = TestHarness::new().with_windows(2);
                harness.pump_frames(5);
                let second = find_window_entity(1, harness.world());
                let world = harness.world();
                world
                    .query::<&mut LayoutStrip>()
                    .single_mut(world)
                    .unwrap()
                    .stack(second)
                    .unwrap();
                harness
            };
            let entity = find_window_entity(0, harness.world());
            let step = harness
                .world()
                .resource::<Config>()
                .floating_window_resize_step();
            harness
                .world()
                .entity_mut(entity)
                .insert(crate::ecs::ResizeMarker(Size::new(600, 300)));
            harness
                .world()
                .write_message(Event::action_requested(Action::Window(Operation::Resize {
                    axis: ResizeAxis::Height,
                    direction: ResizeDirection::Grow,
                })));
            harness.world().run_system_once(dispatch_actions).unwrap();
            assert_eq!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .unwrap()
                    .0,
                Size::new(600, 300 + step),
                "floating={floating}"
            );
        }
    }

    #[test]
    fn resize_accepts_representable_frames_near_both_coordinate_limits() {
        use crate::tests::find_window_entity;
        for x in [i32::MIN + 100, i32::MAX - 700] {
            let mut harness =
                floating_geometry_harness(MainOptions::default(), IRect::new(0, 0, 1024, 768));
            let entity = find_window_entity(0, harness.world());
            let frame = IRect::new(x, 100, x + 300, 300);
            harness.world().entity_mut(entity).insert((
                crate::ecs::Position(frame.min),
                Bounds(frame.size()),
                crate::ecs::ObservedWindowFrame(frame),
            ));
            harness
                .world()
                .write_message(Event::action_requested(Action::Window(Operation::Resize {
                    axis: ResizeAxis::Height,
                    direction: ResizeDirection::Grow,
                })));
            harness.world().run_system_once(dispatch_actions).unwrap();
            let origin = harness
                .world()
                .get::<crate::ecs::RepositionMarker>(entity)
                .unwrap()
                .0;
            let size = harness
                .world()
                .get::<crate::ecs::ResizeMarker>(entity)
                .unwrap()
                .0;
            assert_eq!(origin.x, if x < 0 { 0 } else { 724 });
            assert!(checked_window_frame(origin, size).is_some());
        }
    }

    #[test]
    fn maximize_restore_rejects_invalid_or_overflowing_widths_without_losing_the_marker() {
        use crate::tests::{TestHarness, find_window_entity};
        for ratio in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::from(i32::MAX) / 1024.0,
        ] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(5);
            let entity = find_window_entity(0, harness.world());
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: ratio,
                floating_frame: None,
            });
            harness
                .world()
                .write_message(Event::action_requested(Action::Window(Operation::Maximize)));
            harness.world().run_system_once(dispatch_actions).unwrap();
            assert!(
                harness.world().get::<FullWidthMarker>(entity).is_some(),
                "ratio={ratio}"
            );
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .is_none()
            );
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::RepositionMarker>(entity)
                    .is_none()
            );
        }
    }

    #[test]
    fn snap_rejects_an_unrepresentable_strip_translation() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        harness
            .world()
            .entity_mut(entity)
            .insert(crate::ecs::LayoutPosition(Origin::new(i32::MIN, 0)));
        let strip_entity = {
            let world = harness.world();
            world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .single(world)
                .unwrap()
        };
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Snap)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        assert!(
            harness
                .world()
                .get::<crate::ecs::RepositionMarker>(strip_entity)
                .is_none()
        );
    }
    #[test]
    fn rejected_stack_commands_preserve_restore_state_and_do_not_reshuffle() {
        use crate::tests::{TestHarness, find_window_entity};

        for (focused, fullscreen) in [(0, None), (1, Some(0)), (1, Some(1))] {
            let mut harness = TestHarness::new()
                .with_windows(3)
                .with_focused_window(focused);
            harness.pump_frames(5);
            let entity = find_window_entity(focused, harness.world());
            if let Some(index) = fullscreen {
                let world = harness.world();
                let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
                let member = strip.get(index).unwrap().top().unwrap();
                *strip.get_column_mut(index).unwrap() = Column::Fullscren(member);
            }
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
            let before = {
                let world = harness.world();
                world
                    .query::<&LayoutStrip>()
                    .single(world)
                    .unwrap()
                    .columns()
                    .map(|column| {
                        (
                            std::mem::discriminant(column),
                            column.window_iter().collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let strip_entity = {
                let world = harness.world();
                world
                    .query_filtered::<Entity, With<LayoutStrip>>()
                    .single(world)
                    .unwrap()
            };
            harness.world().clear_trackers();
            harness
                .world()
                .write_message(Event::action_requested(Action::Window(
                    Operation::ToggleStack,
                )));
            harness.world().run_system_once(dispatch_actions).unwrap();
            assert!(
                harness.world().get::<FullWidthMarker>(entity).is_some(),
                "focused={focused}, fullscreen={fullscreen:?}"
            );
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ReshuffleAroundMarker>(entity)
                    .is_none()
            );
            assert!(
                !harness
                    .world()
                    .entity(strip_entity)
                    .get_ref::<LayoutStrip>()
                    .unwrap()
                    .is_changed()
            );
            let world = harness.world();
            assert_eq!(
                world
                    .query::<&LayoutStrip>()
                    .single(world)
                    .unwrap()
                    .columns()
                    .map(|column| (
                        std::mem::discriminant(column),
                        column.window_iter().collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>(),
                before
            );
        }
    }

    #[test]
    fn rejected_tiled_height_resize_preserves_maximize_restore_state() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        harness.world().entity_mut(entity).insert(FullWidthMarker {
            width_ratio: 0.5,
            floating_frame: None,
        });
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Resize {
                axis: ResizeAxis::Height,
                direction: ResizeDirection::Grow,
            })));
        harness.world().run_system_once(dispatch_actions).unwrap();
        assert!(harness.world().get::<FullWidthMarker>(entity).is_some());
        assert!(
            harness
                .world()
                .get::<crate::ecs::ResizeMarker>(entity)
                .is_none()
        );
        assert!(
            harness
                .world()
                .get::<crate::ecs::ReshuffleAroundMarker>(entity)
                .is_none()
        );
    }

    #[test]
    fn rejected_balance_width_budget_preserves_every_column_and_restore_marker() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let entities = (0..3)
            .map(|id| find_window_entity(id, harness.world()))
            .collect::<Vec<_>>();
        for &entity in &entities {
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
        }
        harness
            .world()
            .entity_mut(entities[0])
            .insert(Bounds(Size::new(i32::MAX / 2, 500)));
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Balance)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        for entity in entities {
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .is_none()
            );
            assert!(harness.world().get::<FullWidthMarker>(entity).is_some());
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ReshuffleAroundMarker>(entity)
                    .is_none()
            );
        }
    }

    #[test]
    fn equalize_keeps_previously_queued_column_widths() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(5);
        let entities = [
            find_window_entity(0, harness.world()),
            find_window_entity(1, harness.world()),
        ];
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .stack(entities[1])
            .unwrap();
        for &entity in &entities {
            harness
                .world()
                .entity_mut(entity)
                .insert(crate::ecs::ResizeMarker(Size::new(600, 300)));
        }
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Equalize)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        for entity in entities {
            assert_eq!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .unwrap()
                    .0
                    .x,
                600
            );
        }
    }

    #[test]
    fn balance_preserves_pending_heights_and_uses_the_pending_focused_width() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let entities = (0..3)
            .map(|id| find_window_entity(id, harness.world()))
            .collect::<Vec<_>>();
        for (index, &entity) in entities.iter().enumerate() {
            harness
                .world()
                .entity_mut(entity)
                .insert(crate::ecs::ResizeMarker(Size::new(
                    if index == 0 { 600 } else { 400 },
                    250 + i32::try_from(index).unwrap(),
                )));
        }
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Balance)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        for (index, entity) in entities.into_iter().enumerate() {
            assert_eq!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .unwrap()
                    .0,
                Size::new(600, 250 + i32::try_from(index).unwrap())
            );
        }
    }

    #[test]
    fn balance_rejects_one_unrepresentable_member_without_partial_updates() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(5);
        let first = find_window_entity(0, harness.world());
        let second = find_window_entity(1, harness.world());
        harness.world().entity_mut(second).insert((
            crate::ecs::RepositionMarker(Origin::new(i32::MAX - 100, 20)),
            crate::ecs::ResizeMarker(Size::new(100, 200)),
        ));
        for entity in [first, second] {
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
        }
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Balance)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        assert!(
            harness
                .world()
                .get::<crate::ecs::ResizeMarker>(first)
                .is_none()
        );
        assert_eq!(
            harness
                .world()
                .get::<crate::ecs::ResizeMarker>(second)
                .unwrap()
                .0,
            Size::new(100, 200)
        );
        for entity in [first, second] {
            assert!(harness.world().get::<FullWidthMarker>(entity).is_some());
        }
    }

    #[test]
    fn balance_accepts_the_largest_uniform_width_that_fits_the_strip() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let entities = (0..3)
            .map(|id| find_window_entity(id, harness.world()))
            .collect::<Vec<_>>();
        let width = i32::MAX / 3;
        harness
            .world()
            .entity_mut(entities[0])
            .insert(crate::ecs::ResizeMarker(Size::new(width, 500)));
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Balance)));
        harness.world().run_system_once(dispatch_actions).unwrap();
        for entity in entities {
            assert_eq!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .unwrap()
                    .0
                    .x,
                width
            );
        }
    }

    #[test]
    fn stack_toggle_rejects_overflow_then_recovers_without_losing_restore_state() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(3).with_focused_window(1);
        harness.pump_frames(5);
        let members = [
            find_window_entity(0, harness.world()),
            find_window_entity(1, harness.world()),
        ];
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .stack(members[1])
            .unwrap();
        for &entity in &members {
            harness
                .world()
                .entity_mut(entity)
                .insert(Bounds(Size::new(i32::MAX / 2, 300)));
        }
        harness
            .world()
            .entity_mut(members[1])
            .insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(
                Operation::ToggleStack,
            )));
        harness.world().run_system_once(dispatch_actions).unwrap();
        let world = harness.world();
        assert_eq!(
            world.query::<&LayoutStrip>().single(world).unwrap().len(),
            2
        );
        assert!(world.get::<FullWidthMarker>(members[1]).is_some());
        assert!(
            world
                .get::<crate::ecs::ReshuffleAroundMarker>(members[1])
                .is_none()
        );
        for &entity in &members {
            world.entity_mut(entity).insert(Bounds(Size::new(400, 300)));
        }
        world.resource_mut::<Messages<Event>>().clear();
        world.write_message(Event::action_requested(Action::Window(
            Operation::ToggleStack,
        )));
        world.run_system_once(dispatch_actions).unwrap();
        assert_eq!(
            world.query::<&LayoutStrip>().single(world).unwrap().len(),
            3
        );
        assert!(world.get::<FullWidthMarker>(members[1]).is_none());
        assert!(
            world
                .get::<crate::ecs::ReshuffleAroundMarker>(members[1])
                .is_some()
        );
        harness.pump_frames(5);
    }

    #[test]
    fn equalize_rejects_unavailable_or_unrepresentable_members_atomically() {
        use crate::tests::{TestHarness, find_window_entity};
        for unavailable in [false, true] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(5);
            let first = find_window_entity(0, harness.world());
            let second = find_window_entity(1, harness.world());
            let world = harness.world();
            world
                .query::<&mut LayoutStrip>()
                .single_mut(world)
                .unwrap()
                .stack(second)
                .unwrap();
            world
                .entity_mut(first)
                .insert(crate::ecs::ResizeMarker(Size::new(600, 300)));
            world.entity_mut(second).insert((
                crate::ecs::RepositionMarker(Origin::new(0, i32::MAX - 300)),
                crate::ecs::ResizeMarker(Size::new(600, 100)),
            ));
            if unavailable {
                world.entity_mut(second).remove::<Bounds>();
            }
            world.write_message(Event::action_requested(Action::Window(Operation::Equalize)));
            world.run_system_once(dispatch_actions).unwrap();
            assert_eq!(
                world.get::<crate::ecs::ResizeMarker>(first).unwrap().0,
                Size::new(600, 300)
            );
            assert_eq!(
                world.get::<crate::ecs::ResizeMarker>(second).unwrap().0,
                Size::new(600, 100)
            );
        }
    }

    fn floating_geometry_harness(
        options: crate::config::MainOptions,
        display: IRect,
    ) -> crate::tests::TestHarness {
        use crate::config::WindowParams;
        use crate::tests::{TEST_DISPLAY_ID, TEST_WORKSPACE_ID, TestHarness};

        let mut floating = WindowParams::new(".*", None);
        floating.floating = Some(true);
        let mut harness = TestHarness::new()
            .with_config((options, vec![floating]).into())
            .with_display(TEST_DISPLAY_ID, display, vec![TEST_WORKSPACE_ID])
            .with_window(0, |window| {
                window.frame = IRect::from_corners(
                    display.min + IVec2::new(100, 100),
                    display.min + IVec2::new(400, 300),
                );
            })
            .with_focused_window(0);
        harness.pump_frames(5);
        harness
    }

    #[test]
    fn floating_resize_large_step_clamps_without_overflow() {
        for (axis, direction, expected) in [
            (
                ResizeAxis::Width,
                ResizeDirection::Grow,
                IVec2::new(1024, 200),
            ),
            (
                ResizeAxis::Height,
                ResizeDirection::Grow,
                IVec2::new(300, 748),
            ),
            (
                ResizeAxis::Width,
                ResizeDirection::Shrink,
                IVec2::new(100, 200),
            ),
            (
                ResizeAxis::Height,
                ResizeDirection::Shrink,
                IVec2::new(300, 100),
            ),
        ] {
            let options = crate::config::MainOptions {
                floating_window_resize_step: Some(i32::MAX),
                ..Default::default()
            };
            let mut harness = floating_geometry_harness(options, IRect::new(0, 0, 1024, 768));
            harness
                .world()
                .write_message(crate::events::Event::ActionRequested {
                    action: Action::Window(Operation::Resize { axis, direction }),
                });
            harness.pump_frames(5);
            assert_eq!(
                harness.mock_state.actual_window_frame(0).unwrap().size(),
                expected,
                "{axis:?} {direction:?} must clamp to the usable viewport"
            );
        }
    }

    #[test]
    fn floating_resize_fits_a_viewport_smaller_than_the_minimum_size() {
        for (axis, options, expected) in [
            (
                ResizeAxis::Width,
                crate::config::MainOptions {
                    padding_right: Some(974),
                    ..Default::default()
                },
                IVec2::new(50, 200),
            ),
            (
                ResizeAxis::Height,
                crate::config::MainOptions {
                    padding_bottom: Some(698),
                    ..Default::default()
                },
                IVec2::new(300, 50),
            ),
        ] {
            for direction in [ResizeDirection::Grow, ResizeDirection::Shrink] {
                let mut harness =
                    floating_geometry_harness(options.clone(), IRect::new(0, 0, 1024, 768));
                harness
                    .world()
                    .write_message(crate::events::Event::ActionRequested {
                        action: Action::Window(Operation::Resize { axis, direction }),
                    });
                harness.pump_frames(5);
                assert_eq!(
                    harness.mock_state.actual_window_frame(0).unwrap().size(),
                    expected,
                    "{axis:?} {direction:?} must fit the positive usable dimension"
                );
            }
        }
    }

    #[test]
    fn floating_move_large_step_clamps_on_positive_and_negative_displays() {
        for display_origin in [IVec2::ZERO, IVec2::new(-1024, -768)] {
            for (direction, offset) in [
                (Direction::West, IVec2::new(0, 100)),
                (Direction::East, IVec2::new(724, 100)),
                (Direction::North, IVec2::new(100, 20)),
                (Direction::South, IVec2::new(100, 568)),
            ] {
                let options = crate::config::MainOptions {
                    floating_window_move_step: Some(i32::MAX),
                    ..Default::default()
                };
                let display =
                    IRect::from_corners(display_origin, display_origin + IVec2::new(1024, 768));
                let mut harness = floating_geometry_harness(options, display);
                harness
                    .world()
                    .write_message(crate::events::Event::ActionRequested {
                        action: Action::Window(Operation::Move(direction.clone())),
                    });
                harness.pump_frames(5);
                let frame = harness.mock_state.actual_window_frame(0).unwrap();
                assert_eq!(frame.min, display_origin + offset, "{direction:?}");
                assert_eq!(frame.size(), IVec2::new(300, 200));
            }
        }
    }

    #[test]
    fn floating_geometry_commands_preserve_state_without_a_usable_viewport() {
        for (right, bottom) in [(1024, 0), (1025, 0), (0, 748), (0, 749)] {
            for operation in [
                Operation::Move(Direction::East),
                Operation::Center,
                Operation::Maximize,
                Operation::Resize {
                    axis: ResizeAxis::Width,
                    direction: ResizeDirection::Grow,
                },
                Operation::Resize {
                    axis: ResizeAxis::Height,
                    direction: ResizeDirection::Shrink,
                },
                Operation::SetWidth(0.5),
            ] {
                let options = crate::config::MainOptions {
                    padding_right: Some(right),
                    padding_bottom: Some(bottom),
                    ..Default::default()
                };
                let mut harness = floating_geometry_harness(options, IRect::new(0, 0, 1024, 768));
                let before = harness.mock_state.actual_window_frame(0).unwrap();
                let writes = harness.mock_state.frame_write_attempts(0);
                let entity = crate::tests::find_window_entity(0, harness.world());
                harness.world().entity_mut(entity).insert(FullWidthMarker {
                    width_ratio: 0.5,
                    floating_frame: Some(before),
                });
                harness
                    .world()
                    .write_message(crate::events::Event::ActionRequested {
                        action: Action::Window(operation.clone()),
                    });
                harness.pump_frames(5);
                assert_eq!(
                    harness.mock_state.actual_window_frame(0),
                    Some(before),
                    "{operation:?}"
                );
                assert_eq!(
                    harness.mock_state.frame_write_attempts(0),
                    writes,
                    "{operation:?}"
                );
                assert!(
                    harness.world().get::<FullWidthMarker>(entity).is_some(),
                    "{operation:?}"
                );
            }
        }
    }

    #[test]
    fn unrepresentable_column_width_preserves_frames_and_maximize_state() {
        use crate::tests::{TestHarness, find_window_entity};

        let ratio = f64::from(i32::MAX) / 1024.0;
        for operation in [
            Operation::SetWidth(ratio),
            Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            },
        ] {
            let options = crate::config::MainOptions {
                preset_column_widths: vec![ratio],
                ..Default::default()
            };
            let mut harness = TestHarness::new()
                .with_config((options, vec![]).into())
                .with_windows(3);
            harness.pump_frames(5);
            let entity = find_window_entity(0, harness.world());
            let frame = harness.mock_state.actual_window_frame(0).unwrap();
            let writes = harness.mock_state.frame_write_attempts(0);
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
            harness
                .world()
                .write_message(crate::events::Event::action_requested(Action::Window(
                    operation,
                )));
            harness.pump_frames(5);
            assert_eq!(harness.mock_state.actual_window_frame(0), Some(frame));
            assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
            assert!(harness.world().get::<FullWidthMarker>(entity).is_some());
        }
    }

    #[test]
    fn invalid_width_ratio_preserves_floating_geometry_and_maximize_state() {
        for ratio in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::MAX,
        ] {
            let mut harness = floating_geometry_harness(
                crate::config::MainOptions::default(),
                IRect::new(0, 0, 1024, 768),
            );
            let before = harness.mock_state.actual_window_frame(0).unwrap();
            let writes = harness.mock_state.frame_write_attempts(0);
            let entity = crate::tests::find_window_entity(0, harness.world());
            harness.world().entity_mut(entity).insert(FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: Some(before),
            });
            harness
                .world()
                .write_message(crate::events::Event::ActionRequested {
                    action: Action::Window(Operation::SetWidth(ratio)),
                });
            harness.pump_frames(5);
            assert_eq!(
                harness.mock_state.actual_window_frame(0),
                Some(before),
                "{ratio}"
            );
            assert_eq!(
                harness.mock_state.frame_write_attempts(0),
                writes,
                "{ratio}"
            );
            assert!(harness.world().get::<FullWidthMarker>(entity).is_some());
        }
    }

    #[test]
    fn system_overview_actions_reach_the_platform_once_per_request_even_after_failure() {
        use crate::events::Event;
        use crate::manager::MockWindowManagerApi;
        use crate::platform::mission_control::SystemOverview;
        use std::sync::{Arc, Mutex};

        let received = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&received);
        let mut manager = MockWindowManagerApi::new();
        manager
            .expect_perform_system_overview()
            .times(3)
            .returning(move |overview| {
                recorded.lock().unwrap().push(overview);
                if overview == SystemOverview::ShowDesktop {
                    Err(crate::errors::Error::Generic(
                        "mock launch failure".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            });
        let mut app = App::new();
        app.add_message::<Event>()
            .insert_resource(WindowManager(Box::new(manager)))
            .add_systems(PreUpdate, dispatch_actions);
        for action in [
            Action::MissionControl,
            Action::ShowDesktop,
            Action::MissionControl,
        ] {
            app.world_mut()
                .write_message(Event::ActionRequested { action });
        }
        app.world_mut().write_message(Event::MissionControlExit);
        app.update();
        app.update();
        assert_eq!(
            *received.lock().unwrap(),
            vec![
                SystemOverview::MissionControl,
                SystemOverview::ShowDesktop,
                SystemOverview::MissionControl,
            ]
        );
    }

    fn setup_world_with_layout() -> (World, LayoutStrip, Vec<Entity>) {
        let mut world = World::new();
        // e0, e1 are stacked, e2 is single, e3 is single
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]); // This will become a stack
        strip.append(entities[1]);
        strip.append(entities[2]);
        strip.append(entities[3]);
        strip.stack(entities[1]).unwrap(); // Stack e1 onto e0

        (world, strip, entities)
    }

    #[test]
    fn test_get_window_in_direction_simple() {
        let (_world, strip, entities) = setup_world_with_layout();
        let e0 = entities[0];
        let e2 = entities[2];
        let e3 = entities[3];
        let east = Direction::East;
        let west = Direction::West;

        // From e2, east should be e3, west should be e0 (top of stack)
        assert_eq!(get_window_in_direction(&east, e2, &strip), Some(e3));
        assert_eq!(get_window_in_direction(&west, e2, &strip), Some(e0));

        // From e3, west is e2, east is None
        assert_eq!(get_window_in_direction(&west, e3, &strip), Some(e2));
        assert_eq!(get_window_in_direction(&east, e3, &strip), None);

        // From e0, east is e2, west is None
        assert_eq!(get_window_in_direction(&east, e0, &strip), Some(e2));
        assert_eq!(get_window_in_direction(&west, e0, &strip), None);
    }

    #[test]
    fn test_get_window_in_direction_stacked() {
        let (_world, strip, entities) = setup_world_with_layout();
        let e0 = entities[0];
        let e1 = entities[1];
        let north = Direction::North;
        let south = Direction::South;

        // From e0 (top of stack), south should be e1, north is None
        assert_eq!(get_window_in_direction(&south, e0, &strip), Some(e1));
        assert_eq!(get_window_in_direction(&north, e0, &strip), None);

        // From e1 (bottom of stack), north should be e0, south is None
        assert_eq!(get_window_in_direction(&north, e1, &strip), Some(e0));
        assert_eq!(get_window_in_direction(&south, e1, &strip), None);
    }

    #[test]
    fn test_get_window_in_direction_adjacent_stacks() {
        // Layout: [Stack(e0, e1), Stack(e2, e3)]
        let mut world = World::new();
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]);
        strip.append(entities[1]);
        strip.append(entities[2]);
        strip.append(entities[3]);
        strip.stack(entities[1]).unwrap(); // Stack e1 onto e0: [Stack(e0, e1), e2, e3]
        strip.stack(entities[3]).unwrap(); // Stack e3 onto e2: [Stack(e0, e1), Stack(e2, e3)]

        let east = Direction::East;
        let west = Direction::West;

        // From e0 (top-left), east should go to e2 (top-right)
        assert_eq!(
            get_window_in_direction(&east, entities[0], &strip),
            Some(entities[2])
        );
        // From e1 (bottom-left), east should go to e3 (bottom-right)
        assert_eq!(
            get_window_in_direction(&east, entities[1], &strip),
            Some(entities[3])
        );
        // From e2 (top-right), west should go to e0 (top-left)
        assert_eq!(
            get_window_in_direction(&west, entities[2], &strip),
            Some(entities[0])
        );
        // From e3 (bottom-right), west should go to e1 (bottom-left)
        assert_eq!(
            get_window_in_direction(&west, entities[3], &strip),
            Some(entities[1])
        );
    }

    #[test]
    fn pick_nearest_in_direction_east_picks_closer() {
        let mut world = World::new();
        let near = world.spawn(()).id();
        let far = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![
            (near, bevy::math::IVec2::new(10, 0)),
            (far, bevy::math::IVec2::new(50, 0)),
        ];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            Some(near),
        );
    }

    #[test]
    fn pick_nearest_in_direction_respects_cone() {
        let mut world = World::new();
        let candidate = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        // y/x ratio > 1 → outside the 45° east cone.
        let candidates = vec![(candidate, bevy::math::IVec2::new(10, 20))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            None,
        );
    }

    #[test]
    fn pick_nearest_in_direction_ignores_wrong_side() {
        let mut world = World::new();
        let west_one = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![(west_one, bevy::math::IVec2::new(-10, 0))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            None,
        );
    }

    #[test]
    fn pick_nearest_in_direction_first_last_return_none() {
        let mut world = World::new();
        let any = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![(any, bevy::math::IVec2::new(10, 0))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::First, focused, candidates.clone()),
            None,
        );
        assert_eq!(
            pick_nearest_in_direction(&Direction::Last, focused, candidates),
            None,
        );
    }
}

//! Ordered execution returns admission, never an enqueue acknowledgement.

use bevy::prelude::*;
use spool_shared_types::commands::SpaceLayoutOperation;

use super::{Action, DisplayTarget, Operation};

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(super) fn execute(world: &mut World, action: Action) -> crate::errors::Result<()> {
    let lifecycle = world.resource::<crate::lifecycle::Lifecycle>();
    if let Some(reason) = lifecycle.rejection() {
        return Err(crate::errors::Error::rejected(reason));
    }
    if matches!(action, Action::Quit | Action::Restart) {
        // The IPC adapter attempts the receipt before starting shutdown.
        lifecycle.set(crate::lifecycle::Phase::Stopping);
        return Ok(());
    }
    let action = world
        .run_system_cached_with(resolve_default, action)
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("execution_unavailable", error)
        })??;
    if let Some(result) = world
        .run_system_cached_with(super::column_width::execute, action.clone())
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("execution_unavailable", error)
        })?
    {
        return result;
    }
    if let Some(result) = world
        .run_system_cached_with(super::layout_edit::execute, action.clone())
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("execution_unavailable", error)
        })?
    {
        return result;
    }
    if matches!(
        action,
        Action::TargetedWindow { .. } | Action::Window(_) | Action::SpaceLayout { .. }
    ) && (world.contains_resource::<crate::ecs::Initializing>()
        || world
            .get_resource::<crate::ecs::MissionControlActive>()
            .is_some_and(|overview| overview.0)
        || world.contains_resource::<crate::ecs::exit_restore::ExitInProgress>())
    {
        return Err(crate::errors::Error::rejected("session_not_writable"));
    }
    if let Action::Window(
        operation @ (Operation::FocusFloating | Operation::FocusTiled | Operation::FocusOtherLayer),
    ) = &action
    {
        world
            .run_system_cached_with(validate_layer_target, operation.clone())
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })??;
    }
    match action {
        Action::Window(operation @ (Operation::Focus(_) | Operation::FocusStep(_))) => world
            .run_system_cached_with(super::command_move_focus, operation)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        Action::Window(Operation::FocusFloating) => world
            .run_system_cached(super::command_focus_floating)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::Window(Operation::FocusTiled) => world
            .run_system_cached(super::command_focus_tiled)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::Window(Operation::FocusOtherLayer) => world
            .run_system_cached(super::command_focus_other_layer)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::Mouse(super::MouseMove::ToNextDisplay) => world
            .run_system_cached_with(
                super::checked_focus_other_display,
                (None, DisplayTarget::Next),
            )
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        Action::ToggleBarCollapse => world
            .run_system_cached(super::command_toggle_bar_collapse)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::PrintState => world
            .run_system_cached(super::print_internal_state_handler)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::ReconcileWindows => world
            .run_system_cached(super::reconcile_windows_handler)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            }),
        Action::MissionControl => crate::platform::mission_control::SystemOverview::MissionControl
            .launch()
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("native_operation_rejected", error)
            }),
        Action::ShowDesktop => crate::platform::mission_control::SystemOverview::ShowDesktop
            .launch()
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("native_operation_rejected", error)
            }),
        action @ (Action::FocusWindow { .. }
        | Action::FocusWindowInSpace { .. }
        | Action::FocusSpace { .. }
        | Action::CreateSpace { .. }
        | Action::DeleteSpace { .. }
        | Action::MoveWindowToSpace { .. }
        | Action::MoveColumnToSpace { .. }) => world
            .run_system_cached_with(
                crate::ecs::native_space::execute_native_space_command,
                crate::events::Event::action_requested(action),
            )
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        Action::TargetedWindow {
            window_id,
            operation: Operation::ToNextDisplay(move_focus),
        } => world
            .run_system_cached_with(
                super::transfer::execute,
                (window_id, move_focus, DisplayTarget::Next),
            )
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        Action::TargetedWindow {
            window_id,
            operation: Operation::Move(direction),
        } => {
            let destination = world
                .run_system_cached_with(move_destination, (window_id, direction.clone()))
                .map_err(|error| {
                    crate::errors::Error::rejection_with_cause("execution_unavailable", error)
                })?;
            if let Some(destination) = destination {
                world
                    .run_system_cached_with(
                        super::transfer::execute,
                        (window_id, super::MoveFocus::Follow, destination),
                    )
                    .map_err(|error| {
                        crate::errors::Error::rejection_with_cause("execution_unavailable", error)
                    })?
            } else {
                world
                    .run_system_cached_with(
                        super::targeted::execute,
                        Action::TargetedWindow {
                            window_id,
                            operation: Operation::Move(direction),
                        },
                    )
                    .map_err(|error| {
                        crate::errors::Error::rejection_with_cause("execution_unavailable", error)
                    })?
            }
        }
        action @ Action::TargetedWindow { .. } => world
            .run_system_cached_with(super::targeted::execute, action)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        Action::SpaceLayout {
            space_id,
            operation: SpaceLayoutOperation::ToggleTiledVisibility,
        } => world
            .run_system_cached_with(crate::ecs::tiled_visibility::toggle, space_id)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("execution_unavailable", error)
            })?,
        _ => Err(crate::errors::Error::rejected("unsupported_operation")),
    }
}

fn move_destination(
    In((window_id, direction)): In<(i32, super::Direction)>,
    windows: crate::ecs::params::Windows,
    strips: Query<&crate::ecs::layout::LayoutStrip>,
) -> Option<DisplayTarget> {
    let (_, entity) = windows.find(window_id)?;
    let (_, _, state) = windows.get_tracked(entity)?;
    if !state.is_tiled() {
        return None;
    }
    let strip = strips.iter().find(|strip| strip.contains(entity))?;
    if super::get_window_in_direction(&direction, entity, strip).is_some() {
        return None;
    }
    DisplayTarget::from_direction(&direction)
}

fn resolve_default(
    In(action): In<Action>,
    windows: crate::ecs::params::Windows,
    focus: Res<crate::ecs::focus::FocusCoordinator>,
) -> crate::errors::Result<Action> {
    let focused_id = || {
        super::command_entity(&windows, &focus)
            .and_then(|entity| windows.get(entity))
            .map(|window| window.id())
            .ok_or_else(|| crate::errors::Error::rejected("no_focused_window"))
    };
    Ok(match action {
        Action::MoveFocusedWindowToSpace {
            space_id,
            move_focus,
        } => Action::MoveWindowToSpace {
            window_id: focused_id()?,
            space_id,
            move_focus,
        },
        Action::Window(Operation::Equalize) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::Equalize { column: None },
        },
        Action::Window(Operation::Balance) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::Balance {
                reference_column: None,
            },
        },
        Action::Window(Operation::ToggleTiledVisibility) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::ToggleTiledVisibility,
        },
        Action::Window(
            operation @ (Operation::Center
            | Operation::Move(_)
            | Operation::Resize { .. }
            | Operation::SetWidth(_)
            | Operation::Maximize
            | Operation::Snap
            | Operation::ToggleFloating
            | Operation::ToggleStack
            | Operation::ToNextDisplay(_)),
        ) => Action::TargetedWindow {
            window_id: focused_id()?,
            operation,
        },
        action => action,
    })
}

fn validate_layer_target(
    In(operation): In<Operation>,
    windows: crate::ecs::params::Windows,
    active: crate::ecs::params::ActiveDisplay,
    focus: Res<crate::ecs::focus::FocusCoordinator>,
    manager: Res<crate::manager::WindowManager>,
    mut topology: ResMut<crate::ecs::topology::NativeTopology>,
) -> crate::errors::Result<()> {
    let floating = match operation {
        Operation::FocusFloating => true,
        Operation::FocusTiled => false,
        _ => !super::command_entity(&windows, &focus)
            .and_then(|entity| windows.get_tracked(entity))
            .is_some_and(|(_, _, state)| state.is_floating()),
    };
    let target = if floating {
        let candidates = super::visible_floating_entities(
            &windows,
            &manager,
            active.active_strip().id(),
            active.bounds(),
        );
        focus
            .last_floating(active.active_strip().id())
            .filter(|entity| candidates.contains(entity))
            .or_else(|| candidates.into_iter().next())
    } else {
        let strip = windows.navigable_strip(active.active_strip());
        focus
            .last_tiled(strip.id())
            .filter(|entity| strip.contains(*entity))
            .or_else(|| strip.all_columns().into_iter().next())
    }
    .ok_or_else(|| crate::errors::Error::rejected("focus_target_not_found"))?;
    let window = windows
        .get(target)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    if !windows.layout_is_writable(target)
        || topology.observe_visible_window_space(&manager, window.id())
            != Some(active.active_strip().id())
    {
        return Err(crate::errors::Error::rejected("focus_target_unavailable"));
    }
    Ok(())
}

//! Replays a script's [`LayoutOp`]s (returned by a Lua handler that
//! transformed a `WindowSet`) against the live world.
//!
//! Unlike other command handlers, each op names a window explicitly rather
//! than acting on the focused one. Because the snapshot a script transformed
//! may be a frame stale, each op is applied independently and best-effort: one
//! that can't be resolved is logged at debug and skipped without affecting
//! the rest.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::Without;
use bevy::ecs::system::{Commands, In, Query, Res};
use spool_shared_types::windowset::{LayoutOp, LayoutPlan, LayoutSnapshot};
use std::sync::Arc;
use tracing::debug;

use crate::ecs::layout::{LayoutStrip, WidthIntent};
use crate::ecs::layout_snapshot::{LayoutSession, matches_window};
use crate::ecs::params::Windows;
use crate::ecs::window_frame::checked_window_frame;
use crate::ecs::{Floating, RetileWindow, SpawnCommandsExt, Window};
use crate::events::Event;
use crate::manager::{Origin, Size};

/// Applies the layout operations a Lua handler returned.
pub(crate) fn apply_layout_plan(In(plan): In<LayoutPlan>, world: &mut bevy::prelude::World) {
    let Ok(initial) = world.run_system_cached(capture_column_bindings) else {
        return;
    };
    let Ok(initial_structures) = world.run_system_cached(capture_structure_bindings) else {
        return;
    };
    let stale_structures = plan
        .snapshot
        .windows
        .keys()
        .filter(|window| initial_structures.get(window) != plan.snapshot.structures.get(window))
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let stale = plan
        .snapshot
        .columns
        .iter()
        .filter_map(|(window, binding)| (initial.get(window) != Some(binding)).then_some(*window))
        .collect::<std::collections::HashSet<_>>();
    let mut snapshot = (*plan.snapshot).clone();
    // This synchronous transaction does not yield to native events or Lua.
    // Subsequent operations may address columns created by earlier operations
    // in this plan, while an already-stale incoming binding stays rejected.
    for op in plan.ops {
        if matches!(
            op,
            LayoutOp::Swap(..) | LayoutOp::Stack { .. } | LayoutOp::Unstack(_)
        ) && op
            .targets()
            .any(|window| stale_structures.contains(&window))
        {
            continue;
        }
        if let LayoutOp::SetWidth { window, .. } = op
            && (stale.contains(&window) || !plan.snapshot.columns.contains_key(&window))
        {
            continue;
        }
        if let Err(error) =
            world.run_system_cached_with(apply_layout_op, (op, Arc::new(snapshot.clone())))
        {
            debug!(%error, "layout operation unavailable");
        }
        if let Ok(columns) = world.run_system_cached(capture_column_bindings) {
            snapshot.columns = columns;
        }
        if let Ok(structures) = world.run_system_cached(capture_structure_bindings) {
            snapshot.structures = structures;
        }
    }
}

fn capture_column_bindings(
    windows: Windows,
    strips: Query<&LayoutStrip>,
) -> std::collections::BTreeMap<crate::platform::WinID, (u64, u64)> {
    let windows = &windows;
    strips
        .iter()
        .flat_map(|strip| {
            strip
                .columns()
                .enumerate()
                .flat_map(move |(index, column)| {
                    let state = strip.column_state(index);
                    column.window_iter().filter_map(move |entity| {
                        Some((
                            windows.get_any(entity)?.id(),
                            (state?.id.0, state?.intent_revision),
                        ))
                    })
                })
        })
        .collect()
}

fn capture_structure_bindings(
    windows: Windows,
    strips: Query<&LayoutStrip>,
) -> std::collections::BTreeMap<crate::platform::WinID, (u64, u64)> {
    strips
        .iter()
        .flat_map(|strip| {
            strip
                .columns()
                .flat_map(|column| column.window_iter())
                .filter_map(|entity| {
                    Some((
                        windows.get_any(entity)?.id(),
                        (strip.id(), strip.structure_revision()),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "ordered identity and domain admission checks precede one operation"
)]
fn apply_layout_op(
    In((op, snapshot)): In<(LayoutOp, Arc<LayoutSnapshot>)>,
    windows: Windows,
    mut workspaces: Query<&mut LayoutStrip, Without<Window>>,
    session: Res<LayoutSession>,
    topology: Res<super::topology::NativeTopology>,
    mut commands: Commands,
) {
    if !session.accepts(&snapshot, op, &windows) {
        debug!(target: "spool::lua", "skipping {op:?}: snapshot identity is no longer current");
        return;
    }
    if let LayoutOp::SetWidth { window, .. } = op {
        let current = windows.find_any(window).and_then(|(_, entity)| {
            workspaces.iter().find_map(|strip| {
                let state = strip.column_state(strip.index_of(entity).ok()?)?;
                Some((state.id.0, state.intent_revision))
            })
        });
        let cohort_valid = windows.find_any(window).is_some_and(|(_, entity)| {
            workspaces
                .iter()
                .find(|strip| strip.contains(entity))
                .is_some_and(|strip| {
                    strip.column_containing(entity).is_some_and(|column| {
                        column.window_iter().all(|member| {
                            !windows.layout_transition_pending(member)
                                && windows.get_any(member).is_some_and(|window| {
                                    matches_window(&snapshot, window.id(), &windows)
                                })
                        })
                    })
                })
        });
        if !cohort_valid || current.is_none() || snapshot.columns.get(&window).copied() != current {
            debug!(target: "spool::lua", "rejecting stale column width edit");
            return;
        }
    }
    if matches!(op, LayoutOp::SetFrame { .. })
        && op.targets().any(|id| {
            windows
                .find(id)
                .is_none_or(|(_, entity)| !windows.layout_is_writable(entity))
        })
    {
        return;
    }
    if matches!(
        op,
        LayoutOp::Swap(..) | LayoutOp::Stack { .. } | LayoutOp::Unstack(_)
    ) {
        for target in op.targets() {
            let Some((_, entity)) = windows.find_any(target) else {
                return;
            };
            let Some(strip) = workspaces.iter().find(|strip| strip.contains(entity)) else {
                return;
            };
            if snapshot.structures.get(&target) != Some(&(strip.id(), strip.structure_revision())) {
                return;
            }
            let Some(column) = strip.column_containing(entity) else {
                return;
            };
            if column.window_iter().any(|member| {
                windows.layout_transition_pending(member)
                    || windows
                        .get_any(member)
                        .is_none_or(|window| !matches_window(&snapshot, window.id(), &windows))
            }) {
                return;
            }
        }
    }
    if matches!(
        op,
        LayoutOp::Focus(_) | LayoutOp::MoveToWorkspace { .. } | LayoutOp::View { .. }
    ) {
        commands.run_system_cached_with(
            super::native_space::apply_native_space_command,
            Event::LayoutSpaceRequested { op, snapshot },
        );
        return;
    }
    apply(op, &windows, &mut workspaces, &topology, &mut commands);
}

/// Applies one op, or explains why it could not be.
#[allow(clippy::too_many_lines)]
fn apply(
    op: LayoutOp,
    windows: &Windows,
    workspaces: &mut Query<&mut LayoutStrip, Without<Window>>,
    topology: &super::topology::NativeTopology,
    commands: &mut Commands,
) {
    // Resolve up front so every arm below can assume the window still exists.
    let entity = if let Some(window_id) = op.target() {
        let resolved = if matches!(
            op,
            LayoutOp::SetWidth { .. }
                | LayoutOp::Swap(..)
                | LayoutOp::Stack { .. }
                | LayoutOp::Unstack(_)
        ) {
            windows.find_any(window_id)
        } else {
            windows.find(window_id)
        };
        let Some((_, entity)) = resolved else {
            debug!(
                target: "spool::lua",
                "skipping {op:?}: window {window_id} is gone since the handler ran"
            );
            return;
        };
        Some(entity)
    } else {
        None
    };

    match op {
        LayoutOp::Swap(_, other) => {
            let entity = entity.expect("Swap names a window");
            let Some((_, other_entity)) = windows.find_any(other) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {other} is gone");
                return;
            };
            if is_floating(entity, windows) || is_floating(other_entity, windows) {
                return;
            }
            let Some(mut strip) = workspaces
                .iter_mut()
                .find(|strip| strip.contains(entity) && strip.contains(other_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            match strip.swap_items(entity, other_entity) {
                Ok(true) => {
                    if topology.visible_display_for_space(strip.id()).is_some() {
                        commands.reshuffle_around(entity);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    debug!(target: "spool::lua", %error, "skipping {op:?}: the layout refused the swap");
                }
            }
        }

        LayoutOp::Focus(_) | LayoutOp::MoveToWorkspace { .. } | LayoutOp::View { .. } => {
            unreachable!("deferred with snapshot binding")
        }

        LayoutOp::SetFloating { floating, .. } => {
            let entity = entity.expect("SetFloating names a window");
            set_floating(entity, floating, windows, commands);
        }

        LayoutOp::SetWidth { ratio, .. } => {
            let entity = entity.expect("SetWidth names a window");
            if is_floating(entity, windows) {
                return;
            }
            let Some(mut strip) = workspaces.iter_mut().find(|strip| strip.contains(entity)) else {
                return;
            };
            let Some(id) = strip.column_id(entity) else {
                return;
            };
            let mut proposed = strip.clone();
            match proposed.set_width_intent(id, WidthIntent::ViewportRatio(ratio)) {
                Ok(_) if proposed.width_budget_is_valid() => *strip = proposed,
                Ok(_) => debug!(target: "spool::lua", "column width offsets overflow"),
                Err(error) => debug!(target: "spool::lua", %error, "invalid column width intent"),
            }
        }

        LayoutOp::SetFrame { frame, .. } => {
            let entity = entity.expect("SetFrame names a window");
            // The layout engine owns a tiled window's geometry and will move
            // it back, so warn if the target isn't floated.
            if !is_floating(entity, windows) {
                debug!(target: "spool::lua", "rejecting frame edit for tiled window");
                return;
            }
            let origin = Origin::new(frame.x, frame.y);
            let size = Size::new(frame.width.max(1), frame.height.max(1));
            if checked_window_frame(origin, size).is_none() {
                debug!(target: "spool::lua", "skipping {op:?}: the frame endpoints overflow");
                return;
            }
            commands.reposition_entity(entity, origin);
            commands.resize_entity(entity, size);
        }

        LayoutOp::Stack { onto, .. } => {
            let entity = entity.expect("Stack names a window");
            let Some((_, onto_entity)) = windows.find_any(onto) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {onto} is gone");
                return;
            };
            if is_floating(entity, windows) || is_floating(onto_entity, windows) {
                debug!(target: "spool::lua", "skipping {op:?}: a floating window has no stack slot");
                return;
            }
            let Some(mut strip) = workspaces
                .iter_mut()
                .find(|strip| strip.contains(entity) && strip.contains(onto_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            match strip.stack_onto(entity, onto_entity) {
                Ok(true) => {
                    if topology.visible_display_for_space(strip.id()).is_some() {
                        commands.reshuffle_around(entity);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    debug!(target: "spool::lua", %error, "skipping {op:?}: the layout refused the stack");
                }
            }
        }

        LayoutOp::Unstack(_) => {
            let entity = entity.expect("Unstack names a window");
            if is_floating(entity, windows) {
                return;
            }
            let Some(mut strip) = workspaces.iter_mut().find(|strip| strip.contains(entity)) else {
                return;
            };
            let mut proposed = strip.clone();
            match proposed.unstack(entity) {
                Ok(true) => {
                    if !proposed.width_budget_is_valid() {
                        debug!(target: "spool::lua", "skipping {op:?}: unstacked column offsets overflow");
                        return;
                    }
                    *strip = proposed;
                    if topology.visible_display_for_space(strip.id()).is_some() {
                        commands.reshuffle_around(entity);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    debug!(target: "spool::lua", %error, "skipping {op:?}: the window is not in a stack");
                }
            }
        }
    }
}

fn is_floating(entity: Entity, windows: &Windows) -> bool {
    windows
        .get_tracked(entity)
        .is_some_and(|(_, _, state)| state.is_floating())
}

/// Shares the normal retile observer's native membership and capability checks.
fn set_floating(entity: Entity, floating: bool, windows: &Windows, commands: &mut Commands) {
    if floating {
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert(Floating);
        }
    } else if is_floating(entity, windows) {
        commands.trigger(RetileWindow(entity));
    }
}

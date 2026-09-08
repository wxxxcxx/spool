//! Replays a script's [`LayoutOp`]s (returned by a Lua handler that
//! transformed a `WindowSet`) against the live world.
//!
//! Unlike other command handlers, each op names a window explicitly rather
//! than acting on the focused one. Because the snapshot a script transformed
//! may be a frame stale, each op is applied independently and best-effort: one
//! that can't be resolved is logged at debug and skipped without affecting
//! the rest.

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::Without;
use bevy::ecs::system::{Commands, In, Query, Res};
use bevy::math::IRect;
use spool_shared_types::windowset::{LayoutOp, LayoutPlan, LayoutSnapshot};
use std::sync::Arc;
use tracing::debug;

use crate::config::Config;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::layout_snapshot::{LayoutSession, matches_window};
use crate::ecs::params::Windows;
use crate::ecs::window_frame::checked_window_frame;
use crate::ecs::{DockPosition, Floating, FullWidthMarker, RetileWindow, SpawnCommandsExt, Window};
use crate::events::Event;
use crate::manager::{Display, Origin, Size};

/// Applies the layout operations a Lua handler returned.
pub(crate) fn apply_layout_plan(In(plan): In<LayoutPlan>, mut commands: Commands) {
    // Each cached run flushes its observers before the next operation.
    for op in &plan.ops {
        commands.run_system_cached_with(apply_layout_op, (*op, Arc::clone(&plan.snapshot)));
    }
}

fn apply_layout_op(
    In((op, snapshot)): In<(LayoutOp, Arc<LayoutSnapshot>)>,
    windows: Windows,
    mut workspaces: Query<(&mut LayoutStrip, &ChildOf), Without<Window>>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    config: Res<Config>,
    session: Res<LayoutSession>,
    mut commands: Commands,
) {
    if !session.accepts(&snapshot, op, &windows) {
        debug!(target: "spool::lua", "skipping {op:?}: snapshot identity is no longer current");
        return;
    }
    if matches!(
        op,
        LayoutOp::Swap(..)
            | LayoutOp::Stack { .. }
            | LayoutOp::Unstack(_)
            | LayoutOp::SetWidth { .. }
            | LayoutOp::SetFrame { .. }
    ) && op.targets().any(|id| {
        windows
            .find(id)
            .is_none_or(|(_, entity)| !windows.layout_is_writable(entity))
    }) {
        debug!(target: "spool::lua", "skipping {op:?}: native reassignment owns a layout endpoint");
        return;
    }
    // Widths and native tab entries can affect more than the named endpoint.
    // Check those current members too, without invalidating unrelated ops.
    if matches!(
        op,
        LayoutOp::SetWidth { .. }
            | LayoutOp::Swap(..)
            | LayoutOp::Stack { .. }
            | LayoutOp::Unstack(_)
    ) {
        for target in op.targets() {
            let Some((_, entity)) = windows.find(target) else {
                return;
            };
            let Some((strip, _)) = workspaces.iter().find(|(strip, _)| strip.contains(entity))
            else {
                continue;
            };
            if !windows.layout_column_is_writable(strip, entity) {
                debug!(target: "spool::lua", "skipping {op:?}: an affected column is not writable");
                return;
            }
            let members = if matches!(op, LayoutOp::SetWidth { .. }) {
                strip
                    .column_containing(entity)
                    .map(|column| column.window_iter().collect())
                    .unwrap_or_default()
            } else {
                strip.tab_group(entity).unwrap_or_else(|| vec![entity])
            };
            if members.into_iter().any(|member| {
                windows
                    .get(member)
                    .is_none_or(|window| !matches_window(&snapshot, window.id(), &windows))
            }) {
                debug!(target: "spool::lua", "skipping {op:?}: an affected layout member has been replaced");
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
    let viewport = if let LayoutOp::SetWidth { window, .. } = op {
        windows.find(window).and_then(|(_, entity)| {
            workspaces
                .iter()
                .find(|(strip, _)| strip.contains(entity))
                .and_then(|(_, child)| displays.get(child.parent()).ok())
                .map(|(display, dock)| display.actual_display_bounds(dock, &config))
        })
    } else {
        None
    };
    apply(op, &windows, &mut workspaces, viewport, &mut commands);
}

/// Applies one op, or explains why it could not be.
#[allow(clippy::too_many_lines)]
fn apply(
    op: LayoutOp,
    windows: &Windows,
    workspaces: &mut Query<(&mut LayoutStrip, &ChildOf), Without<Window>>,
    viewport: Option<IRect>,
    commands: &mut Commands,
) {
    // Resolve up front so every arm below can assume the window still exists.
    let entity = if let Some(window_id) = op.target() {
        let Some((_, entity)) = windows.find(window_id) else {
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
            let Some((_, other_entity)) = windows.find(other) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {other} is gone");
                return;
            };
            if is_floating(entity, windows) || is_floating(other_entity, windows) {
                return;
            }
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity) && strip.contains(other_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            let widths = (|| {
                let left = strip.get(strip.index_of(entity).ok()?).ok()?;
                let right = strip.get(strip.index_of(other_entity).ok()?).ok()?;
                let left_width = left.width(&|member| windows.requested_frame(member))?;
                let right_width = right.width(&|member| windows.requested_frame(member))?;
                let left_members = strip.tab_group(entity).unwrap_or_else(|| vec![entity]);
                let right_members = strip
                    .tab_group(other_entity)
                    .unwrap_or_else(|| vec![other_entity]);
                let mut sizes = resized_members(windows, left_members, right_width)?;
                sizes.extend(resized_members(windows, right_members, left_width)?);
                Some(sizes)
            })();
            let Some(widths) = widths else {
                return;
            };
            match strip.swap_items(entity, other_entity) {
                Ok(true) => {
                    apply_member_sizes(widths, commands);
                    commands.reshuffle_around(entity);
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
            let Some(viewport) = viewport.filter(|frame| frame.width() > 0 && frame.height() > 0)
            else {
                return;
            };
            let width = (ratio * f64::from(viewport.width())).round();
            if !(1.0..=f64::from(i32::MAX)).contains(&width) || is_floating(entity, windows) {
                debug!(target: "spool::lua", "skipping {op:?}: no representable tiled width");
                return;
            }
            let Some((strip, _)) = workspaces.iter().find(|(strip, _)| strip.contains(entity))
            else {
                return;
            };
            let width = crate::util::round_px(width);
            if !strip.accepts_column_width(entity, width, |member| windows.requested_frame(member))
            {
                debug!(target: "spool::lua", "skipping {op:?}: the strip offsets overflow");
                return;
            }
            let Some(column) = strip
                .index_of(entity)
                .ok()
                .and_then(|index| strip.get(index).ok())
                .filter(|column| !matches!(column, Column::Fullscren(_)))
            else {
                return;
            };
            let sizes = resized_members(windows, column.window_iter(), width);
            let Some(sizes) = sizes else {
                debug!(target: "spool::lua", "skipping {op:?}: a column member has no representable frame");
                return;
            };
            apply_member_sizes(sizes, commands);
            commands.reshuffle_around(entity);
        }

        LayoutOp::SetFrame { frame, .. } => {
            let entity = entity.expect("SetFrame names a window");
            // The layout engine owns a tiled window's geometry and will move
            // it back, so warn if the target isn't floated.
            if !is_floating(entity, windows) {
                debug!(
                    target: "spool::lua",
                    "{op:?} targets a tiled window; float it first or the layout will move it back"
                );
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
            let Some((_, onto_entity)) = windows.find(onto) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {onto} is gone");
                return;
            };
            if is_floating(entity, windows) || is_floating(onto_entity, windows) {
                debug!(target: "spool::lua", "skipping {op:?}: a floating window has no stack slot");
                return;
            }
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity) && strip.contains(onto_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            match strip.stack_onto(entity, onto_entity) {
                Ok(true) => commands.reshuffle_around(entity),
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
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity))
            else {
                return;
            };
            let mut proposed = strip.clone();
            match proposed.unstack(entity) {
                Ok(true) => {
                    if !proposed.accepts_column_widths(|_, column| {
                        column.width(&|member| windows.requested_frame(member))
                    }) {
                        debug!(target: "spool::lua", "skipping {op:?}: unstacked column offsets overflow");
                        return;
                    }
                    *strip = proposed;
                    commands.reshuffle_around(entity);
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

fn resized_members(
    windows: &Windows,
    members: impl IntoIterator<Item = Entity>,
    width: i32,
) -> Option<Vec<(Entity, Size)>> {
    members
        .into_iter()
        .map(|member| {
            let frame = windows.requested_frame(member)?;
            let size = frame.size().with_x(width);
            checked_window_frame(frame.min, size).map(|_| (member, size))
        })
        .collect()
}

fn apply_member_sizes(sizes: Vec<(Entity, Size)>, commands: &mut Commands) {
    for (member, size) in sizes {
        commands.resize_entity(member, size);
        if let Ok(mut entity_commands) = commands.get_entity(member) {
            entity_commands.try_remove::<FullWidthMarker>();
        }
    }
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

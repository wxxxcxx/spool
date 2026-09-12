//! Explicit command targets are resolved without changing focus or active markers.

use bevy::prelude::*;

use super::{Action, Direction, Operation, ResizeAxis};
use crate::config::Config;
use crate::ecs::layout::{
    Column, LayoutStrip, centered_origin_in_viewport, clamp_origin_to_viewport,
};
use crate::ecs::params::Windows;
use crate::ecs::topology::NativeTopology;
use crate::ecs::{
    DockPosition, Floating, FullWidthMarker, RetilePending, RetileWindow, SpawnCommandsExt,
};
use crate::manager::{Display, WindowManager};

type BlockedWindows<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<crate::ecs::NativeFullscreenMarker>,
        With<crate::ecs::WindowDefaultsPending>,
    )>,
>;

/// Validates and publishes one directly addressed operation.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy injects independent system parameters"
)]
pub(super) fn execute(
    In(action): In<Action>,
    windows: Windows,
    strips: Query<(Entity, &LayoutStrip, &ChildOf)>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    pending_retiles: Query<(), With<RetilePending>>,
    blocked: BlockedWindows,
    mut topology: ResMut<NativeTopology>,
    manager: Res<WindowManager>,
    config: Res<Config>,
    mut commands: Commands,
) -> crate::errors::Result<()> {
    let Action::TargetedWindow {
        window_id,
        operation,
    } = action
    else {
        return Err(crate::errors::Error::rejected(
            "unsupported explicit operation",
        ));
    };
    let (_, entity) = windows
        .find(window_id)
        .ok_or_else(|| crate::errors::Error::rejected("window_not_found"))?;
    let (_, _, state) = windows
        .get_tracked(entity)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    if blocked.contains(entity) || !state.is_visible() || !windows.layout_is_writable(entity) {
        return Err(crate::errors::Error::rejected("window_unavailable"));
    }
    let space = topology
        .observe_visible_window_space(&manager, window_id)
        .ok_or_else(|| crate::errors::Error::rejected("space_not_visible"))?;
    if topology.is_fullscreen(space) {
        return Err(crate::errors::Error::rejected("fullscreen_space"));
    }
    let (strip_entity, strip, parent) = strips
        .iter()
        .find(|(_, s, _)| s.id() == space)
        .ok_or_else(|| crate::errors::Error::rejected("layout_not_found"))?;
    let (display, dock) = displays
        .get(parent.parent())
        .map_err(|error| crate::errors::Error::rejection_with_cause("display_not_found", error))?;
    if topology.visible_display_for_space(space) != Some(display.id()) {
        return Err(crate::errors::Error::rejected("topology_unresolved"));
    }
    let (native, _) = topology
        .known_displays()
        .find(|(native, _)| native.id() == display.id())
        .ok_or_else(|| crate::errors::Error::rejected("topology_unresolved"))?;
    if display.clone().update_geometry(native) {
        return Err(crate::errors::Error::rejected("display_geometry_stale"));
    }
    if state.is_tiled()
        && strip
            .column_containing(entity)
            .is_none_or(|column| matches!(column, Column::Fullscren(_)))
    {
        return Err(crate::errors::Error::rejected("ineligible_layout_entry"));
    }
    let viewport = display
        .checked_actual_display_bounds(dock, &config)
        .ok_or_else(|| crate::errors::Error::rejected("invalid_display_frame"))?;
    let frame = windows
        .requested_frame(entity)
        .ok_or_else(|| crate::errors::Error::rejected("geometry_unavailable"))?;
    match operation {
        Operation::Move(direction) => {
            if state.is_floating() {
                let step = config.floating_window_move_step();
                let delta = match direction {
                    Direction::West => IVec2::new(-step, 0),
                    Direction::East => IVec2::new(step, 0),
                    Direction::North => IVec2::new(0, -step),
                    Direction::South => IVec2::new(0, step),
                    _ => return Err(crate::errors::Error::rejected("invalid_floating_direction")),
                };
                let origin = IVec2::new(
                    frame.min.x.saturating_add(delta.x),
                    frame.min.y.saturating_add(delta.y),
                );
                commands.reposition_entity(
                    entity,
                    clamp_origin_to_viewport(origin, frame.size(), viewport),
                );
                return Ok(());
            }
            if !windows.layout_column_is_writable(strip, entity) {
                return Err(crate::errors::Error::rejected("column_unavailable"));
            }
            let Some(other) = super::get_window_in_direction(&direction, entity, strip) else {
                return Ok(());
            };
            if !super::writable_column_range(&windows, strip, entity, other) {
                return Err(crate::errors::Error::rejected("column_unavailable"));
            }
            let index = strip.index_of(entity).map_err(|error| {
                crate::errors::Error::rejection_with_cause("layout_membership_unresolved", error)
            })?;
            let target_index = strip.index_of(other).map_err(|error| {
                crate::errors::Error::rejection_with_cause("layout_membership_unresolved", error)
            })?;
            let mut proposed = strip.clone();
            match index.cmp(&target_index) {
                std::cmp::Ordering::Equal => {
                    if let Some(Column::Stack(stack)) = proposed.get_column_mut(index) {
                        let a = stack
                            .iter()
                            .position(|item| item.contains(entity))
                            .ok_or_else(|| {
                                crate::errors::Error::rejected("layout_membership_unresolved")
                            })?;
                        let b = stack
                            .iter()
                            .position(|item| item.contains(other))
                            .ok_or_else(|| {
                                crate::errors::Error::rejected("layout_membership_unresolved")
                            })?;
                        stack.swap(a, b);
                    }
                }
                std::cmp::Ordering::Less => {
                    for index in index..target_index {
                        proposed.swap(index, index + 1);
                    }
                }
                std::cmp::Ordering::Greater => {
                    for index in (target_index..index).rev() {
                        proposed.swap(index, index + 1);
                    }
                }
            }
            commands.entity(strip_entity).insert(proposed);
            commands.ensure_visible(entity);
            Ok(())
        }
        Operation::ToggleFloating => {
            if pending_retiles.contains(entity) {
                commands.entity(entity).remove::<RetilePending>();
            } else if state.is_floating() {
                commands.trigger(RetileWindow(entity));
            } else {
                commands.entity(entity).insert(Floating);
            }
            Ok(())
        }
        Operation::Snap => {
            let frame = windows
                .moving_frame(entity)
                .ok_or_else(|| crate::errors::Error::rejected("geometry_unavailable"))?;
            let origin = clamp_origin_to_viewport(frame.min, frame.size(), viewport);
            if state.is_floating() {
                commands.reposition_entity(entity, origin);
            } else {
                if strip
                    .column_containing(entity)
                    .is_none_or(|column| matches!(column, Column::Fullscren(_)))
                {
                    return Err(crate::errors::Error::rejected("ineligible_layout_entry"));
                }
                let position = windows
                    .layout_position(entity)
                    .ok_or_else(|| crate::errors::Error::rejected("layout_position_unavailable"))?
                    .0;
                let x = origin
                    .x
                    .checked_sub(position.x)
                    .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
                let y = origin
                    .y
                    .checked_sub(position.y)
                    .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
                commands.reposition_entity(strip_entity, IVec2::new(x, y));
            }
            Ok(())
        }
        Operation::ToggleStack => {
            if !state.is_tiled() || !windows.layout_column_is_writable(strip, entity) {
                return Err(crate::errors::Error::rejected("column_unavailable"));
            }
            let mut proposed = strip.clone();
            let stacked = matches!(strip.column_containing(entity), Some(Column::Stack(_)));
            let changed = if stacked {
                proposed.unstack(entity)
            } else {
                let index = strip.index_of(entity).map_err(|error| {
                    crate::errors::Error::rejection_with_cause(
                        "layout_membership_unresolved",
                        error,
                    )
                })?;
                if index > 0
                    && strip.get(index - 1).is_ok_and(|column| {
                        column
                            .window_iter()
                            .any(|member| !windows.layout_is_writable(member))
                    })
                {
                    return Err(crate::errors::Error::rejected("column_unavailable"));
                }
                proposed.stack(entity)
            }
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("ineligible_layout_entry", error)
            })?;
            if !changed {
                return Ok(());
            }
            if !proposed.accepts_column_widths(|_, column| {
                column.width(&|member| windows.requested_frame(member))
            }) {
                return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
            }
            commands.entity(strip_entity).insert(proposed);
            commands.entity(entity).remove::<FullWidthMarker>();
            commands.reshuffle_around(entity);
            Ok(())
        }
        Operation::Maximize => {
            if state.is_tiled() && !windows.layout_column_is_writable(strip, entity) {
                return Err(crate::errors::Error::rejected("column_unavailable"));
            }
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
                    super::checked_frame_size(frame)
                        .ok_or_else(|| crate::errors::Error::rejected("invalid_restore_frame"))?;
                    frame
                } else {
                    let width = super::checked_ratio_width(marker.width_ratio, viewport.width())
                        .ok_or_else(|| crate::errors::Error::rejected("invalid_restore_width"))?;
                    super::checked_window_frame(frame.min, viewport.size().with_x(width))
                        .ok_or_else(|| crate::errors::Error::rejected("invalid_restore_frame"))?
                }
            } else {
                viewport
            };
            let mut proposed = strip.clone();
            let changed = if state.is_tiled() && !restoring {
                proposed.unstack(entity).map_err(|error| {
                    crate::errors::Error::rejection_with_cause("ineligible_layout_entry", error)
                })?
            } else {
                false
            };
            let sizes = if state.is_floating() {
                vec![(entity, target.size())]
            } else {
                super::column_resize_plan(&windows, &proposed, entity, target)
                    .ok_or_else(|| crate::errors::Error::rejected("column_unavailable"))?
            };
            if changed {
                commands.entity(strip_entity).insert(proposed);
            }
            super::apply_column_sizes(sizes, &mut commands);
            if !restoring {
                commands.entity(entity).insert(FullWidthMarker {
                    width_ratio: f64::from(frame.width()) / f64::from(viewport.width()),
                    floating_frame: state.is_floating().then_some(frame),
                });
            }
            commands.reposition_entity(entity, target.min);
            if state.is_tiled() {
                commands.reshuffle_around(entity);
            }
            Ok(())
        }
        Operation::Center => {
            if state.is_tiled() {
                if !strip.contains(entity) {
                    return Err(crate::errors::Error::rejected(
                        "layout_membership_unresolved",
                    ));
                }
                let position = windows
                    .layout_position(entity)
                    .ok_or_else(|| crate::errors::Error::rejected("layout_position_unavailable"))?;
                let origin = frame
                    .min
                    .with_x(display.bounds().center().x - frame.width() / 2);
                let x = origin
                    .x
                    .checked_sub(position.0.x)
                    .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
                let y = origin
                    .y
                    .checked_sub(position.0.y)
                    .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
                commands.reposition_entity(strip_entity, IVec2::new(x, y));
            } else {
                let origin = clamp_origin_to_viewport(
                    viewport.center() - frame.size() / 2,
                    frame.size(),
                    viewport,
                );
                commands.reposition_entity(entity, origin);
            }
            Ok(())
        }
        Operation::Resize { axis, direction } => {
            let operation = Operation::Resize { axis, direction };
            if state.is_floating() {
                let target = super::floating_resize_frame(&operation, frame, viewport, &config)
                    .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
                if target != frame {
                    commands.entity(entity).remove::<FullWidthMarker>();
                    commands.reposition_entity(entity, target.min);
                    commands.resize_entity(entity, target.size());
                }
                return Ok(());
            }
            if !windows.layout_column_is_writable(strip, entity) {
                return Err(crate::errors::Error::rejected("column_unavailable"));
            }
            if axis == ResizeAxis::Height {
                if !matches!(strip.column_containing(entity), Some(Column::Stack(_))) {
                    return Err(crate::errors::Error::rejected("not_a_stack"));
                }
                if super::resize_tiled_height(
                    entity,
                    frame,
                    viewport,
                    direction,
                    strip,
                    &config,
                    &mut commands,
                ) {
                    commands.entity(entity).remove::<FullWidthMarker>();
                }
                return Ok(());
            }
            let ratio = super::tiled_width_ratio(
                &operation,
                f64::from(frame.width()) / f64::from(viewport.width()),
                &config,
            )
            .ok_or_else(|| crate::errors::Error::rejected("invalid_width"))?;
            let width = super::checked_ratio_width(ratio, viewport.width())
                .ok_or_else(|| crate::errors::Error::rejected("invalid_width"))?;
            let size = frame.size().with_x(width);
            let target = super::checked_window_frame(
                centered_origin_in_viewport(frame, size, viewport),
                size,
            )
            .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
            let sizes = super::column_resize_plan(&windows, strip, entity, target)
                .ok_or_else(|| crate::errors::Error::rejected("column_unavailable"))?;
            super::apply_column_sizes(sizes, &mut commands);
            commands.reposition_entity(entity, target.min);
            commands.reshuffle_around(entity);
            Ok(())
        }
        _ => Err(crate::errors::Error::rejected(
            "unsupported explicit operation",
        )),
    }
}

//! Explicit command targets are resolved without changing focus or active markers.

use bevy::prelude::*;

use super::admission::Admission;
use super::{Action, Direction, Operation};
use crate::config::Config;
use crate::ecs::layout::clamp_origin_to_viewport;
use crate::ecs::params::Windows;
use crate::ecs::{Floating, FullWidthMarker, RetilePending, RetileWindow, SpawnCommandsExt};

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
    pending_retiles: Query<(), With<RetilePending>>,
    blocked: BlockedWindows,
    mut admission: Admission,
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
    let entity = admission.writable_window(window_id)?;
    if blocked.contains(entity) {
        return Err(crate::errors::Error::rejected("window_unavailable"));
    }
    let (_, _, state) = windows
        .get_tracked(entity)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    let space = admission.visible_space(window_id)?;
    let view = admission.space_view(space)?;
    let strip_entity = view.strip_entity;
    let strip = view.strip;
    let display = view.display;
    let viewport = view.viewport;
    if state.is_tiled() {
        Admission::tiled_column(strip, entity)?;
    }
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
            Err(crate::errors::Error::rejected("invalid_geometry_domain"))
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
                Admission::tiled_column(strip, entity)?;
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
        Operation::Maximize => {
            if state.is_tiled() {
                admission.writable_column(strip, entity)?;
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
            // Tiled maximize is admitted by the column intent domain.
            if !state.is_floating() {
                return Err(crate::errors::Error::rejected("invalid_geometry_domain"));
            }
            let sizes = vec![(entity, target.size())];
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
        operation @ (Operation::Resize { .. } | Operation::SetWidth(_)) => {
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
            Err(crate::errors::Error::rejected("invalid_geometry_domain"))
        }
        _ => Err(crate::errors::Error::rejected(
            "unsupported explicit operation",
        )),
    }
}

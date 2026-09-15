//! Arrangement and stack-height intent, independent of native effect eligibility.
use super::admission::{Admission, Rejection};
use super::{Action, Operation, ResizeAxis, ResizeDirection};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::Windows;
use crate::ecs::topology::NativeTopology;
use crate::ecs::{ActiveWorkspaceMarker, DockPosition, SpawnCommandsExt};
use crate::manager::Display;
use bevy::prelude::*;
use spool_shared_types::commands::SpaceLayoutOperation;

/// `None` delegates floating geometry or an explicitly visible display-edge move.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one state admission boundary resolves complete affected cohorts before atomic mutation"
)]
pub(super) fn execute(
    In(action): In<Action>,
    windows: Windows,
    mut strips: Query<(
        Entity,
        &mut LayoutStrip,
        Option<&ChildOf>,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    topology: Res<NativeTopology>,
    focus: Res<FocusCoordinator>,
    config: Res<Config>,
    mut commands: Commands,
) -> Option<crate::errors::Result<()>> {
    let reject = |reason| Some(Err(crate::errors::Error::rejected(reason)));
    let (space, entity, operation) = match action {
        Action::SpaceLayout {
            space_id,
            operation: SpaceLayoutOperation::Equalize { column },
        } => {
            let strip = match Admission::space_scope_strip_in(
                strips.iter().map(|(_, strip, _, active)| (active, strip)),
                space_id,
            ) {
                Ok(strip) => strip,
                Err(rejection) => return Some(Err(rejection.into())),
            };
            let entity = if let Some(ordinal) = column {
                let index = match Admission::column_index(strip, ordinal) {
                    Ok(index) => index,
                    Err(rejection) => return Some(Err(rejection.into())),
                };
                match Admission::column_leader(strip, index) {
                    Ok(entity) => entity,
                    Err(rejection) => return Some(Err(rejection.into())),
                }
            } else {
                let Some(entity) = super::command_entity(&windows, &focus) else {
                    return Some(Err(Rejection::NoFocusedWindow.into()));
                };
                if !strip.contains(entity) {
                    return Some(Err(Rejection::FocusedWindowOutsideSpace.into()));
                }
                entity
            };
            (strip.id(), entity, Operation::Equalize)
        }
        Action::TargetedWindow {
            window_id,
            operation:
                operation @ (Operation::Move(_)
                | Operation::ToggleStack
                | Operation::Equalize
                | Operation::Resize {
                    axis: ResizeAxis::Height,
                    ..
                }),
        } => {
            let entity = match Admission::window_any(&windows, window_id) {
                Ok(entity) => entity,
                Err(rejection) => return Some(Err(rejection.into())),
            };
            if windows
                .get_tracked(entity)
                .is_some_and(|(_, _, state)| state.is_floating())
            {
                return None;
            }
            let strip = match Admission::strip_containing_in(
                strips.iter().map(|(_, strip, _, _)| strip),
                entity,
            ) {
                Ok(strip) => strip,
                Err(rejection) => return Some(Err(rejection.into())),
            };
            (strip.id(), entity, operation)
        }
        _ => return None,
    };
    let (_, mut strip, parent, _) = strips
        .iter_mut()
        .find(|(_, strip, _, _)| strip.id() == space)?;
    let Ok(index) = strip.index_of(entity) else {
        return reject("layout_membership_unresolved");
    };
    if let Err(rejection) = Admission::ineligible_column(&strip, index) {
        return Some(Err(rejection.into()));
    }
    let visible = topology.visible_display_for_space(space).is_some();
    let mut affected = vec![index];
    let destination = if let Operation::Move(direction) = &operation {
        let destination = super::get_window_in_direction(direction, entity, &strip);
        if let Some(other) = destination {
            let Ok(other_index) = strip.index_of(other) else {
                return reject("layout_membership_unresolved");
            };
            affected = (index.min(other_index)..=index.max(other_index)).collect();
        }
        destination
    } else {
        if matches!(operation, Operation::ToggleStack)
            && !matches!(strip.get(index), Ok(Column::Stack(_)))
            && index > 0
        {
            affected.push(index - 1);
        }
        None
    };
    for index in affected {
        if let Err(rejection) = Admission::eligible_column_in(&windows, &strip, index) {
            return Some(Err(rejection.into()));
        }
    }
    if let Operation::Move(direction) = &operation
        && destination.is_none()
    {
        // Only a currently visible/native-eligible endpoint may enter the existing
        // cross-display effect workflow. Hidden edits never activate anything.
        if visible
            && super::DisplayTarget::from_direction(direction).is_some()
            && windows
                .get_tracked(entity)
                .is_some_and(|(_, _, state)| state.is_visible())
        {
            return None;
        }
        return Some(Ok(()));
    }
    let mut proposed = strip.clone();
    let result = match &operation {
        Operation::Move(_) => {
            let other = destination.expect("move destination resolved above");
            let other_index = proposed.index_of(other).expect("validated destination");
            match index.cmp(&other_index) {
                std::cmp::Ordering::Equal => {
                    proposed.edit_column(index, |column| {
                        if let Column::Stack(stack) = column
                            && let (Some(a), Some(b)) = (
                                stack.iter().position(|item| item.contains(entity)),
                                stack.iter().position(|item| item.contains(other)),
                            )
                        {
                            stack.swap(a, b);
                        }
                    });
                }
                std::cmp::Ordering::Less => {
                    for index in index..other_index {
                        proposed.swap(index, index + 1);
                    }
                }
                std::cmp::Ordering::Greater => {
                    for index in (other_index..index).rev() {
                        proposed.swap(index, index + 1);
                    }
                }
            }
            Ok(())
        }
        Operation::ToggleStack => if matches!(proposed.get(index), Ok(Column::Stack(_))) {
            proposed.unstack(entity)
        } else {
            proposed.stack(entity)
        }
        .map(|_| ()),
        Operation::Equalize => proposed.equalize_heights(entity).map(|_| ()),
        Operation::Resize { direction, .. } => {
            let viewport = parent
                .and_then(|parent| displays.get(parent.parent()).ok())
                .and_then(|(display, dock)| display.checked_actual_display_bounds(dock, &config))
                .map(|bounds| bounds.height());
            let delta = f64::from(config.floating_window_resize_step())
                * if *direction == ResizeDirection::Grow {
                    1.0
                } else {
                    -1.0
                };
            proposed.resize_height(entity, delta, viewport).map(|_| ())
        }
        _ => unreachable!("state-only operations matched above"),
    };
    Some(result.and_then(|()| {
        if !proposed.width_budget_is_valid() {
            return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
        }
        if proposed.structure_revision() == strip.structure_revision()
            && proposed.height_context(entity) == strip.height_context(entity)
        {
            return Ok(());
        }
        *strip = proposed;
        if matches!(operation, Operation::ToggleStack) {
            commands
                .entity(entity)
                .remove::<crate::ecs::FullWidthMarker>();
        }
        if visible {
            if matches!(operation, Operation::Move(_)) {
                commands.ensure_visible(entity);
            } else {
                commands.reshuffle_around(entity);
            }
        }
        Ok(())
    }))
}

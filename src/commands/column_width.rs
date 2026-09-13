//! State-only column edits. Native eligibility belongs to the effect committer.
use super::{Action, Operation, ResizeAxis};
use crate::config::Config;
use crate::ecs::layout::{Column, LayoutStrip, WidthIntent};
use crate::ecs::params::Windows;
use crate::ecs::{ActiveWorkspaceMarker, DockPosition};
use crate::manager::Display;
use bevy::prelude::*;
use spool_shared_types::commands::{ColumnWidth, SpaceLayoutOperation};

type PendingTransitions<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<crate::ecs::native_space::NativeMoveOwner>,
        With<crate::ecs::workspace::WindowSpaceReassignmentPending>,
    )>,
>;

/// `None` means this operation belongs to another domain (e.g. floating geometry).
#[allow(
    clippy::too_many_lines,
    reason = "one atomic admission path resolves both explicit column and window targets"
)]
pub(super) fn execute(
    In(action): In<Action>,
    windows: Windows,
    mut strips: Query<(
        &mut LayoutStrip,
        Option<&ChildOf>,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    config: Res<Config>,
    transitions: PendingTransitions,
) -> Option<crate::errors::Result<()>> {
    let reject = |reason| Some(Err(crate::errors::Error::rejected(reason)));
    let (space, operation) = match action {
        Action::SpaceLayout {
            space_id,
            operation:
                op @ (SpaceLayoutOperation::SetWidth { .. } | SpaceLayoutOperation::Balance { .. }),
        } => (space_id, op),
        Action::TargetedWindow {
            window_id,
            operation:
                op @ (Operation::SetWidth(_)
                | Operation::Resize {
                    axis: ResizeAxis::Width,
                    ..
                }
                | Operation::Maximize),
        } => {
            let Some((_, entity)) = windows.find_any(window_id) else {
                return reject("window_not_found");
            };
            if windows
                .get_tracked(entity)
                .is_some_and(|(_, _, state)| state.is_floating())
            {
                return None;
            }
            let Some((strip, _, _)) = strips.iter().find(|(strip, _, _)| strip.contains(entity))
            else {
                return reject("layout_not_found");
            };
            if strip.column_containing(entity).is_some_and(|column| {
                column
                    .window_iter()
                    .any(|member| transitions.contains(member))
            }) {
                return reject("layout_transition_pending");
            }
            return edit_window(entity, op, strip.id(), &mut strips, &displays, &config);
        }
        _ => return None,
    };
    let mut matches = strips
        .iter_mut()
        .filter(|(strip, _, active)| space.map_or(*active, |id| strip.id() == id));
    let Some((mut strip, _, _)) = matches.next() else {
        return reject("layout_not_found");
    };
    if matches.next().is_some() {
        return reject("ambiguous_layout");
    }
    let ordinal = match operation {
        SpaceLayoutOperation::SetWidth { column, .. } => Some(column),
        SpaceLayoutOperation::Balance { reference_column } => reference_column,
        _ => unreachable!(),
    };
    let index = match ordinal {
        Some(ordinal) => match ordinal.checked_sub(1) {
            Some(index) => index,
            None => return reject("column_out_of_range"),
        },
        None => match windows
            .focused()
            .map(|(_, entity)| entity)
            .and_then(|entity| strip.index_of(entity).ok())
        {
            Some(index) => index,
            None => return reject("no_focused_window"),
        },
    };
    if strip.get(index).is_err()
        || strip
            .get(index)
            .is_ok_and(|column| matches!(column, Column::Fullscren(_)))
    {
        return reject("ineligible_layout_entry");
    }
    if strip
        .columns()
        .enumerate()
        .filter(|(i, _)| matches!(operation, SpaceLayoutOperation::Balance { .. }) || *i == index)
        .any(|(_, column)| {
            column
                .window_iter()
                .any(|member| transitions.contains(member))
        })
    {
        return reject("layout_transition_pending");
    }
    let Some(state) = strip.column_state(index) else {
        return reject("column_out_of_range");
    };
    let id = state.id;
    let width = match operation {
        SpaceLayoutOperation::SetWidth { width, .. } => match width {
            ColumnWidth::Inherit => WidthIntent::InheritConfig,
            ColumnWidth::Points(value) => WidthIntent::Absolute(value),
            ColumnWidth::Ratio(value) => WidthIntent::ViewportRatio(value),
        },
        SpaceLayoutOperation::Balance { .. } => match state.width {
            WidthIntent::InheritConfig => state.configured_width.unwrap_or_else(|| {
                WidthIntent::ViewportRatio(*config.preset_column_widths().first().unwrap_or(&0.5))
            }),
            explicit => explicit,
        },
        _ => unreachable!(),
    };
    let mut proposed = strip.clone();
    let result = if matches!(operation, SpaceLayoutOperation::Balance { .. }) {
        let ids = proposed
            .columns()
            .enumerate()
            .filter(|(_, column)| !matches!(column, Column::Fullscren(_)))
            .filter_map(|(index, _)| proposed.column_state(index).map(|s| s.id))
            .collect::<Vec<_>>();
        ids.into_iter()
            .try_for_each(|id| proposed.set_width_intent(id, width).map(|_| ()))
    } else {
        proposed.set_width_intent(id, width).map(|_| ())
    };
    Some(result.and_then(|()| {
        if !proposed.width_budget_is_valid() {
            return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
        }
        *strip = proposed;
        Ok(())
    }))
}

fn edit_window(
    entity: Entity,
    operation: Operation,
    space: u64,
    strips: &mut Query<(
        &mut LayoutStrip,
        Option<&ChildOf>,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: &Query<(&Display, Option<&DockPosition>)>,
    config: &Config,
) -> Option<crate::errors::Result<()>> {
    let (mut strip, parent, _) = strips
        .iter_mut()
        .find(|(strip, _, _)| strip.id() == space)?;
    let index = strip.index_of(entity).ok()?;
    if matches!(strip.get(index), Ok(Column::Fullscren(_))) {
        return Some(Err(crate::errors::Error::rejected(
            "ineligible_layout_entry",
        )));
    }
    let state = strip.column_state(index)?;
    let id = state.id;
    let width = match operation {
        Operation::SetWidth(ratio) => WidthIntent::ViewportRatio(ratio),
        Operation::Maximize => {
            let mut proposed = strip.clone();
            if state.restore_width.is_none()
                && let Err(error) = proposed.unstack(entity)
            {
                return Some(Err(error));
            }
            let Some(id) = proposed.column_id(entity) else {
                return Some(Err(crate::errors::Error::rejected("column_unavailable")));
            };
            return Some(proposed.toggle_full_width(id).and_then(|_| {
                if !proposed.width_budget_is_valid() {
                    return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
                }
                *strip = proposed;
                Ok(())
            }));
        }
        Operation::Resize { .. } => {
            let current = match state.width {
                WidthIntent::ViewportRatio(ratio) => ratio,
                WidthIntent::InheritConfig => {
                    let Some(ratio) = strip.width_ratio(index) else {
                        return Some(Err(crate::errors::Error::rejected("width_context_unknown")));
                    };
                    ratio
                }
                WidthIntent::Absolute(points) => {
                    let viewport = parent
                        .and_then(|parent| displays.get(parent.parent()).ok())
                        .and_then(|(display, dock)| {
                            display.checked_actual_display_bounds(dock, config)
                        });
                    let Some(viewport) = viewport else {
                        return Some(Err(crate::errors::Error::rejected("viewport_unknown")));
                    };
                    points / f64::from(viewport.width())
                }
            };
            let Some(ratio) = super::tiled_width_ratio(&operation, current, config) else {
                return Some(Err(crate::errors::Error::rejected("invalid_width")));
            };
            WidthIntent::ViewportRatio(ratio)
        }
        _ => return None,
    };
    let mut proposed = strip.clone();
    Some(proposed.set_width_intent(id, width).and_then(|_| {
        if !proposed.width_budget_is_valid() {
            return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
        }
        *strip = proposed;
        Ok(())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ResizeDirection;
    use crate::config::{MainOptions, WindowParams};
    use crate::ecs::layout::WidthConstraint;
    use crate::tests::TestHarness;

    #[test]
    fn balance_copies_reference_configuration_without_baking_in_constraints() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(10);
        let space = {
            let world = harness.world();
            let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
            let first = strip.column_state(0).unwrap().id;
            let second = strip.column_state(1).unwrap().id;
            strip
                .set_width_intent(first, WidthIntent::InheritConfig)
                .unwrap();
            strip
                .set_width_intent(second, WidthIntent::InheritConfig)
                .unwrap();
            strip
                .set_column_default(first, Some(WidthIntent::ViewportRatio(0.75)))
                .unwrap();
            strip
                .set_column_default(second, Some(WidthIntent::ViewportRatio(0.25)))
                .unwrap();
            strip
                .set_column_constraints(
                    first,
                    vec![WidthConstraint::Interval {
                        min: 1000.0,
                        max: 1200.0,
                    }],
                )
                .unwrap();
            assert_eq!(strip.effective_column_width(0).unwrap().slot, 1000);
            assert_eq!(strip.effective_column_width(1).unwrap().slot, 256);
            strip.id()
        };
        harness
            .world()
            .run_system_cached_with(
                execute,
                Action::SpaceLayout {
                    space_id: Some(space),
                    operation: SpaceLayoutOperation::Balance {
                        reference_column: Some(1),
                    },
                },
            )
            .unwrap()
            .unwrap()
            .unwrap();
        let world = harness.world();
        let strip = world.query::<&LayoutStrip>().single(world).unwrap();
        for state in strip.column_states() {
            assert_eq!(state.width, WidthIntent::ViewportRatio(0.75));
            assert_eq!(state.intent_revision, 2);
        }
        assert_eq!(strip.effective_column_width(0).unwrap().slot, 1000);
        assert_eq!(strip.effective_column_width(1).unwrap().slot, 768);
    }

    #[test]
    fn resize_presets_start_from_inherited_rule_not_default_or_constrained_frame() {
        for (direction, expected) in [
            (ResizeDirection::Grow, 1.0),
            (ResizeDirection::Shrink, 0.66667),
        ] {
            let mut rule = WindowParams::new(".*", None);
            rule.width = Some(0.75);
            let config: Config = (MainOptions::default(), vec![rule]).into();
            let mut harness = TestHarness::new().with_config(config).with_windows(1);
            harness.pump_frames(10);
            {
                let world = harness.world();
                let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
                let state = strip.column_state(0).unwrap();
                assert_eq!(state.width, WidthIntent::InheritConfig);
                let id = state.id;
                strip
                    .set_column_constraints(
                        id,
                        vec![WidthConstraint::Interval {
                            min: 1536.0,
                            max: 2048.0,
                        }],
                    )
                    .unwrap();
                assert_eq!(strip.effective_column_width(0).unwrap().slot, 1536);
            }
            harness
                .world()
                .run_system_cached_with(
                    execute,
                    Action::TargetedWindow {
                        window_id: 0,
                        operation: Operation::Resize {
                            axis: ResizeAxis::Width,
                            direction,
                        },
                    },
                )
                .unwrap()
                .unwrap()
                .unwrap();
            let world = harness.world();
            let strip = world.query::<&LayoutStrip>().single(world).unwrap();
            assert_eq!(
                strip.column_state(0).unwrap().width,
                WidthIntent::ViewportRatio(expected)
            );
            assert_eq!(strip.column_state(0).unwrap().intent_revision, 1);
        }
    }
}

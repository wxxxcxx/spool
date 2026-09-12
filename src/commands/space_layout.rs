//! Admission of arrangements against a Space's complete retained columns.

use bevy::prelude::*;
use spool_shared_types::commands::SpaceLayoutOperation;

use super::{Action, checked_window_frame};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::Windows;
use crate::ecs::topology::NativeTopology;
use crate::ecs::{ActiveWorkspaceMarker, DockPosition, FullWidthMarker, SpawnCommandsExt};
use crate::manager::{Display, WindowManager};

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
    strips: Query<(Entity, &LayoutStrip, &ChildOf, Has<ActiveWorkspaceMarker>)>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    focus: Res<FocusCoordinator>,
    mut topology: ResMut<NativeTopology>,
    manager: Res<WindowManager>,
    blocked: Query<(), With<crate::ecs::WindowDefaultsPending>>,
    config: Res<Config>,
    mut commands: Commands,
) -> crate::errors::Result<()> {
    let Action::SpaceLayout {
        space_id,
        operation,
    } = action
    else {
        return Err(crate::errors::Error::rejected("invalid_layout_operation"));
    };
    let mut matches = strips
        .iter()
        .filter(|(_, strip, _, active)| space_id.map_or(*active, |id| strip.id() == id));
    let (_, strip, parent, _) = matches
        .next()
        .ok_or_else(|| crate::errors::Error::rejected("layout_not_found"))?;
    if matches.next().is_some() {
        return Err(crate::errors::Error::rejected("ambiguous_layout"));
    }
    if !topology.refresh_for_command(&manager) {
        return Err(crate::errors::Error::rejected("topology_unresolved"));
    }
    let (display, dock) = displays
        .get(parent.parent())
        .map_err(|error| crate::errors::Error::rejection_with_cause("display_not_found", error))?;
    // A temporary effect-admission gate, not a validity rule of LayoutStrip.
    if topology.visible_display_for_space(strip.id()) != Some(display.id()) {
        return Err(crate::errors::Error::rejected("space_not_visible"));
    }
    if topology.is_fullscreen(strip.id()) {
        return Err(crate::errors::Error::rejected("fullscreen_space"));
    }
    let ordinal = match operation {
        SpaceLayoutOperation::Equalize { column } => column,
        SpaceLayoutOperation::Balance { reference_column } => reference_column,
        SpaceLayoutOperation::ToggleTiledVisibility => {
            return Err(crate::errors::Error::rejected("invalid_layout_operation"));
        }
    };
    let (native, _) = topology
        .known_displays()
        .find(|(native, _)| native.id() == display.id())
        .ok_or_else(|| crate::errors::Error::rejected("topology_unresolved"))?;
    if display.clone().update_geometry(native) {
        return Err(crate::errors::Error::rejected("display_geometry_stale"));
    }
    let (index, anchor) = if let Some(ordinal) = ordinal {
        let index = ordinal
            .checked_sub(1)
            .ok_or_else(|| crate::errors::Error::rejected("column_out_of_range"))?;
        let column = strip.get(index).map_err(|error| {
            crate::errors::Error::rejection_with_cause("column_out_of_range", error)
        })?;
        let anchor = column
            .top()
            .ok_or_else(|| crate::errors::Error::rejected("column_unavailable"))?;
        (index, anchor)
    } else {
        let anchor = super::command_entity(&windows, &focus)
            .ok_or_else(|| crate::errors::Error::rejected("no_focused_window"))?;
        let index = strip.index_of(anchor).map_err(|error| {
            crate::errors::Error::rejection_with_cause("focused_window_outside_space", error)
        })?;
        (index, anchor)
    };
    if !windows.layout_column_is_writable(strip, anchor) {
        return Err(crate::errors::Error::rejected("column_unavailable"));
    }
    let memberships = topology.observe_memberships(&manager).map_err(|error| {
        crate::errors::Error::rejection_with_cause("membership_unresolved", error)
    })?;
    let admitted = |entity| {
        windows
            .get(entity)
            .is_some_and(|window| memberships.unique_space(window.id()) == Some(strip.id()))
            && !blocked.contains(entity)
            && windows.layout_is_writable(entity)
    };
    if !admitted(anchor) {
        return Err(crate::errors::Error::rejected("column_unavailable"));
    }
    let sizes = match operation {
        SpaceLayoutOperation::Equalize { .. } => {
            let Column::Stack(stack) = strip.get(index).map_err(|error| {
                crate::errors::Error::rejection_with_cause("column_out_of_range", error)
            })?
            else {
                return Err(crate::errors::Error::rejected("not_a_stack"));
            };
            let viewport = display
                .checked_actual_display_bounds(dock, &config)
                .ok_or_else(|| crate::errors::Error::rejected("invalid_display_frame"))?;
            let height = i32::try_from(stack.len())
                .ok()
                .and_then(|count| viewport.height().checked_div(count))
                .filter(|height| *height > 0)
                .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
            stack
                .iter()
                .flat_map(crate::ecs::layout::StackItem::window_iter)
                .map(|member| {
                    if !admitted(member) {
                        return None;
                    }
                    let frame = windows.requested_frame(member)?;
                    let size = frame.size().with_y(height);
                    checked_window_frame(frame.min, size)?;
                    Some((member, size))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| crate::errors::Error::rejected("column_unavailable"))?
        }
        SpaceLayoutOperation::Balance { .. } => {
            let width = windows
                .requested_frame(anchor)
                .ok_or_else(|| crate::errors::Error::rejected("geometry_unavailable"))?
                .width();
            if !strip.accepts_column_widths(|_, column| {
                if matches!(column, Column::Fullscren(_)) {
                    column.width(&|member| windows.requested_frame(member))
                } else {
                    Some(width)
                }
            }) {
                return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
            }
            strip
                .columns()
                .filter(|column| !matches!(column, Column::Fullscren(_)))
                .flat_map(Column::window_iter)
                .map(|member| {
                    if !admitted(member) {
                        return None;
                    }
                    let frame = windows.requested_frame(member)?;
                    let size = frame.size().with_x(width);
                    checked_window_frame(frame.min, size)?;
                    Some((member, size))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| crate::errors::Error::rejected("column_unavailable"))?
        }
        SpaceLayoutOperation::ToggleTiledVisibility => unreachable!(),
    };
    for (entity, size) in sizes {
        if matches!(operation, SpaceLayoutOperation::Balance { .. }) {
            commands.entity(entity).remove::<FullWidthMarker>();
        }
        commands.resize_entity(entity, size);
    }
    commands.reshuffle_around(anchor);
    Ok(())
}

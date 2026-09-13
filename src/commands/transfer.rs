//! Resolve display transfers without borrowing the active display as the target.

use bevy::prelude::*;

use super::display_navigation::{DisplayTarget, select_display};
use super::{MoveFocus, checked_window_frame};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{LayoutStrip, centered_origin_in_viewport};
use crate::ecs::native_space::{DisplayMovePlan, NativeSpaceTransactions};
use crate::ecs::params::Windows;
use crate::ecs::topology::NativeTopology;
use crate::ecs::workspace::PendingSpaceDestruction;
use crate::ecs::{DockPosition, NativeFullscreenMarker};
use crate::manager::{Display, Size, WindowManager};

type DisplayTransferBlockedWindows<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<crate::ecs::native_space::NativeMoveOwner>,
        With<crate::ecs::workspace::WindowSpaceReassignmentPending>,
        With<crate::ecs::WindowDefaultsPending>,
    )>,
>;

type TransferStrips<'w, 's> = Query<
    'w,
    's,
    (&'static LayoutStrip, &'static ChildOf),
    (
        Without<PendingSpaceDestruction>,
        Without<NativeFullscreenMarker>,
    ),
>;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn execute(
    In((window_id, move_focus, destination)): In<(i32, MoveFocus, DisplayTarget)>,
    windows: Windows,
    strips: TransferStrips,
    displays: Query<(Entity, &Display, Option<&DockPosition>)>,
    focus: Res<FocusCoordinator>,
    mut topology: ResMut<NativeTopology>,
    moving: DisplayTransferBlockedWindows,
    manager: Res<WindowManager>,
    config: Res<Config>,
    mut transactions: ResMut<NativeSpaceTransactions>,
    time: Res<Time>,
    mut commands: Commands,
) -> crate::errors::Result<()> {
    let (_, entity) = windows
        .find(window_id)
        .ok_or_else(|| crate::errors::Error::rejected("window_not_found"))?;
    let (_, _, state) = windows
        .get_tracked(entity)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    if !state.is_visible() || !windows.layout_is_writable(entity) {
        return Err(crate::errors::Error::rejected("window_unavailable"));
    }
    let source_id = topology
        .observe_visible_window_space(&manager, window_id)
        .ok_or_else(|| crate::errors::Error::rejected("space_not_visible"))?;
    let source_display_id = topology
        .visible_display_for_space(source_id)
        .ok_or_else(|| crate::errors::Error::rejected("topology_unresolved"))?;
    if topology.is_fullscreen(source_id) {
        return Err(crate::errors::Error::rejected("fullscreen_space"));
    }
    let Some(target_display_id) = select_display(
        source_display_id,
        destination,
        topology.known_displays().map(|(display, _)| display),
    ) else {
        return Ok(()); // A bounded display edge is an admitted no-op.
    };
    let target_id = topology
        .visible_space(target_display_id)
        .ok_or_else(|| crate::errors::Error::rejected("target_space_unavailable"))?;
    if topology.visible_display_for_space(target_id) != Some(target_display_id)
        || topology.is_fullscreen(target_id)
    {
        return Err(crate::errors::Error::rejected("target_space_unavailable"));
    }
    let context = |id| -> crate::errors::Result<_> {
        let mut candidates = displays.iter().filter(|(_, display, _)| display.id() == id);
        let (entity, display, dock) = candidates
            .next()
            .ok_or_else(|| crate::errors::Error::rejected("display_not_found"))?;
        if candidates.next().is_some() {
            return Err(crate::errors::Error::rejected("ambiguous_display"));
        }
        let (native, _) = topology
            .known_displays()
            .find(|(display, _)| display.id() == id)
            .ok_or_else(|| crate::errors::Error::rejected("topology_unresolved"))?;
        if display.clone().update_geometry(native) {
            return Err(crate::errors::Error::rejected("display_geometry_stale"));
        }
        let viewport = display
            .checked_actual_display_bounds(dock, &config)
            .ok_or_else(|| crate::errors::Error::rejected("invalid_display_frame"))?;
        Ok((entity, viewport))
    };
    let (source_display, _source_viewport) = context(source_display_id)?;
    let (target_display, target_viewport) = context(target_display_id)?;
    let layout = |id, display| -> crate::errors::Result<_> {
        let mut candidates = strips.iter().filter(|(strip, _)| strip.id() == id);
        let (strip, parent) = candidates
            .next()
            .ok_or_else(|| crate::errors::Error::rejected("layout_not_found"))?;
        if candidates.next().is_some() || parent.parent() != display {
            return Err(crate::errors::Error::rejected(
                "layout_ownership_unresolved",
            ));
        }
        Ok(strip)
    };
    let source_strip = layout(source_id, source_display)?;
    let destination_strip = layout(target_id, target_display)?;
    if state.is_tiled() && !windows.layout_column_is_writable(source_strip, entity) {
        return Err(crate::errors::Error::rejected("column_unavailable"));
    }
    let frame = windows
        .requested_frame(entity)
        .ok_or_else(|| crate::errors::Error::rejected("geometry_unavailable"))?;
    let size = if state.is_tiled() {
        let index = source_strip.index_of(entity)?;
        Size::new(
            source_strip
                .width_for_viewport(index, Some(target_viewport.width()))
                .map_err(|_| crate::errors::Error::rejected("width_projection_blocked"))?
                .slot,
            target_viewport.height(),
        )
    } else {
        frame.size()
    };
    let origin = centered_origin_in_viewport(target_viewport, size, target_viewport)
        .with_y(target_viewport.min.y);
    let target = checked_window_frame(origin, size)
        .ok_or_else(|| crate::errors::Error::rejected("invalid_geometry"))?;
    let members = if state.is_tiled() {
        source_strip
            .tab_group(entity)
            .unwrap_or_else(|| vec![entity])
    } else {
        vec![entity]
    };
    let memberships = topology.observe_memberships(&manager).map_err(|error| {
        crate::errors::Error::rejection_with_cause("native_operation_rejected", error)
    })?;
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
        return Err(crate::errors::Error::rejected("member_unavailable"));
    }
    let mut source = source_strip.clone();
    let mut destination = destination_strip.clone();
    if state.is_tiled() {
        let selected = members.iter().copied().collect();
        let mut transferred = source.take_windows_preserving_layout(&selected);
        destination.append_strip(&mut transferred);
        if !destination.width_budget_is_valid() {
            return Err(crate::errors::Error::rejected("invalid_layout_geometry"));
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
    // Moving an unfocused window with --stay must not steal focus on completion.
    let source_neighbour = focus
        .snapshot()
        .confirmed_entity()
        .filter(|focused| members.contains(focused))
        .and_then(|_| {
            focus.restoration_entity(source_id, eligible).or_else(|| {
                source
                    .all_columns()
                    .into_iter()
                    .find(|candidate| eligible(*candidate))
            })
        });
    transactions.submit_display_move(
        DisplayMovePlan {
            members,
            target,
            viewport: target_viewport,
            target_space_id: target_id,
            source_display_id,
            target_display_id,
            follow: (move_focus == MoveFocus::Follow).then_some(entity),
            source_neighbour,
            tiled: state.is_tiled(),
        },
        &windows,
        source_strip,
        &mut commands,
        time.elapsed(),
    )
}

//! Resolve display transfers without borrowing the active display as the target.

use bevy::prelude::*;

use super::admission::Admission;
use super::display_navigation::{DisplayTarget, select_display};
use super::{MoveFocus, checked_window_frame};
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::centered_origin_in_viewport;
use crate::ecs::native_space::{DisplayMovePlan, NativeSpaceTransactions};
use crate::ecs::params::Windows;
use crate::manager::Size;

type DisplayTransferBlockedWindows<'w, 's> = Query<
    'w,
    's,
    Entity,
    Or<(
        With<crate::ecs::native_space::NativeMoveOwner>,
        With<crate::ecs::workspace::WindowSpaceReassignmentPending>,
        With<crate::ecs::WindowDefaultsPending>,
    )>,
>;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn execute(
    In((window_id, move_focus, destination)): In<(i32, MoveFocus, DisplayTarget)>,
    windows: Windows,
    focus: Res<FocusCoordinator>,
    moving: DisplayTransferBlockedWindows,
    mut admission: Admission,
    mut transactions: ResMut<NativeSpaceTransactions>,
    time: Res<Time>,
    mut commands: Commands,
) -> crate::errors::Result<()> {
    let entity = admission.writable_window(window_id)?;
    let (_, _, state) = windows
        .get_tracked(entity)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    let source_id = admission.visible_space(window_id)?;
    let source_display_id = admission.visible_display_for_space(source_id)?;
    let Some(target_display_id) = select_display(
        source_display_id,
        destination,
        admission.known_displays().map(|(display, _)| display),
    ) else {
        return Ok(()); // A bounded display edge is an admitted no-op.
    };
    let target_id = admission.target_space(target_display_id)?;
    let (source_display, _source_viewport) = admission.display_context(source_display_id)?;
    let (target_display, target_viewport) = admission.display_context(target_display_id)?;
    let source_strip = admission.owned_strip(source_id, source_display)?;
    let destination_strip = admission.owned_strip(target_id, target_display)?;
    if state.is_tiled() {
        admission.writable_column(source_strip, entity)?;
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
    let blocked = moving.iter().collect::<Vec<_>>();
    let memberships = admission.memberships()?;
    if members.iter().any(|member| {
        blocked.contains(member)
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
    // Moving an unfocused window with --stay must not steal focus on completion.
    let source_neighbour = focus
        .snapshot()
        .confirmed_entity()
        .filter(|focused| members.contains(focused))
        .and_then(|_| {
            focus
                .restoration_entity(source_id, |candidate| {
                    admission.remaining_member(
                        &source,
                        candidate,
                        &blocked,
                        &memberships,
                        source_id,
                    )
                })
                .or_else(|| {
                    source.all_columns().into_iter().find(|candidate| {
                        admission.remaining_member(
                            &source,
                            *candidate,
                            &blocked,
                            &memberships,
                            source_id,
                        )
                    })
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

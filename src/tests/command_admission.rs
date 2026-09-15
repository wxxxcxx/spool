use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use super::*;
use crate::commands::admission::{Admission, Rejection};
use crate::ecs::ActiveWorkspaceMarker;
use crate::ecs::WindowVisibility;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::NativeMoveOwner;
use crate::ecs::params::Windows;

fn harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
}

/// The strip view the shared recipes accept, built from the test's own query, so a
/// test asks the same question production asks instead of a second copy of it.
type Strips<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LayoutStrip,
        Option<&'static ChildOf>,
        Has<ActiveWorkspaceMarker>,
    ),
>;

fn strip_of(space: u64, strips: &Strips) -> Result<LayoutStrip, Rejection> {
    Admission::space_scope_strip_in(
        strips.iter().map(|(_, strip, _, active)| (active, strip)),
        Some(space),
    )
    .cloned()
}

fn window(In(window_id): In<i32>, admission: Admission) -> Result<Entity, Rejection> {
    admission.window(window_id).map(|(_, entity)| entity)
}

fn writable_window(In(window_id): In<i32>, admission: Admission) -> Result<Entity, Rejection> {
    admission.writable_window(window_id)
}

fn focus_target(windows: Windows, focus: Res<FocusCoordinator>) -> Result<Entity, Rejection> {
    Admission::focused_target_in(&windows, &focus)
}

fn unique_strip(In(space): In<u64>, strips: Strips) -> Result<(), Rejection> {
    strip_of(space, &strips).map(|_| ())
}

fn target_space(In(display_id): In<u32>, admission: Admission) -> Result<u64, Rejection> {
    admission.target_space(display_id)
}

fn column_index(
    In((space, ordinal)): In<(u64, usize)>,
    strips: Strips,
) -> Result<usize, Rejection> {
    let strip = strip_of(space, &strips)?;
    Admission::column_index(&strip, ordinal)
}

fn eligible_window_column(
    In(window_id): In<i32>,
    windows: Windows,
    admission: Admission,
    strips: Strips,
) -> Result<(), Rejection> {
    let (_, entity) = admission.window(window_id)?;
    let strip = strip_of(TEST_WORKSPACE_ID, &strips)?;
    let index = strip
        .index_of(entity)
        .map_err(|_| Rejection::ColumnUnavailable)?;
    Admission::eligible_column_in(&windows, &strip, index)
}

fn writable_window_column(
    In(window_id): In<i32>,
    admission: Admission,
    strips: Strips,
) -> Result<(), Rejection> {
    let (_, entity) = admission.window(window_id)?;
    let strip = strip_of(TEST_WORKSPACE_ID, &strips)?;
    admission.writable_column(&strip, entity)
}

fn fullscreen_column(In(entity): In<Entity>, windows: Windows) -> Result<(), Rejection> {
    let strip = LayoutStrip::fullscreen(TEST_WORKSPACE_ID, entity);
    Admission::eligible_column_in(&windows, &strip, 0)?;
    Admission::tiled_column(&strip, entity)
}

fn require_topology(mut admission: Admission) -> Result<(), Rejection> {
    admission.require_topology()
}

fn visible_space(In(window_id): In<i32>, mut admission: Admission) -> Result<u64, Rejection> {
    admission.visible_space(window_id)
}

#[test]
fn missing_window_is_not_found() {
    let mut harness = harness();
    let result = harness.world().run_system_once_with(window, 999).unwrap();
    assert_eq!(result, Err(Rejection::WindowNotFound));
}

#[test]
fn invisible_window_is_unavailable() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(WindowVisibility::Hidden);
    let result = harness
        .world()
        .run_system_once_with(writable_window, 0)
        .unwrap();
    assert_eq!(result, Err(Rejection::WindowUnavailable));
}

#[test]
fn in_flight_window_is_not_writable() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(NativeMoveOwner);
    let result = harness
        .world()
        .run_system_once_with(writable_window, 0)
        .unwrap();
    assert_eq!(result, Err(Rejection::WindowUnavailable));
}

#[test]
fn in_flight_column_is_transition_pending() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(NativeMoveOwner);
    let result = harness
        .world()
        .run_system_once_with(eligible_window_column, 0)
        .unwrap();
    assert_eq!(result, Err(Rejection::LayoutTransitionPending));
}

#[test]
fn in_flight_column_is_not_writable() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(NativeMoveOwner);
    let result = harness
        .world()
        .run_system_once_with(writable_window_column, 0)
        .unwrap();
    assert_eq!(result, Err(Rejection::ColumnUnavailable));
}

#[test]
fn absent_strip_is_not_found() {
    let mut harness = harness();
    let result = harness
        .world()
        .run_system_once_with(unique_strip, 9999)
        .unwrap();
    assert_eq!(result, Err(Rejection::LayoutNotFound));
}

#[test]
fn duplicate_strip_is_ambiguous() {
    let mut harness = harness();
    harness.world().spawn(LayoutStrip::new(TEST_WORKSPACE_ID));
    let result = harness
        .world()
        .run_system_once_with(unique_strip, TEST_WORKSPACE_ID)
        .unwrap();
    assert_eq!(result, Err(Rejection::AmbiguousLayout));
}

#[test]
fn column_ordinal_zero_is_out_of_range() {
    let mut harness = harness();
    let result = harness
        .world()
        .run_system_once_with(column_index, (TEST_WORKSPACE_ID, 0))
        .unwrap();
    assert_eq!(result, Err(Rejection::ColumnOutOfRange));
}

#[test]
fn column_beyond_end_is_out_of_range() {
    let mut harness = harness();
    let result = harness
        .world()
        .run_system_once_with(column_index, (TEST_WORKSPACE_ID, 99))
        .unwrap();
    assert_eq!(result, Err(Rejection::ColumnOutOfRange));
}

#[test]
fn fullscreen_column_is_ineligible() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    let result = harness
        .world()
        .run_system_once_with(fullscreen_column, entity)
        .unwrap();
    assert_eq!(result, Err(Rejection::IneligibleLayoutEntry));
}

#[test]
fn incomplete_topology_is_unresolved() {
    let mut harness = harness();
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
    let result = harness.world().run_system_once(require_topology).unwrap();
    assert_eq!(result, Err(Rejection::TopologyUnresolved));
}

#[test]
fn unresolvable_window_space_is_not_visible() {
    let mut harness = harness();
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
    let result = harness
        .world()
        .run_system_once_with(visible_space, 0)
        .unwrap();
    assert_eq!(result, Err(Rejection::SpaceNotVisible));
}

#[test]
fn no_focused_window_is_rejected() {
    let mut harness = TestHarness::new().with_windows(1).without_focused_window();
    harness.pump_frames(10);
    let result = harness.world().run_system_once(focus_target).unwrap();
    assert_eq!(result, Err(Rejection::NoFocusedWindow));
}

#[test]
fn an_unreachable_target_is_not_a_fullscreen_one() {
    let mut harness = harness();

    // Nothing is visible on a display that is not there: that is "not now".
    let result = harness
        .world()
        .run_system_once_with(target_space, 999)
        .unwrap();
    assert_eq!(result, Err(Rejection::SpaceNotVisible));

    // A fullscreen Space is not a user Space at all, which is a different answer.
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, true);
    harness.pump_frames(2);
    let result = harness
        .world()
        .run_system_once_with(target_space, TEST_DISPLAY_ID)
        .unwrap();
    assert_eq!(result, Err(Rejection::FullscreenSpace));
}

#[test]
fn rejection_codes_match_the_ipc_contract() {
    for (rejection, code) in [
        (Rejection::WindowNotFound, "window_not_found"),
        (Rejection::WindowUnavailable, "window_unavailable"),
        (Rejection::NoFocusedWindow, "no_focused_window"),
        (
            Rejection::FocusedWindowOutsideSpace,
            "focused_window_outside_space",
        ),
        (Rejection::SpaceNotVisible, "space_not_visible"),
        (Rejection::FullscreenSpace, "fullscreen_space"),
        (Rejection::LayoutNotFound, "layout_not_found"),
        (Rejection::AmbiguousLayout, "ambiguous_layout"),
        (
            Rejection::LayoutTransitionPending,
            "layout_transition_pending",
        ),
        (Rejection::ColumnOutOfRange, "column_out_of_range"),
        (Rejection::ColumnUnavailable, "column_unavailable"),
        (Rejection::IneligibleLayoutEntry, "ineligible_layout_entry"),
        (Rejection::TopologyUnresolved, "topology_unresolved"),
    ] {
        assert_eq!(rejection.code(), code);
        assert_eq!(crate::errors::Error::from(rejection).admission_code(), code);
    }
}

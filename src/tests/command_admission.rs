use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use super::*;
use crate::commands::admission::{Admission, Rejection};
use crate::ecs::WindowVisibility;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::NativeMoveOwner;

fn harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
}

fn window(In(window_id): In<i32>, admission: Admission) -> Result<Entity, Rejection> {
    admission.window(window_id).map(|(_, entity)| entity)
}

fn writable_window(In(window_id): In<i32>, admission: Admission) -> Result<Entity, Rejection> {
    admission.writable_window(window_id)
}

fn focus_target(admission: Admission) -> Result<Entity, Rejection> {
    admission.focus_target()
}
fn unique_strip(In(space): In<u64>, admission: Admission) -> Result<(), Rejection> {
    admission.unique_strip(space).map(|_| ())
}

fn column_index(
    In((space, ordinal)): In<(u64, usize)>,
    admission: Admission,
) -> Result<usize, Rejection> {
    let strip = admission.unique_strip(space)?;
    Admission::column_index(strip, ordinal)
}

fn eligible_window_column(In(window_id): In<i32>, admission: Admission) -> Result<(), Rejection> {
    let (_, entity) = admission.window(window_id)?;
    let strip = admission.unique_strip(TEST_WORKSPACE_ID)?;
    let index = strip
        .index_of(entity)
        .map_err(|_| Rejection::ColumnUnavailable)?;
    admission.eligible_column(strip, index)
}

fn writable_window_column(In(window_id): In<i32>, admission: Admission) -> Result<(), Rejection> {
    let (_, entity) = admission.window(window_id)?;
    let strip = admission.unique_strip(TEST_WORKSPACE_ID)?;
    admission.writable_column(strip, entity)
}

fn fullscreen_column(In(entity): In<Entity>, admission: Admission) -> Result<(), Rejection> {
    let strip = LayoutStrip::fullscreen(TEST_WORKSPACE_ID, entity);
    admission.eligible_column(&strip, 0)?;
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

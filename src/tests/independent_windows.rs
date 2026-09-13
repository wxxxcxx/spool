use bevy::prelude::*;

use super::*;
use crate::ecs::SpawnWindowTrigger;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::manager::Application;

fn published(harness: &TestHarness, id: i32, published: bool) {
    harness.mock_state.update_window(id, |window| {
        window.published = published;
        window.ordered_out = !published;
        window.visible = published;
    });
}

#[test]
fn startup_background_identities_do_not_become_layout_windows() {
    let mut harness = TestHarness::new();
    harness.pump_frames(10);
    let application = harness
        .world()
        .query_filtered::<Entity, With<Application>>()
        .single(harness.world())
        .unwrap();
    let windows = (0..3)
        .map(|id| {
            harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                id,
                IRect::new(20, 30, 420, 700),
            )
        })
        .collect();
    published(&harness, 0, false);
    published(&harness, 1, false);
    harness.mock_state.focus_window(2);
    harness
        .world()
        .trigger(SpawnWindowTrigger::for_application(application, windows));
    harness.pump_frames(40);
    assert_eq!(harness.mock_state.position_write_attempts(0), 0);
    assert_eq!(harness.mock_state.position_write_attempts(1), 0);
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .single(harness.world())
        .unwrap();
    assert_eq!(
        strip.len(),
        1,
        "three native identities must occupy one ordinary column"
    );
    assert!(matches!(strip.get(0), Ok(Column::Single(_))));
    assert_eq!(
        harness
            .world()
            .query::<&Window>()
            .iter(harness.world())
            .count(),
        1
    );
}

#[test]
fn selected_identity_reuses_the_existing_ordinary_window_entity() {
    let mut harness = TestHarness::new().with_windows(1).with_focused_window(0);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let next = harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
    published(&harness, 0, false);
    harness.mock_state.focus_window(1);
    harness.world().trigger(SpawnWindowTrigger::new(vec![next]));
    harness.pump_frames(80);
    assert_eq!(
        find_window_entity(1, harness.world()),
        original,
        "a change of native control target must not allocate another layout window"
    );
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .single(harness.world())
        .unwrap();
    assert_eq!(strip.len(), 1);
    assert!(matches!(strip.get(0), Ok(Column::Single(entity)) if entity == original));
}

#[test]
fn retained_native_owner_follows_selection_without_a_creation_event() {
    let mut harness = TestHarness::new().with_windows(1).with_focused_window(0);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let old_writes = harness.mock_state.position_write_attempts(0);
    harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        1,
        IRect::new(40, 50, 740, 700),
    );
    published(&harness, 0, false);
    harness
        .mock_state
        .update_window(0, |window| window.represented_window_id = Some(1));
    harness.mock_state.focus_window(1);
    harness.pump_frames(80);
    assert_eq!(harness.mock_state.position_write_attempts(0), old_writes);
    assert_eq!(find_window_entity(1, harness.world()), original);
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .single(harness.world())
        .unwrap();
    assert_eq!(strip.len(), 1);
    assert!(matches!(strip.get(0), Ok(Column::Single(_))));
}

#[test]
fn native_selection_does_not_raise_other_independent_windows() {
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    harness.mock_state.take_raise_requests();
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 2, frame);
    let mut previous = 0;
    for next in [2, 0, 2, 0] {
        published(&harness, previous, false);
        published(&harness, next, true);
        harness
            .mock_state
            .update_window(previous, |window| window.represented_window_id = Some(next));
        harness
            .mock_state
            .update_window(next, |window| window.represented_window_id = None);
        harness.mock_state.focus_window(next);
        harness
            .world()
            .write_message(crate::events::Event::FocusRevalidationRequested {
                pid: TEST_PROCESS_ID,
                source: crate::events::FocusSource::AccessibilityUiElement,
            });
        harness.pump_frames(80);
        assert_eq!(find_window_entity(next, harness.world()), original);
        assert!(
            harness.mock_state.take_raise_requests().is_empty(),
            "changing a native control target must not raise the independent sibling window"
        );
        previous = next;
    }
}

#[test]
fn detaching_the_selected_root_preserves_the_remaining_window_slot() {
    let mut harness = TestHarness::new().with_windows(1).with_focused_window(0);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
    // Both roots are now independently published. The retained chrome stayed
    // with the original physical window, whose new control target is 1.
    harness
        .mock_state
        .update_window(0, |window| window.represented_window_id = Some(1));
    harness.pump_frames(100);
    assert_eq!(find_window_entity(1, harness.world()), original);
    assert_ne!(find_window_entity(0, harness.world()), original);
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .single(harness.world())
        .unwrap();
    assert_eq!(strip.len(), 2);
    assert!(
        strip
            .columns()
            .all(|column| matches!(column, Column::Single(_)))
    );
}

#[test]
fn same_frame_public_windows_are_never_automatically_grouped() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let next = harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
    harness.mock_state.window_visible(0, false);
    harness.world().trigger(SpawnWindowTrigger::new(vec![next]));
    harness.pump_frames(60);
    assert_ne!(find_window_entity(1, harness.world()), original);
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .single(harness.world())
        .unwrap();
    assert_eq!(strip.len(), 2);
    assert!(
        strip
            .columns()
            .all(|column| matches!(column, Column::Single(_)))
    );
}

#[test]
fn closed_window_is_not_rebound_to_a_same_frame_replacement() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let next = harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
    published(&harness, 0, false);
    harness
        .mock_state
        .omit_window_from_window_server_inventory(0, true);
    harness.world().trigger(SpawnWindowTrigger::new(vec![next]));
    harness.pump_frames(60);
    assert_ne!(find_window_entity(1, harness.world()), original);
}

#[test]
fn simultaneous_public_roots_do_not_guess_a_geometry_replacement() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let windows = (1..=2)
        .map(|id| {
            harness
                .mock_state
                .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, id, frame)
        })
        .collect();
    published(&harness, 0, false);
    harness.world().trigger(SpawnWindowTrigger::new(windows));
    harness.pump_frames(60);
    assert_ne!(find_window_entity(1, harness.world()), original);
    assert_ne!(find_window_entity(2, harness.world()), original);
    assert_ne!(
        find_window_entity(1, harness.world()),
        find_window_entity(2, harness.world())
    );
}

#[test]
fn incomplete_publication_does_not_rebind_an_existing_window() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let next = harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
    published(&harness, 0, false);
    harness
        .mock_state
        .update_window(0, |window| window.represented_window_id = Some(1));
    harness
        .mock_state
        .set_application_inventory_complete(TEST_PROCESS_ID, false);
    harness.world().trigger(SpawnWindowTrigger::new(vec![next]));
    harness.pump_frames(60);
    assert_eq!(find_window_entity(0, harness.world()), original);
    assert_ne!(find_window_entity(1, harness.world()), original);
}

#[test]
fn merging_published_windows_never_duplicates_the_control_target() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(30);
    published(&harness, 0, false);
    harness
        .mock_state
        .update_window(0, |window| window.represented_window_id = Some(1));
    harness.pump_frames(100);
    assert_eq!(
        harness
            .world()
            .query::<&Window>()
            .iter(harness.world())
            .filter(|window| window.id() == 1)
            .count(),
        1
    );
    let frame = harness.mock_state.actual_window_frame(1).unwrap();
    assert!(frame.width() > 0);
}

#[test]
fn complete_inventory_recovers_duplicate_slots_without_writing_the_dormant_root() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(2);
    harness.pump_frames(30);
    let dormant = find_window_entity(0, harness.world());
    published(&harness, 0, false);
    harness
        .mock_state
        .set_application_inventory_complete(TEST_PROCESS_ID, false);
    harness.pump_frames(30);
    // Finder's desktop used to keep this inventory incomplete indefinitely.
    harness
        .mock_state
        .set_application_inventory_complete(TEST_PROCESS_ID, true);
    harness.pump_frames(60);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(dormant)
            .is_some_and(crate::ecs::reconcile::WindowUnavailable::excludes_from_layout_projection)
    );
    let writes = harness.mock_state.frame_write_attempts(0);
    let positions = harness.mock_state.position_write_attempts(0);
    harness.pump_frames(80);
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    assert_eq!(harness.mock_state.position_write_attempts(0), positions);
    let left = harness.mock_state.actual_window_frame(1).unwrap();
    let right = harness.mock_state.actual_window_frame(2).unwrap();
    assert_eq!(right.min.x - left.min.x, TEST_DISPLAY_WIDTH / 4);
}

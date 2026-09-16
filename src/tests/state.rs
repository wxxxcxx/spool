use bevy::ecs::system::RunSystemOnce as _;

use crate::ecs::layout::LayoutStrip;
use crate::ecs::state::QueryStateParams;
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_WORKSPACE_ID, TestHarness,
};
use spool_shared_types::state::StateQueryKind;

const FLOAT_SPACE: u64 = TEST_WORKSPACE_ID + 1;

#[test]
fn window_set_distinguishes_visible_spaces_from_the_globally_active_display() {
    use crate::tests::{EXT_DISPLAY_ID, EXT_WORKSPACE_ID};
    let mut harness = TestHarness::new()
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            bevy::math::IRect::new(1024, 0, 2944, 1200),
            vec![EXT_WORKSPACE_ID, EXT_WORKSPACE_ID + 1],
        )
        .with_workspace_window(1, EXT_WORKSPACE_ID, |_| {})
        .with_focused_window(0);
    harness.pump_frames(10);
    let set = harness
        .world()
        .run_system_once(|state: QueryStateParams| state.extract_window_set())
        .unwrap();
    assert_eq!(set.current().unwrap().space_id, TEST_WORKSPACE_ID);
    assert!(set.workspace(TEST_WORKSPACE_ID).unwrap().active);
    assert!(set.workspace(EXT_WORKSPACE_ID).unwrap().active);
    assert!(!set.workspace(EXT_WORKSPACE_ID + 1).unwrap().active);
    assert!(
        !set.displays()
            .iter()
            .find(|display| display.id == EXT_DISPLAY_ID)
            .unwrap()
            .active
    );
}

#[test]
fn window_set_current_requires_a_known_native_visibility_observation() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(crate::ecs::topology::NativeTopology::default());
    let set = harness
        .world()
        .run_system_once(|state: QueryStateParams| state.extract_window_set())
        .unwrap();
    assert!(set.current().is_none());
    assert!(
        set.workspace(TEST_WORKSPACE_ID).is_some(),
        "unknown visibility does not remove the retained layout"
    );
}

#[test]
fn window_set_uses_observed_display_activity_instead_of_retained_markers() {
    use crate::tests::{EXT_DISPLAY_ID, EXT_WORKSPACE_ID};
    let mut harness = TestHarness::new().with_windows(1).with_display(
        EXT_DISPLAY_ID,
        bevy::math::IRect::new(1024, 0, 2944, 1200),
        vec![EXT_WORKSPACE_ID],
    );
    harness.pump_frames(10);
    let original = layout_snapshot(&mut harness);
    assert_eq!(original.current().unwrap().space_id, TEST_WORKSPACE_ID);
    for (response, expected) in [
        (Err(()), None),
        (Ok(u32::MAX), None),
        (Ok(EXT_DISPLAY_ID), Some(EXT_WORKSPACE_ID)),
    ] {
        harness.mock_state.script_active_display_queries([response]);
        harness
            .world()
            .run_system_once(crate::ecs::topology::gather_initial_topology)
            .unwrap();
        let set = layout_snapshot(&mut harness);
        assert_eq!(set.current().map(|space| space.space_id), expected);
        assert_eq!(
            set.displays()
                .iter()
                .filter(|display| display.active)
                .count(),
            usize::from(expected.is_some())
        );
        assert!(set.workspace(TEST_WORKSPACE_ID).unwrap().active);
        assert!(set.workspace(EXT_WORKSPACE_ID).unwrap().active);
        assert!(set.window(0).unwrap().visible);
        harness
            .world()
            .run_system_once(crate::ecs::topology::gather_initial_topology)
            .unwrap();
        assert_eq!(
            layout_snapshot(&mut harness).current().unwrap().space_id,
            TEST_WORKSPACE_ID
        );
    }
}

fn floating_snapshot_harness(space: u64) -> TestHarness {
    let config = crate::config::Config::try_from(
        r#"{"windows":{"float":{"title":"^Floating$","floating":true}}}"#,
    )
    .unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_display(
            TEST_DISPLAY_ID,
            bevy::math::IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, FLOAT_SPACE],
        )
        .with_windows(1)
        .with_workspace_window(1, space, |window| window.title = "Floating".into())
        .with_workspace_window(2, space, |window| window.title = "Floating".into());
    harness.pump_frames(10);
    let entity = crate::tests::find_window_entity(1, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(entity)
            .is_some()
    );
    harness
}

fn layout_snapshot(harness: &mut TestHarness) -> spool_shared_types::windowset::WindowSet {
    harness
        .world()
        .run_system_once(|state: QueryStateParams| state.extract_window_set())
        .unwrap()
}

fn assert_query_window_matches_snapshot(harness: &mut TestHarness, id: crate::platform::WinID) {
    let snapshot = layout_snapshot(harness);
    let expected = snapshot.window(id).expect("snapshot window");
    let state = harness
        .world()
        .run_system_once(extract_query_state)
        .unwrap()
        .unwrap();
    let window = state
        .spaces
        .iter()
        .flat_map(|space| &space.windows)
        .find(|window| window.window_id == id)
        .expect("query window");
    assert_eq!(window.visible, expected.visible);
    assert_eq!(window.frame, expected.frame);
    assert_eq!(window.floating, expected.floating);
    assert_eq!(window.focused, expected.focused);
    assert_eq!(
        state
            .on_screen()
            .iter()
            .any(|window| window.window_id == id),
        expected.visible
    );
}

#[test]
fn window_set_includes_floating_windows_on_inactive_spaces() {
    let mut harness = floating_snapshot_harness(FLOAT_SPACE);
    let snapshot = layout_snapshot(&mut harness);
    assert_eq!(
        snapshot.workspace_of(1).map(|space| space.space_id),
        Some(FLOAT_SPACE)
    );
    assert!(snapshot.window(1).unwrap().floating);
    assert!(!snapshot.window(1).unwrap().visible);
    assert!(snapshot.workspace(FLOAT_SPACE).unwrap().columns.is_empty());
    assert_query_window_matches_snapshot(&mut harness, 1);
}

#[test]
fn window_set_does_not_mark_inactive_tiled_windows_visible_from_geometry_alone() {
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            bevy::math::IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, FLOAT_SPACE],
        )
        .with_windows(1)
        .with_workspace_window(1, FLOAT_SPACE, |_| {});
    harness.pump_frames(10);
    let snapshot = layout_snapshot(&mut harness);
    assert_eq!(snapshot.workspace_of(1).unwrap().space_id, FLOAT_SPACE);
    assert!(!snapshot.window(1).unwrap().visible);
    assert!(snapshot.window(0).unwrap().visible);
    assert_query_window_matches_snapshot(&mut harness, 1);
}

#[test]
fn window_set_keeps_hidden_floating_records_without_marking_them_visible() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    let entity = crate::tests::find_window_entity(1, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::WindowVisibility::Hidden);
    let snapshot = layout_snapshot(&mut harness);
    let window = snapshot.window(1).expect("hidden window is still tracked");
    assert!(window.floating);
    assert!(!window.visible);
    assert_query_window_matches_snapshot(&mut harness, 1);
}

#[test]
fn window_set_does_not_guess_ambiguous_floating_membership() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0, 1])]);
    harness
        .mock_state
        .script_workspace_membership_queries(FLOAT_SPACE, [Ok(vec![1])]);
    let snapshot = layout_snapshot(&mut harness);
    assert!(snapshot.window(1).is_none());
    assert!(snapshot.window(0).is_some(), "tiled projection is retained");
}

#[test]
fn window_set_rejects_partial_floating_membership_then_recovers() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    let entity = crate::tests::find_window_entity(1, harness.world());
    let frame = harness.mock_state.actual_window_frame(1);
    let writes = harness.mock_state.frame_write_attempts(1);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0, 1])]);
    harness
        .mock_state
        .script_workspace_membership_queries(FLOAT_SPACE, [Err(())]);
    assert!(layout_snapshot(&mut harness).window(1).is_none());
    assert!(
        harness
            .world()
            .get::<crate::manager::Window>(entity)
            .is_some()
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(entity)
            .is_some()
    );
    assert_eq!(harness.mock_state.actual_window_frame(1), frame);
    assert_eq!(harness.mock_state.frame_write_attempts(1), writes);
    let recovered = layout_snapshot(&mut harness);
    assert_eq!(
        recovered.workspace_of(1).unwrap().space_id,
        TEST_WORKSPACE_ID
    );
}

#[test]
fn window_set_deduplicates_floating_membership_without_reordering_it() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![2, 1, 2, 1, 0])]);
    let snapshot = layout_snapshot(&mut harness);
    assert_eq!(
        snapshot.windows().filter(|window| window.id == 1).count(),
        1
    );
    assert_eq!(
        snapshot
            .workspace(TEST_WORKSPACE_ID)
            .unwrap()
            .floating
            .iter()
            .map(|window| window.id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[test]
fn window_set_distinguishes_known_membership_from_unknown_visibility() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    harness
        .mock_state
        .script_active_space_queries(TEST_DISPLAY_ID, [Err(())]);
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    let snapshot = layout_snapshot(&mut harness);
    assert_eq!(
        snapshot.workspace_of(1).unwrap().space_id,
        TEST_WORKSPACE_ID
    );
    assert!(!snapshot.window(1).unwrap().visible);
    assert!(!snapshot.window(0).unwrap().visible);
    assert_query_window_matches_snapshot(&mut harness, 0);
    assert_query_window_matches_snapshot(&mut harness, 1);
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    assert!(layout_snapshot(&mut harness).window(1).unwrap().visible);
    assert_query_window_matches_snapshot(&mut harness, 1);
}

#[test]
fn public_query_requires_complete_unique_floating_membership() {
    for other_members in [Ok(vec![1]), Err(())] {
        let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
        let frame = harness.mock_state.actual_window_frame(1);
        let writes = harness.mock_state.frame_write_attempts(1);
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0, 1])]);
        harness
            .mock_state
            .script_workspace_membership_queries(FLOAT_SPACE, [other_members]);
        let query = harness
            .world()
            .run_system_once(extract_query_state)
            .unwrap()
            .unwrap();
        assert!(
            !query
                .spaces
                .iter()
                .flat_map(|space| &space.windows)
                .any(|window| window.window_id == 1)
        );
        assert!(
            query
                .spaces
                .iter()
                .flat_map(|space| &space.windows)
                .any(|window| window.window_id == 0)
        );
        assert_eq!(harness.mock_state.actual_window_frame(1), frame);
        assert_eq!(harness.mock_state.frame_write_attempts(1), writes);
        assert_query_window_matches_snapshot(&mut harness, 1);
    }
}

#[test]
fn public_query_uses_native_floating_membership_instead_of_a_retained_column() {
    let mut harness = floating_snapshot_harness(FLOAT_SPACE);
    let entity = crate::tests::find_window_entity(1, harness.world());
    let world = harness.world();
    world
        .query::<&mut LayoutStrip>()
        .iter_mut(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap()
        .append(entity);
    let query = harness
        .world()
        .run_system_once(extract_query_state)
        .unwrap()
        .unwrap();
    let owners = query
        .spaces
        .iter()
        .filter(|space| space.windows.iter().any(|window| window.window_id == 1))
        .map(|space| space.space_id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![FLOAT_SPACE]);
}

#[test]
fn window_set_defers_floating_membership_on_incomplete_topology() {
    let mut harness = floating_snapshot_harness(TEST_WORKSPACE_ID);
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Err(())]);
    let snapshot = layout_snapshot(&mut harness);
    assert!(snapshot.window(1).is_none());
    assert!(snapshot.window(0).is_some());
    assert!(
        harness
            .world()
            .resource::<crate::manager::WindowManager>()
            .windows_in_workspace(TEST_WORKSPACE_ID)
            .is_err(),
        "incomplete topology must not start a membership scan"
    );
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    assert!(layout_snapshot(&mut harness).window(1).is_some());
}

#[test]
fn window_set_without_floats_does_not_scan_native_memberships() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Err(())]);
    assert!(layout_snapshot(&mut harness).window(0).is_some());
    assert!(
        harness
            .world()
            .resource::<crate::manager::WindowManager>()
            .windows_in_workspace(TEST_WORKSPACE_ID)
            .is_err()
    );
}

/// Systems that tick a window's cached frame or a lifecycle timer re-mark the
/// `Window` component without changing anything the Bar draws. The Bar's
/// projection must not follow that: re-drawing it costs a native membership
/// scan, so an incidental touch would put that scan back on the frame rate.
#[test]
fn incidental_window_mutation_does_not_dirty_the_bar_projection() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);
    let dirty = harness
        .world()
        .register_system(crate::ecs::bar_projection_dirty);
    // A system's first run sees everything that already existed as new.
    harness.world().run_system(dirty).unwrap();
    assert!(
        !harness.world().run_system(dirty).unwrap(),
        "an idle world is not a dirty Bar"
    );

    harness
        .world()
        .run_system_once(
            |mut windows: bevy::prelude::Query<&mut crate::manager::Window>| {
                for mut window in &mut windows {
                    // Any `Mut<Window>` deref re-marks the component.
                    let _ = &mut *window;
                }
            },
        )
        .unwrap();
    assert!(
        !harness.world().run_system(dirty).unwrap(),
        "touching a Window component is not a Bar change"
    );

    let entity = crate::tests::find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::WindowVisibility::Hidden);
    assert!(
        harness.world().run_system(dirty).unwrap(),
        "hiding a window the Bar lists is a Bar change"
    );
}

#[test]
fn query_tokens_only_expose_v3_native_space_contract() {
    assert_eq!(StateQueryKind::tokens(), "state, spaces, active, on-screen");
    assert!(StateQueryKind::parse("virtual-workspaces").is_none());
}

#[test]
fn query_state_preserves_layout_column_order() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(15);

    let mut strips = harness.world().query::<&mut LayoutStrip>();
    strips
        .single_mut(harness.world())
        .expect("one layout strip")
        .swap(0, 1);
    harness.pump_frames(2);

    let state = harness
        .world()
        .run_system_once(extract_query_state)
        .expect("extract query state system")
        .expect("extract query state");
    let window_ids = state.spaces[0]
        .windows
        .iter()
        .map(|window| window.window_id)
        .collect::<Vec<_>>();
    assert_eq!(window_ids, vec![1, 0, 2]);
}

#[test]
fn query_state_uses_the_ecs_projection_when_platform_topology_is_temporarily_unavailable() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
    harness
        .mock_state
        .script_active_space_queries(TEST_DISPLAY_ID, [Err(())]);

    let state = harness
        .world()
        .run_system_once(extract_query_state)
        .expect("extract query state system")
        .expect("extract query state");

    assert_eq!(state.displays.len(), 1);
    assert_eq!(state.displays[0].visible_space_id, Some(TEST_WORKSPACE_ID));
    assert_eq!(state.spaces.len(), 1);
    assert_eq!(state.spaces[0].space_id, TEST_WORKSPACE_ID);
    assert_eq!(state.spaces[0].windows.len(), 1);
}

fn extract_query_state(
    params: QueryStateParams,
) -> crate::errors::Result<crate::ecs::state::SpoolQueryState> {
    params.extract()
}

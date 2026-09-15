use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::ecs::system::RunSystemOnce as _;

use crate::ecs::layout::LayoutStrip;
use crate::ecs::state::{
    QueryStateParams, SpoolState, StateFilePath, StatePersistence, periodic_state_save,
};
use crate::events::Event;
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

fn test_dir(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("spool-{name}-{}-{nonce}", std::process::id()))
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
fn test_harness_state_saves_never_touch_the_user_path() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    let path = harness
        .world()
        .resource::<StateFilePath>()
        .as_path()
        .to_path_buf();

    assert_ne!(path, SpoolState::default_state_file_path());
    harness
        .world()
        .run_system_once(periodic_state_save)
        .expect("periodic state save system");
    assert!(path.exists());

    drop(harness);
    assert!(!path.exists());
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

fn intent_snapshot(width: crate::ecs::layout::WidthIntent) -> SpoolState {
    use crate::ecs::layout::ColumnId;
    use crate::ecs::state::{INTENT_STATE_VERSION, SavedColumn, SavedSpace};
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 0,
        floating: Vec::new(),
        spaces: vec![SavedSpace {
            space_id: TEST_WORKSPACE_ID,
            columns: vec![SavedColumn {
                column_id: ColumnId(1),
                width,
                kind: spool_shared_types::windowset::ColumnKind::Single,
                items: vec![crate::ecs::state::SavedItem {
                    item_id: 1000,
                    weight: 1.0,
                    tabs: false,
                    members: Vec::new(),
                }],
            }],
        }],
    }
}

#[test]
fn intent_file_preserves_raw_values_and_rejects_old_formats_without_rewriting() {
    use crate::ecs::layout::WidthIntent;
    let dir = test_dir("raw-intent");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    let mut writer = StatePersistence::default();
    for width in [
        WidthIntent::Absolute(800.0),
        WidthIntent::ViewportRatio(0.75),
        WidthIntent::InheritConfig,
    ] {
        let snapshot = writer.capture(intent_snapshot(width)).unwrap();
        assert!(writer.commit(&snapshot, &path).unwrap());
        let candidate = SpoolState::load_from_file(&path).unwrap();
        assert_eq!(candidate.spaces[0].columns[0].width, width);
        let json = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "frame",
            "observed",
            "presented",
            "effective",
            "animation",
            "active",
            "displays",
        ] {
            assert!(
                !json.contains(forbidden),
                "unexpected persisted projection: {forbidden}"
            );
        }
    }
    for old in [
        r#"{"version":2,"workspaces":[]}"#,
        r#"{"version":3,"spaces":[]}"#,
        r#"{"version":4,"revision":1,"spaces":[]}"#,
        // v5 held member hints directly instead of one slot per retained
        // member; the member count is unrecoverable, so it is not migrated.
        r#"{"version":5,"revision":1,"spaces":[]}"#,
        "broken",
    ] {
        fs::write(&path, old).unwrap();
        assert!(SpoolState::load_from_file(&path).is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), old);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "loading must not migrate or back up old state"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stale_save_cannot_replace_newer_intent_and_failure_stays_dirty() {
    use crate::ecs::layout::WidthIntent;
    let dir = test_dir("save-revisions");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    let mut writer = StatePersistence::default();
    let older = writer
        .capture(intent_snapshot(WidthIntent::Absolute(800.0)))
        .unwrap();
    let newer = writer
        .capture(intent_snapshot(WidthIntent::Absolute(900.0)))
        .unwrap();
    assert!(writer.is_dirty());
    assert!(!writer.commit(&older, &path).unwrap());
    assert!(!path.exists());
    assert!(writer.commit(&newer, &path).unwrap());
    assert!(!writer.is_dirty());
    assert!(!writer.commit(&older, &path).unwrap());
    assert_eq!(SpoolState::load_from_file(&path).unwrap(), newer);
    let newest = writer
        .capture(intent_snapshot(WidthIntent::Absolute(950.0)))
        .unwrap();
    let blocker = dir.join("not-a-directory");
    fs::write(&blocker, "block").unwrap();
    assert!(writer.commit(&newest, &blocker.join("state.json")).is_err());
    assert!(writer.is_dirty());
    assert_eq!(writer.saved_revision(), Some(newer.revision));
    assert_eq!(SpoolState::load_from_file(&path).unwrap(), newer);
    assert!(writer.commit(&newest, &path).unwrap());
    assert_eq!(writer.saved_revision(), Some(newest.revision));
    assert!(!writer.is_dirty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_intent_snapshot_is_not_admitted_to_the_save_writer() {
    use crate::ecs::layout::WidthIntent;
    let mut writer = StatePersistence::default();
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            writer
                .capture(intent_snapshot(WidthIntent::Absolute(invalid)))
                .is_err()
        );
    }
    assert_eq!(writer.accepted_revision(), 0);
    assert!(!writer.is_dirty());
}

#[test]
fn accepted_width_can_be_saved_while_native_topology_is_unavailable() {
    use crate::ecs::layout::WidthIntent;
    use crate::ecs::topology::NativeTopology;
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    let path = harness
        .world()
        .resource::<StateFilePath>()
        .as_path()
        .to_path_buf();
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(1);
    assert!(!harness.world().resource::<NativeTopology>().is_complete());
    {
        let mut query = harness.world().query::<&mut LayoutStrip>();
        let mut strip = query.single_mut(harness.world()).unwrap();
        let id = strip.column_state(0).unwrap().id;
        strip
            .set_width_intent(id, WidthIntent::Absolute(800.0))
            .unwrap();
    }
    harness
        .world()
        .run_system_once(periodic_state_save)
        .unwrap();
    let saved = SpoolState::load_from_file(&path).unwrap();
    assert_eq!(
        saved.spaces[0].columns[0].width,
        WidthIntent::Absolute(800.0)
    );
    assert!(!harness.world().resource::<StatePersistence>().is_dirty());
}

#[test]
fn accepted_empty_space_intent_is_persisted_without_active_projection() {
    let mut harness = TestHarness::new();
    harness.pump_frames(10);
    let path = harness
        .world()
        .resource::<StateFilePath>()
        .as_path()
        .to_path_buf();
    harness
        .world()
        .run_system_once(periodic_state_save)
        .unwrap();
    let state = SpoolState::load_from_file(&path).unwrap();
    assert_eq!(state.spaces.len(), 1);
    assert!(state.spaces[0].columns.is_empty());
}

#[test]
fn capture_publishes_new_acceptance_before_durable_save() {
    use crate::ecs::layout::WidthIntent;
    use crate::ecs::state::capture_state_changes;
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    let path = harness
        .world()
        .resource::<StateFilePath>()
        .as_path()
        .to_path_buf();
    harness
        .world()
        .run_system_once(periodic_state_save)
        .unwrap();
    let before = fs::read(&path).unwrap();
    let saved_revision = harness
        .world()
        .resource::<StatePersistence>()
        .saved_revision();
    {
        let mut query = harness.world().query::<&mut LayoutStrip>();
        let mut strip = query.single_mut(harness.world()).unwrap();
        let id = strip.column_state(0).unwrap().id;
        strip
            .set_width_intent(id, WidthIntent::Absolute(812.0))
            .unwrap();
    }
    harness
        .world()
        .run_system_once(capture_state_changes)
        .unwrap();
    let persistence = harness.world().resource::<StatePersistence>();
    assert!(persistence.is_dirty());
    assert_eq!(persistence.saved_revision(), saved_revision);
    assert!(Some(persistence.accepted_revision()) > saved_revision);
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "capture cannot perform a durable save"
    );
}

#[test]
fn saved_revision_floor_does_not_import_a_prior_sessions_intent() {
    use crate::ecs::layout::WidthIntent;
    let mut writer = StatePersistence::starting_after(73);
    assert_eq!(writer.saved_revision(), None);
    assert!(!writer.is_dirty());
    let current = writer
        .capture(intent_snapshot(WidthIntent::InheritConfig))
        .unwrap();
    assert_eq!(current.revision, 74);
    assert_eq!(
        current.spaces[0].columns[0].width,
        WidthIntent::InheritConfig
    );
    assert!(writer.is_dirty());
}

#[test]
fn persistence_tracks_arrangement_and_raw_height_without_native_geometry() {
    use crate::ecs::layout::LayoutStrip;
    use crate::ecs::state::SpoolState;
    let mut world = bevy::prelude::World::new();
    let first = world.spawn_empty().id();
    let second = world.spawn_empty().id();
    let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
    strip.append(first);
    strip.append(second);
    let capture =
        |strip: &LayoutStrip| SpoolState::from_layouts([strip], |_| None, std::iter::empty());
    let mut persistence = StatePersistence::default();
    persistence.capture(capture(&strip)).unwrap();
    let first_revision = persistence.accepted_revision();
    strip.stack(second).unwrap();
    let stacked = persistence.capture(capture(&strip)).unwrap();
    assert!(persistence.accepted_revision() > first_revision);
    assert_eq!(
        stacked.spaces[0].columns[0].kind,
        spool_shared_types::windowset::ColumnKind::Stack
    );
    assert_eq!(stacked.spaces[0].columns[0].items.len(), 2);
    strip.set_height_weight(first, 3.0).unwrap();
    let changed = persistence.capture(capture(&strip)).unwrap();
    assert_eq!(
        changed.spaces[0].columns[0].items[0].weight.to_bits(),
        3.0_f64.to_bits()
    );
    let revision = persistence.accepted_revision();
    assert!(!strip.set_height_weight(first, 3.0).unwrap());
    persistence.capture(capture(&strip)).unwrap();
    assert_eq!(persistence.accepted_revision(), revision);
    assert!(changed.valid());
    let roundtrip: SpoolState =
        serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
    assert_eq!(roundtrip, changed);
}

#[test]
fn malformed_height_candidates_are_rejected() {
    use crate::ecs::layout::WidthIntent;
    for weight in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut state = intent_snapshot(WidthIntent::InheritConfig);
        state.spaces[0].columns[0].items[0].weight = weight;
        assert!(!state.valid());
    }
    let mut state = intent_snapshot(WidthIntent::InheritConfig);
    state.spaces[0].columns[0].kind = spool_shared_types::windowset::ColumnKind::Stack;
    assert!(
        !state.valid(),
        "a stack must contain at least two independent slots"
    );
}

/// A retained member whose identity is not resolvable at save time must stay
/// an explicit slot, never a fabricated hint and never a silently shortened
/// member list.
#[test]
fn unresolved_members_are_persisted_as_explicit_slots() {
    use crate::ecs::layout::LayoutStrip;
    let mut world = bevy::prelude::World::new();
    let first = world.spawn_empty().id();
    let second = world.spawn_empty().id();
    let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
    strip.append(first);
    strip.append(second);
    strip.stack(second).unwrap();
    let unresolved = SpoolState::from_layouts([&strip], |_| None, std::iter::empty());
    assert!(unresolved.valid());
    let item = &unresolved.spaces[0].columns[0].items[0];
    assert_eq!(
        item.members.len(),
        1,
        "a stack item keeps one slot per retained member"
    );
    assert!(item.members[0].hint.is_none());
    let encoded = serde_json::to_value(&unresolved).unwrap();
    assert_eq!(
        encoded["spaces"][0]["columns"][0]["items"][0]["members"][0]["hint"],
        serde_json::Value::Null,
        "an unresolved member is an explicit null hint, not an omitted slot"
    );
    let roundtrip: SpoolState = serde_json::from_value(encoded).unwrap();
    assert_eq!(roundtrip, unresolved);

    let mut seen = 0;
    let mixed = SpoolState::from_layouts(
        [&strip],
        |entity| {
            seen += 1;
            (entity == first).then(|| crate::ecs::state::SavedWindow {
                window_id: 7,
                pid: 11,
                bundle_id: "fixture".into(),
            })
        },
        std::iter::empty(),
    );
    let members = &mixed.spaces[0].columns[0].items[0].members;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].hint.as_ref().unwrap().window_id, 7);
    assert!(
        seen >= 2,
        "the provider must be consulted for every retained member"
    );
}

#[test]
fn saved_native_tabs_remain_one_height_slot_inside_a_stack() {
    use crate::ecs::layout::LayoutStrip;
    let mut world = bevy::prelude::World::new();
    let first = world.spawn_empty().id();
    let tab = world.spawn_empty().id();
    let lower = world.spawn_empty().id();
    let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
    strip.append(first);
    strip.append(tab);
    strip.convert_to_tabs(first, tab).unwrap();
    strip.append(lower);
    strip.stack(lower).unwrap();
    strip.set_height_weight(tab, 2.0).unwrap();
    let saved = SpoolState::from_layouts(
        [&strip],
        |entity| {
            Some(crate::ecs::state::SavedWindow {
                window_id: if entity == first {
                    1
                } else if entity == tab {
                    2
                } else {
                    3
                },
                pid: 42,
                bundle_id: "fixture".into(),
            })
        },
        std::iter::empty(),
    );
    assert!(saved.valid());
    let items = &saved.spaces[0].columns[0].items;
    assert_eq!(items.len(), 2);
    assert!(items[0].tabs);
    assert_eq!(items[0].members.len(), 2);
    assert_eq!(items[0].weight.to_bits(), 2.0_f64.to_bits());
    assert!(!items[1].tabs);
    assert_eq!(items[1].members.len(), 1);
}

/// A floating frame is saved as isolated intent, and a file written before the
/// field existed still loads: the frame is an addition to the format, not a new
/// format, so no existing state is discarded.
#[test]
fn a_floating_frame_round_trips_and_an_older_file_still_loads() {
    use crate::ecs::state::{INTENT_STATE_VERSION, SavedFloatingWindow};
    use bevy::ecs::entity::Entity as BevyEntity;
    use spool_shared_types::state::Frame;

    let mut saved = SpoolState::from_layouts(
        std::iter::empty(),
        |_: BevyEntity| None,
        [SavedFloatingWindow {
            window_id: 7,
            pid: 11,
            bundle_id: "fixture".into(),
            frame: Frame {
                x: 240,
                y: 120,
                width: 400,
                height: 400,
            },
        }],
    );
    saved.revision = 3;

    let encoded = serde_json::to_string(&saved).expect("serialize");
    let decoded: SpoolState = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, saved, "the frame survives the round trip");

    let older = serde_json::json!({
        "version": INTENT_STATE_VERSION,
        "revision": 1,
        "spaces": [],
    });
    let loaded: SpoolState = serde_json::from_value(older).expect("a file without the field loads");
    assert!(loaded.floating.is_empty());
}

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::ecs::system::RunSystemOnce as _;

use crate::ecs::layout::LayoutStrip;
use crate::ecs::state::{
    QueryStateParams, SavedDisplay, SavedRect, SavedSpace, SpoolState, StateFilePath,
    periodic_state_save,
};
use crate::events::Event;
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_MENUBAR_HEIGHT,
    TEST_WORKSPACE_ID, TestHarness,
};
use spool_shared_types::state::{SpaceKind, StateQueryKind};

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
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    assert!(layout_snapshot(&mut harness).window(1).unwrap().visible);
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

fn test_dir(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("spool-{name}-{}-{nonce}", std::process::id()))
}

#[test]
fn v3_state_serializes_without_virtual_workspace_fields() {
    let state = SpoolState {
        version: 3,
        timestamp: 123,
        active_display_id: Some(TEST_DISPLAY_ID),
        displays: vec![SavedDisplay {
            display_id: TEST_DISPLAY_ID,
            bounds: SavedRect {
                min_x: 0,
                min_y: TEST_MENUBAR_HEIGHT,
                max_x: TEST_DISPLAY_WIDTH,
                max_y: TEST_DISPLAY_HEIGHT,
            },
            active: true,
            space_ids: vec![TEST_WORKSPACE_ID],
        }],
        spaces: vec![SavedSpace {
            space_id: TEST_WORKSPACE_ID,
            display_id: Some(TEST_DISPLAY_ID),
            ordinal: Some(0),
            kind: SpaceKind::User,
            active: true,
            columns: Vec::new(),
        }],
    };

    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["version"], 3);
    assert!(value.get("spaces").is_some());
    assert!(value.get("workspaces").is_none());
    assert!(!value.to_string().contains("virtual_index"));
    assert!(!value.to_string().contains("active_virtual_index"));
}

#[test]
fn v2_migration_dry_run_is_read_only_and_apply_backs_up_then_folds() {
    let dir = test_dir("state-migration");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    let legacy = serde_json::json!({
        "version": 2,
        "timestamp": 123,
        "active_display_id": TEST_DISPLAY_ID,
        "displays": [{
            "display_id": TEST_DISPLAY_ID,
            "bounds": {
                "min_x": 0,
                "min_y": TEST_MENUBAR_HEIGHT,
                "max_x": TEST_DISPLAY_WIDTH,
                "max_y": TEST_DISPLAY_HEIGHT
            },
            "active": true,
            "workspace_ids": [TEST_WORKSPACE_ID]
        }],
        "workspaces": [{
            "workspace_id": TEST_WORKSPACE_ID,
            "display_id": TEST_DISPLAY_ID,
            "active_virtual_index": 1,
            "strips": [
                {"virtual_index": 1, "columns": []},
                {"virtual_index": 0, "columns": []}
            ]
        }]
    });
    let original = serde_json::to_string_pretty(&legacy).unwrap();
    fs::write(&path, &original).unwrap();

    let dry_run = SpoolState::migrate_file(&path, false).unwrap();
    assert!(dry_run.needs_migration);
    assert_eq!(dry_run.native_spaces, 1);
    assert_eq!(dry_run.virtual_rows, 2);
    assert!(!dry_run.applied);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(!dry_run.backup_path.exists());

    let applied = SpoolState::migrate_file(&path, true).unwrap();
    assert!(applied.applied);
    assert!(applied.backup_path.exists());
    assert_eq!(fs::read_to_string(&applied.backup_path).unwrap(), original);

    let migrated = SpoolState::load_from_file(&path).unwrap();
    assert_eq!(migrated.version, 3);
    assert_eq!(migrated.spaces.len(), 1);
    assert_eq!(migrated.spaces[0].space_id, TEST_WORKSPACE_ID);
    assert_eq!(migrated.spaces[0].ordinal, Some(0));
    assert!(migrated.spaces[0].active);

    fs::remove_dir_all(dir).unwrap();
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
fn unavailable_display_topology_does_not_replace_the_last_good_state() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    let path = harness
        .world()
        .resource::<StateFilePath>()
        .as_path()
        .to_path_buf();

    harness
        .world()
        .run_system_once(periodic_state_save)
        .expect("initial periodic state save");
    let mut seeded = SpoolState::load_from_file(&path).expect("saved state");
    seeded.timestamp = 0;
    seeded
        .save_to_file(&path)
        .expect("seed deterministic timestamp");
    let last_good = fs::read(&path).expect("last good state");

    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    harness.world().write_message(Event::SystemWoke {
        msg: "test unavailable display topology".to_string(),
    });
    harness.pump_frames(5);
    assert_eq!(
        harness
            .world()
            .query::<&crate::manager::Display>()
            .iter(harness.world())
            .count(),
        1,
        "an empty active-display sample while asleep must preserve the last good display projection"
    );
    assert_eq!(
        harness
            .world()
            .query::<&LayoutStrip>()
            .iter(harness.world())
            .count(),
        1,
        "an empty active-display sample must not orphan or delete Space state"
    );
    harness
        .world()
        .run_system_once(periodic_state_save)
        .expect("periodic state save while topology is unavailable");

    assert_eq!(
        fs::read(&path).expect("preserved state"),
        last_good,
        "a transiently empty display projection must not overwrite the last trustworthy layout"
    );
}

#[test]
fn incomplete_native_space_observation_preserves_periodic_and_exit_snapshots() {
    use crate::ecs::state::cleanup_on_exit;
    use crate::ecs::topology::NativeTopology;
    use bevy::app::AppExit;

    for exiting in [false, true] {
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
        let mut saved = SpoolState::load_from_file(&path).unwrap();
        saved.timestamp = 0;
        saved.save_to_file(&path).unwrap();
        let before = fs::read(&path).unwrap();
        harness
            .mock_state
            .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(())]);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(1);
        assert!(!harness.world().resource::<NativeTopology>().is_complete());
        if exiting {
            harness.world().write_message(AppExit::Success);
            harness.world().run_system_once(cleanup_on_exit).unwrap();
        } else {
            harness
                .world()
                .run_system_once(periodic_state_save)
                .unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), before, "exiting={exiting}");

        harness
            .world()
            .resource_mut::<bevy::ecs::message::Messages<AppExit>>()
            .clear();
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(1);
        assert!(harness.world().resource::<NativeTopology>().is_complete());
        if exiting {
            harness.world().write_message(AppExit::Success);
            harness.world().run_system_once(cleanup_on_exit).unwrap();
        } else {
            harness
                .world()
                .run_system_once(periodic_state_save)
                .unwrap();
        }
        assert!(SpoolState::load_from_file(&path).unwrap().timestamp > 0);
    }
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

#[test]
fn trustworthy_space_without_windows_is_still_persisted() {
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
        .expect("periodic state save");
    let state = SpoolState::load_from_file(&path).expect("saved empty layout state");

    assert_eq!(state.active_display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(state.displays.len(), 1);
    assert_eq!(state.spaces.len(), 1);
    assert!(state.spaces[0].active);
    assert!(state.spaces[0].columns.is_empty());
}

#[test]
fn incomplete_v3_topology_is_not_loaded_as_restore_state() {
    let dir = test_dir("incomplete-v3-state");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    SpoolState {
        version: 3,
        timestamp: 123,
        active_display_id: Some(TEST_DISPLAY_ID),
        displays: vec![SavedDisplay {
            display_id: TEST_DISPLAY_ID,
            bounds: SavedRect {
                min_x: 0,
                min_y: TEST_MENUBAR_HEIGHT,
                max_x: TEST_DISPLAY_WIDTH,
                max_y: TEST_DISPLAY_HEIGHT,
            },
            active: true,
            space_ids: Vec::new(),
        }],
        spaces: Vec::new(),
    }
    .save_to_file(&path)
    .unwrap();

    assert!(
        SpoolState::load_from_file(&path).is_none(),
        "an incomplete topology snapshot must not influence startup layout"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn extract_query_state(
    params: QueryStateParams,
) -> crate::errors::Result<crate::ecs::state::SpoolQueryState> {
    params.extract()
}

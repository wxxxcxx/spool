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

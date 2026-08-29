use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ecs::state::{SavedDisplay, SavedRect, SavedSpace, SpoolState};
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_MENUBAR_HEIGHT,
    TEST_WORKSPACE_ID,
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

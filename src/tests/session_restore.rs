use bevy::ecs::query::Has;
use bevy::prelude::*;

use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::state::{
    SavedColumn, SavedDisplay, SavedRect, SavedSpace, SavedWindow, SpoolState,
};
use crate::manager::Size;
use crate::platform::ProcessSerialNumber;
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_MENUBAR_HEIGHT, TEST_PROCESS_ID,
    TEST_WINDOW_HEIGHT, TEST_WINDOW_WIDTH, TEST_WORKSPACE_ID, TestHarness,
};
use spool_shared_types::state::SpaceKind;

#[test]
fn startup_restore_rebuilds_one_strip_for_a_native_space() {
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
            columns: vec![SavedColumn::Single(saved_window(0))],
        }],
    };

    let mut harness = TestHarness::new().with_windows(1).with_state(state);
    for _ in 0..5 {
        harness.app.update();
    }

    let world = harness.world();
    let mut query = world.query::<(&LayoutStrip, Has<crate::ecs::ActiveWorkspaceMarker>)>();
    let strips = query
        .iter(world)
        .filter(|(strip, _)| strip.id() == TEST_WORKSPACE_ID)
        .collect::<Vec<_>>();

    assert_eq!(strips.len(), 1);
    assert!(strips[0].1);
    assert!(matches!(
        strips[0].0.columns().next(),
        Some(Column::Single(_))
    ));
}

fn saved_window(id: i32) -> SavedWindow {
    SavedWindow {
        window_id: id,
        pid: TEST_PROCESS_ID,
        psn: ProcessSerialNumber { high: 0, low: 0 },
        bundle_id: "test".into(),
        title: format!("Window {id}"),
        identifier: String::new(),
        role: "AXWindow".into(),
        subrole: "AXStandardWindow".into(),
    }
}

#[allow(dead_code)]
fn test_window_size() -> Size {
    Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT)
}

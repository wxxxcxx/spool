use bevy::prelude::*;

use crate::ecs::display::FloatingLayer;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::workspace::WindowSpaceReassignmentPending;
use crate::ecs::{ActiveWorkspaceMarker, Position};
use crate::events::Event;
use crate::manager::Display;
use crate::platform::WorkspaceId;
use crate::tests::harness::find_window_entity;
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_WORKSPACE_ID, TestHarness,
};
use spool_shared_types::state::SpaceKind;

#[test]
fn topology_heartbeat_projects_a_target_when_space_created_is_lost() {
    const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 200;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_SPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_SPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_SPACE_ID, true);
    harness.pump_frames(10);

    let window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TARGET_SPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TARGET_SPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_SPACE_ID);

    // No SpaceCreated, SpaceChanged, or SpaceDestroyed event is emitted.
    harness.pump_frames(12);

    let world = harness.world();
    let (target_entity, target_parent) = {
        let target = world
            .query::<(Entity, &LayoutStrip, &NativeSpace, &ChildOf)>()
            .iter(world)
            .filter(|(_, strip, ..)| strip.id() == TARGET_SPACE_ID)
            .collect::<Vec<_>>();
        assert_eq!(target.len(), 1);
        let (target_entity, target_strip, native, target_parent) = target[0];
        assert!(target_strip.contains(window));
        assert_eq!(native.ordinal, 0);
        assert_eq!(native.kind, SpaceKind::User);
        (target_entity, target_parent.parent())
    };
    assert_eq!(
        world
            .get::<Position>(target_entity)
            .expect("target origin")
            .0,
        world
            .get::<Display>(target_parent)
            .expect("target display")
            .bounds()
            .min
    );
    assert!(
        world
            .get::<VisibleNativeSpaceMarker>(target_entity)
            .is_some()
    );
    assert!(world.get::<ActiveWorkspaceMarker>(target_entity).is_some());
    assert!(
        world
            .get::<WindowSpaceReassignmentPending>(window)
            .is_none()
    );
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .all(|strip| strip.id() != FULLSCREEN_SPACE_ID)
    );

    let layers = world
        .query::<(&FloatingLayer, &ChildOf)>()
        .iter(world)
        .filter(|(layer, _)| layer.workspace_id == TARGET_SPACE_ID)
        .collect::<Vec<_>>();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].1.parent(), target_parent);
}

#[test]
fn topology_projection_places_a_created_space_on_its_own_inactive_display() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const SECOND_USER_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 200;
    let second_bounds = IRect::new(
        TEST_DISPLAY_WIDTH,
        0,
        TEST_DISPLAY_WIDTH * 2,
        TEST_DISPLAY_HEIGHT,
    );

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            second_bounds,
            vec![FULLSCREEN_SPACE_ID, SECOND_USER_SPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_SPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_SPACE_ID, true);
    harness.pump_frames(10);

    let window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TARGET_SPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, TARGET_SPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_SPACE_ID);
    harness.world().write_message(Event::SpaceCreated {
        space_id: TARGET_SPACE_ID,
    });
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_SPACE_ID,
    });
    harness.pump_frames(3);

    let world = harness.world();
    let (second_display, second_origin) = world
        .query::<(Entity, &Display)>()
        .iter(world)
        .find(|(_, display)| display.id() == SECOND_DISPLAY_ID)
        .map(|(entity, display)| (entity, display.bounds().min))
        .expect("second display projection");
    let target_entity = {
        let target = world
            .query::<(Entity, &LayoutStrip, &ChildOf, &Position)>()
            .iter(world)
            .filter(|(_, strip, ..)| strip.id() == TARGET_SPACE_ID)
            .collect::<Vec<_>>();
        assert_eq!(target.len(), 1);
        let (target_entity, target_strip, target_parent, position) = target[0];
        assert!(target_strip.contains(window));
        assert_eq!(target_parent.parent(), second_display);
        assert_eq!(position.0, second_origin);
        target_entity
    };
    assert!(
        world
            .get::<VisibleNativeSpaceMarker>(target_entity)
            .is_some()
    );
    assert!(world.get::<ActiveWorkspaceMarker>(target_entity).is_none());

    let mut visible_ids = world
        .query_filtered::<&LayoutStrip, With<VisibleNativeSpaceMarker>>()
        .iter(world)
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    visible_ids.sort_unstable();
    assert_eq!(visible_ids, vec![TEST_WORKSPACE_ID, TARGET_SPACE_ID]);
    let active_ids = world
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
        .iter(world)
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    assert_eq!(active_ids, vec![TEST_WORKSPACE_ID]);

    let layers = world
        .query::<(&FloatingLayer, &ChildOf)>()
        .iter(world)
        .filter(|(layer, _)| layer.workspace_id == TARGET_SPACE_ID)
        .collect::<Vec<_>>();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].1.parent(), second_display);
}

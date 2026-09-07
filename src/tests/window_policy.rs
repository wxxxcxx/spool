use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use super::harness::{TestHarness, find_window_entity};
use super::{TEST_DISPLAY_ID, TEST_PROCESS_ID, TEST_WORKSPACE_ID};
use crate::commands::{Action, Operation};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    BProcess, Floating, FreshMarker, RetilePending, RetileWindow, WindowDefaultsPending,
};
use crate::events::Event;
use crate::manager::Application;
use crate::platform::WinID;

fn assert_outside_layout(harness: &mut TestHarness, id: WinID) -> Entity {
    let world = harness.world();
    let entity = find_window_entity(id, world);
    assert!(world.get::<Floating>(entity).is_some());
    let mut strips = world.query::<&LayoutStrip>();
    assert!(strips.iter(world).all(|strip| !strip.contains(entity)));
    entity
}

fn assert_tiled(harness: &mut TestHarness, id: WinID) {
    let world = harness.world();
    let entity = find_window_entity(id, world);
    assert!(world.get::<Floating>(entity).is_none());
    let mut strips = world.query::<&LayoutStrip>();
    assert_eq!(
        strips
            .iter(world)
            .filter(|strip| strip.contains(entity))
            .count(),
        1
    );
}

#[test]
fn window_policy_does_not_infer_float_from_dialog_purpose() {
    let mut harness = TestHarness::new().with_window(1, |window| {
        window.title = "Settings".into();
        window.subrole = "AXDialog".into();
    });
    harness.pump_frames(20);
    assert_tiled(&mut harness, 1);
}

#[test]
fn window_policy_user_rule_can_override_dialog_preference() {
    let config = Config::try_from(
        r#"{"windows": {
        "dialog_default": {"subrole": "AXDialog", "floating": true, "priority": -100},
        "my_dialog": {"title": "Settings", "floating": false}
    }}"#,
    )
    .unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_window(1, |window| {
            window.title = "Settings".into();
            window.subrole = "AXDialog".into();
        });
    harness.pump_frames(20);
    assert_tiled(&mut harness, 1);
}

#[test]
fn window_policy_nonmovable_and_fixed_windows_cannot_bypass_retile() {
    for resizable in [false, true] {
        let config =
            Config::try_from(r#"{"windows": {"force_tile": {"floating": false}}}"#).unwrap();
        let mut harness = TestHarness::new()
            .with_config(config)
            .with_window(1, |window| {
                window.resizable = resizable;
                window.movable = !resizable;
            });
        harness.pump_frames(20);
        let entity = assert_outside_layout(&mut harness, 1);
        harness.world().trigger(RetileWindow(entity));
        harness.pump_frames(10);
        assert_outside_layout(&mut harness, 1);
        assert_eq!(harness.mock_state.frame_write_attempts(1), 0);
    }
}

#[test]
fn window_policy_unknown_capability_retries_initial_defaults() {
    let mut harness = TestHarness::new().with_window(1, |window| {
        window.capabilities_available = false;
    });
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 1);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);
    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_tiled(&mut harness, 1);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
}

#[test]
fn window_policy_deferred_startup_window_keeps_its_native_space() {
    let background = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, background],
        )
        .with_workspace_window(1, background, |window| {
            window.capabilities_available = false;
        });
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 1);
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);

    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = true);
    harness.pump_frames(70);

    assert_tiled(&mut harness, 1);
    assert_eq!(harness.mock_state.window_workspace(1), Some(background));
    let world = harness.world();
    let owners = world
        .query::<&LayoutStrip>()
        .iter(world)
        .filter(|strip| strip.contains(entity))
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![background]);
    let layout = world
        .run_system_once(|state: crate::ecs::state::QueryStateParams| state.extract_window_set())
        .unwrap();
    let projected_owners = layout
        .displays()
        .iter()
        .flat_map(|display| display.workspaces.iter())
        .filter(|space| {
            space
                .columns
                .iter()
                .any(|column| column.windows.iter().any(|window| window.id == 1))
        })
        .map(|space| space.space_id)
        .collect::<Vec<_>>();
    assert_eq!(projected_owners, vec![background]);
    assert!(world.get::<WindowDefaultsPending>(entity).is_none());
}

#[test]
fn window_policy_deferred_placement_waits_for_unique_native_membership() {
    let background = TEST_WORKSPACE_ID + 1;
    for (space, membership) in [
        (TEST_WORKSPACE_ID, Ok(vec![1])),
        (background, Ok(vec![])),
        (background, Err(())),
    ] {
        let mut harness = TestHarness::new()
            .with_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, 1200, 800),
                vec![TEST_WORKSPACE_ID, background],
            )
            .with_workspace_window(1, background, |window| {
                window.capabilities_available = false;
            });
        harness.pump_frames(20);
        let entity = assert_outside_layout(&mut harness, 1);
        harness
            .mock_state
            .update_window(1, |window| window.capabilities_available = true);
        harness
            .mock_state
            .script_workspace_membership_queries(space, std::iter::repeat_n(membership, 1000));
        harness.pump_frames(70);
        assert_outside_layout(&mut harness, 1);
        assert!(
            harness
                .world()
                .get::<WindowDefaultsPending>(entity)
                .is_some()
        );
        assert_eq!(harness.mock_state.frame_write_attempts(1), 0);

        harness
            .mock_state
            .script_workspace_membership_queries(space, []);
        harness.pump_frames(10);
        assert_tiled(&mut harness, 1);
        let world = harness.world();
        let owners = world
            .query::<&LayoutStrip>()
            .iter(world)
            .filter(|strip| strip.contains(entity))
            .map(LayoutStrip::id)
            .collect::<Vec<_>>();
        assert_eq!(owners, vec![background]);
    }
}

#[test]
fn window_policy_unknown_startup_fullscreen_recovers_after_exit() {
    let fullscreen = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, fullscreen],
        )
        .with_workspace_window(1, fullscreen, |window| {
            window.is_full_screen = true;
            window.capabilities_available = false;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, fullscreen, true);
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 1);
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(entity)
            .is_some()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);

    harness.mock_state.update_window(1, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, fullscreen);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: fullscreen,
    });
    harness.pump_frames(20);
    assert_outside_layout(&mut harness, 1);
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);

    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_tiled(&mut harness, 1);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(entity)
            .is_none()
    );
}

#[test]
fn window_policy_deferred_fullscreen_exit_recovers_without_a_final_event() {
    let fullscreen = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, fullscreen],
        )
        .with_workspace_window(1, fullscreen, |window| {
            window.is_full_screen = true;
            window.capabilities_available = false;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, fullscreen, true);
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 1);

    harness.mock_state.update_window(1, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, fullscreen);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: fullscreen,
    });
    harness.pump_frames(20);
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(entity)
            .is_some()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);

    // AX settles after the only native event. The heartbeat must retry it.
    harness.mock_state.update_window(1, |window| {
        window.is_full_screen = false;
        window.capabilities_available = true;
    });
    harness.pump_frames(70);
    assert_tiled(&mut harness, 1);
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
}

#[test]
fn window_policy_manual_tile_intent_survives_a_failed_capability_read() {
    let config = Config::try_from(r#"{"windows": {"float": {"floating": true}}}"#).unwrap();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 0);
    let writes_before = harness.mock_state.frame_write_attempts(0);
    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = false);
    harness.world().write_message(Event::ActionRequested {
        action: Action::Window(Operation::ToggleFloating),
    });
    harness.pump_frames(10);
    assert_outside_layout(&mut harness, 0);
    assert!(harness.world().get::<RetilePending>(entity).is_some());
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes_before);
    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_tiled(&mut harness, 0);
    assert!(harness.world().get::<RetilePending>(entity).is_none());
}

#[test]
fn window_policy_deferred_manual_retile_keeps_its_native_space() {
    let other = TEST_WORKSPACE_ID + 1;
    let config = Config::try_from(r#"{"windows": {"float": {"floating": true}}}"#).unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, other],
        )
        .with_windows(1);
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 0);
    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = false);
    harness.world().trigger(RetileWindow(entity));
    harness.world().flush();
    assert!(harness.world().get::<RetilePending>(entity).is_some());

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, other, false);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(10);
    assert_outside_layout(&mut harness, 0);
    let world = harness.world();
    assert_eq!(
        world
            .query_filtered::<&LayoutStrip, With<crate::ecs::ActiveWorkspaceMarker>>()
            .single(world)
            .unwrap()
            .id(),
        other
    );

    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_tiled(&mut harness, 0);
    assert_eq!(
        harness.mock_state.window_workspace(0),
        Some(TEST_WORKSPACE_ID)
    );
    let world = harness.world();
    let owners = world
        .query::<&LayoutStrip>()
        .iter(world)
        .filter(|strip| strip.contains(entity))
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![TEST_WORKSPACE_ID]);
    assert!(world.get::<RetilePending>(entity).is_none());
}

#[test]
fn window_policy_retile_uses_native_owner_display_dimensions() {
    let mut harness = native_owner_retile_harness();
    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_tiled(&mut harness, 1);
    assert_eq!(
        harness.mock_state.window_workspace(1),
        Some(TEST_WORKSPACE_ID + 1)
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().width(),
        1000,
        "half of the native owner's 2000px display, not the active 1200px display"
    );
}

#[test]
fn window_policy_retile_uses_native_owner_without_an_active_display() {
    let mut harness = native_owner_retile_harness();
    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = true);
    let world = harness.world();
    let active_displays = world
        .query_filtered::<Entity, With<crate::ecs::ActiveDisplayMarker>>()
        .iter(world)
        .collect::<Vec<_>>();
    for entity in active_displays {
        world
            .entity_mut(entity)
            .remove::<crate::ecs::ActiveDisplayMarker>();
    }
    let entity = find_window_entity(1, world);
    world.trigger(RetileWindow(entity));
    world.flush();
    assert_tiled(&mut harness, 1);
    harness.pump_frames(10);
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().width(),
        1000
    );
}

fn native_owner_retile_harness() -> TestHarness {
    let config =
        Config::try_from(r#"{"windows": {"float": {"floating": true, "width": 0.5}}}"#).unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID],
        )
        .with_display(
            TEST_DISPLAY_ID + 1,
            IRect::new(1200, 0, 3200, 1000),
            vec![TEST_WORKSPACE_ID + 1],
        )
        .with_workspace_window(1, TEST_WORKSPACE_ID + 1, |_| {});
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 1);
    harness
        .mock_state
        .update_window(1, |window| window.capabilities_available = false);
    harness.world().trigger(RetileWindow(entity));
    harness.world().flush();
    assert!(harness.world().get::<RetilePending>(entity).is_some());
    harness
}

#[test]
fn window_policy_manual_retile_waits_for_unique_native_membership() {
    let other = TEST_WORKSPACE_ID + 1;
    for (space, membership) in [
        (other, Ok(vec![0])),
        (TEST_WORKSPACE_ID, Ok(vec![])),
        (other, Err(())),
    ] {
        let config = Config::try_from(r#"{"windows": {"float": {"floating": true}}}"#).unwrap();
        let mut harness = TestHarness::new()
            .with_config(config)
            .with_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, 1200, 800),
                vec![TEST_WORKSPACE_ID, other],
            )
            .with_windows(1);
        harness.pump_frames(20);
        let entity = assert_outside_layout(&mut harness, 0);
        let writes_before = harness.mock_state.frame_write_attempts(0);
        harness
            .mock_state
            .script_workspace_membership_queries(space, std::iter::repeat_n(membership, 1000));
        harness.world().trigger(RetileWindow(entity));
        harness.pump_frames(70);
        assert_outside_layout(&mut harness, 0);
        assert!(harness.world().get::<RetilePending>(entity).is_some());
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes_before);

        harness
            .mock_state
            .script_workspace_membership_queries(space, []);
        harness.pump_frames(70);
        assert_tiled(&mut harness, 0);
        let world = harness.world();
        let owners = world
            .query::<&LayoutStrip>()
            .iter(world)
            .filter(|strip| strip.contains(entity))
            .map(LayoutStrip::id)
            .collect::<Vec<_>>();
        assert_eq!(owners, vec![TEST_WORKSPACE_ID]);
        assert!(world.get::<RetilePending>(entity).is_none());
    }
}

#[test]
fn window_policy_second_toggle_cancels_a_pending_tile_request() {
    let config = Config::try_from(r#"{"windows": {"float": {"floating": true}}}"#).unwrap();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(20);
    let entity = assert_outside_layout(&mut harness, 0);
    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = false);
    harness.world().trigger(RetileWindow(entity));
    harness.pump_frames(5);
    assert!(harness.world().get::<RetilePending>(entity).is_some());
    harness.world().write_message(Event::ActionRequested {
        action: Action::Window(Operation::ToggleFloating),
    });
    harness.pump_frames(5);
    assert!(harness.world().get::<RetilePending>(entity).is_none());
    harness
        .mock_state
        .update_window(0, |window| window.capabilities_available = true);
    harness.pump_frames(70);
    assert_outside_layout(&mut harness, 0);
}

#[test]
fn window_policy_fixed_window_grid_does_not_issue_impossible_writes() {
    let config = Config::try_from(
        r#"{"windows": {"float": {
        "floating": true, "grid": "2:2:1:1:1:1"
    }}}"#,
    )
    .unwrap();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(10);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        1,
        IRect::new(40, 50, 440, 350),
    );
    harness
        .mock_state
        .update_window(1, |window| window.resizable = false);
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(30);
    assert_outside_layout(&mut harness, 1);
    assert_eq!(harness.mock_state.frame_write_attempts(1), 0);
}

#[test]
fn window_policy_empty_application_survives_and_discovers_a_later_window() {
    let mut harness = TestHarness::new();
    harness.pump_frames(10);
    let pid = TEST_PROCESS_ID + 1;
    harness
        .mock_state
        .spawn_app(pid, "accessory.test", "Accessory");
    let process = harness.mock_state.create_process(pid);
    harness
        .world()
        .spawn((BProcess(Box::new(process)), FreshMarker));
    harness.pump_frames(80);
    let world = harness.world();
    let mut apps = world.query::<&Application>();
    assert!(apps.iter(world).any(|app| app.pid() == pid));
    let _window =
        harness
            .mock_state
            .spawn_window(pid, TEST_WORKSPACE_ID, 2, IRect::new(0, 0, 400, 400));
    harness.pump_frames(30);
    assert_tiled(&mut harness, 2);
}

#[test]
fn window_policy_existing_process_waits_for_readiness_before_observing_ax() {
    let mut harness = TestHarness::new();
    harness.pump_frames(10);
    let pid = TEST_PROCESS_ID + 1;
    harness
        .mock_state
        .spawn_app(pid, "accessory.test", "Accessory Helper");
    harness.mock_state.update_app(pid, |app| app.ready = false);
    let process = harness.mock_state.create_process(pid);
    let entity = harness
        .world()
        .spawn((BProcess(Box::new(process)), crate::ecs::ExistingMarker))
        .id();
    harness
        .world()
        .run_system_once(crate::ecs::systems::add_existing_process)
        .unwrap();
    harness.pump_frames(20);
    let world = harness.world();
    assert!(
        world.get::<FreshMarker>(entity).is_some(),
        "unready processes must stay pending"
    );
    let mut apps = world.query::<&Application>();
    assert!(!apps.iter(world).any(|app| app.pid() == pid));
    assert_eq!(harness.mock_state.application_ax_attempts(pid), (0, 0));

    harness.mock_state.update_app(pid, |app| app.ready = true);
    harness.pump_frames(20);
    let world = harness.world();
    assert!(world.get::<FreshMarker>(entity).is_none());
    let mut apps = world.query::<&Application>();
    assert!(apps.iter(world).any(|app| app.pid() == pid));
    assert!(harness.mock_state.application_ax_attempts(pid).0 > 0);
}

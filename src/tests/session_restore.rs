use bevy::ecs::query::Has;
use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::native_space::NativeSpace;
use crate::ecs::params::Windows;
use crate::ecs::state::{
    SavedColumn, SavedDisplay, SavedRect, SavedSpace, SavedWindow, SpoolState,
};
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, RestoreWindowState, SpawnWindowTrigger,
};
use crate::events::Event;
use crate::manager::{Application, Display, Size};
use crate::platform::{ProcessSerialNumber, WorkspaceId};
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_MENUBAR_HEIGHT, TEST_PROCESS_ID,
    TEST_WINDOW_HEIGHT, TEST_WINDOW_WIDTH, TEST_WORKSPACE_ID, TestHarness,
};
use spool_shared_types::state::SpaceKind;

#[test]
fn restore_changes_columns_without_replacing_the_native_space_entity() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(30);
    let (entity, native, parent) = {
        let world = harness.world();
        let (entity, _, native, parent) = world
            .query::<(Entity, &LayoutStrip, &NativeSpace, &ChildOf)>()
            .iter(world)
            .find(|(_, strip, _, _)| strip.id() == TEST_WORKSPACE_ID)
            .expect("Space");
        (entity, *native, parent.parent())
    };
    let mut state = target_restore_state(TEST_WORKSPACE_ID);
    state.spaces[0].columns = vec![
        SavedColumn::Single(saved_window(1)),
        SavedColumn::Single(saved_window(0)),
    ];
    harness.world().insert_resource(state);
    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(20);
    let expected = [
        crate::tests::harness::find_window_entity(1, harness.world()),
        crate::tests::harness::find_window_entity(0, harness.world()),
    ];
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(entity)
            .expect("restore must retain the native Space entity")
            .all_windows(),
        expected
    );
    assert_eq!(harness.world().get::<NativeSpace>(entity), Some(&native));
    assert_eq!(
        harness
            .world()
            .get::<ChildOf>(entity)
            .expect("parent")
            .parent(),
        parent
    );
}

#[test]
fn restore_obeys_current_inactive_space_membership() {
    let actual_space = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, actual_space],
        )
        .with_workspace_window(0, actual_space, |_| {})
        .with_state(target_restore_state(TEST_WORKSPACE_ID));
    harness.pump_frames(50);
    assert_eq!(harness.mock_state.window_workspace(0), Some(actual_space));
    let window = crate::tests::harness::find_window_entity(0, harness.world());
    let world = harness.world();
    let owners = world
        .query::<&LayoutStrip>()
        .iter(world)
        .filter(|strip| strip.contains(window))
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![actual_space]);
}

fn late_fallback_restore_harness() -> TestHarness {
    let config = crate::config::Config::try_from(
        r#"{"restore":{"startup_grace_ms":10000},
            "windows":{"float":{"title":"^Window 0$","floating":true}}}"#,
    )
    .unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_state(target_restore_state(TEST_WORKSPACE_ID));
    harness.pump_frames(5);
    assert!(
        harness
            .world()
            .contains_resource::<crate::ecs::restore::SessionRestore>()
    );
    harness
}

#[test]
fn fallback_restore_takes_precedence_over_initial_floating_rule() {
    let mut harness = late_fallback_restore_harness();
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        10,
        IRect::new(40, 50, 440, 350),
    );
    harness.mock_state.update_window(10, |window| {
        window.title = "Window 0".into();
        window.identifier.clear();
    });
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.world().flush();
    let entity = crate::tests::harness::find_window_entity(10, harness.world());
    let world = harness.world();
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .any(|strip| strip.id() == TEST_WORKSPACE_ID && strip.contains(entity)),
        "the unique fallback must actually restore before initial defaults run"
    );
    harness.pump_frames(10);

    let entity = crate::tests::harness::find_window_entity(10, harness.world());
    let world = harness.world();
    assert!(
        world.get::<crate::ecs::Floating>(entity).is_none(),
        "an applied fallback restore must win over the initial floating preference"
    );
    let owners = world
        .query::<&LayoutStrip>()
        .iter(world)
        .filter(|strip| strip.contains(entity))
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![TEST_WORKSPACE_ID]);
    assert!(
        world
            .get::<crate::ecs::WindowDefaultsPending>(entity)
            .is_none()
    );
}

#[test]
fn fallback_restore_survives_deferred_membership_and_initial_floating_rule() {
    for membership in [Err(()), Ok(vec![])] {
        let mut harness = late_fallback_restore_harness();
        let window = harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            10,
            IRect::new(40, 50, 440, 350),
        );
        harness.mock_state.update_window(10, |window| {
            window.title = "Window 0".into();
            window.identifier.clear();
        });
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID, [membership]);
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.world().flush();
        let entity = crate::tests::harness::find_window_entity(10, harness.world());
        let world = harness.world();
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .all(|strip| !strip.contains(entity)),
            "unresolved membership cannot establish a restore destination"
        );

        harness.pump_frames(1);
        harness.pump_frames(10);
        let world = harness.world();
        assert!(world.contains_resource::<crate::ecs::restore::SessionRestore>());
        assert!(
            world.get::<crate::ecs::Floating>(entity).is_none(),
            "membership recovery within startup grace must preserve restore precedence"
        );
        let owners = world
            .query::<&LayoutStrip>()
            .iter(world)
            .filter(|strip| strip.contains(entity))
            .map(LayoutStrip::id)
            .collect::<Vec<_>>();
        assert_eq!(owners, vec![TEST_WORKSPACE_ID]);
        assert!(
            world
                .get::<crate::ecs::WindowDefaultsPending>(entity)
                .is_none()
        );
    }
}

#[test]
fn deferred_fallback_restore_preserves_explicit_floating_choice() {
    let config =
        crate::config::Config::try_from(r#"{"restore":{"startup_grace_ms":10000}}"#).unwrap();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_state(target_restore_state(TEST_WORKSPACE_ID));
    harness.pump_frames(5);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        10,
        IRect::new(40, 50, 440, 350),
    );
    harness.mock_state.update_window(10, |window| {
        window.title = "Window 0".into();
        window.identifier.clear();
    });
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, std::iter::repeat_n(Err(()), 1000));
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = Some(10);
    });
    harness.world().write_message(Event::window_focused(10));
    harness.pump_frames(1);
    let entity = crate::tests::harness::find_window_entity(10, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(entity)
            .is_none()
    );
    harness.world().write_message(Event::ActionRequested {
        action: crate::commands::Action::Window(crate::commands::Operation::ToggleFloating),
    });
    harness.pump_frames(1);
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(entity)
            .is_some()
    );

    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, []);
    harness.pump_frames(10);
    let world = harness.world();
    assert!(
        world.get::<crate::ecs::Floating>(entity).is_some(),
        "initial defaults and deferred restore must not undo a later explicit floating choice"
    );
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .all(|strip| !strip.contains(entity))
    );
    assert!(
        world
            .get::<crate::ecs::WindowDefaultsPending>(entity)
            .is_none()
    );
}

#[test]
fn fallback_restore_takes_precedence_over_initial_geometry_rules() {
    for rule in [
        serde_json::json!({"title":"^Window 0$", "width":0.8}),
        serde_json::json!({"title":"^Window 0$", "floating":true, "grid":"2:2:1:1:1:1"}),
    ] {
        let config = crate::config::Config::try_from(
            serde_json::json!({
                "restore":{"startup_grace_ms":10000},
                "windows":{"restored":rule},
            })
            .to_string()
            .as_str(),
        )
        .unwrap();
        let mut harness = TestHarness::new()
            .with_config(config)
            .with_state(target_restore_state(TEST_WORKSPACE_ID));
        harness.pump_frames(5);
        let window = harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            10,
            IRect::new(40, 50, 440, 350),
        );
        harness.mock_state.update_window(10, |window| {
            window.title = "Window 0".into();
            window.identifier.clear();
        });
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.world().flush();
        let entity = crate::tests::harness::find_window_entity(10, harness.world());
        let world = harness.world();
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .any(|strip| strip.id() == TEST_WORKSPACE_ID && strip.contains(entity))
        );
        harness.pump_frames(30);
        assert!(
            harness
                .world()
                .get::<crate::ecs::Floating>(entity)
                .is_none()
        );
        assert_eq!(
            harness.mock_state.actual_window_frame(10).unwrap().width(),
            400,
            "initial width/grid preferences cannot resize a restored tiled window"
        );
    }
}

#[test]
fn fallback_restore_does_not_bypass_capability_loss_before_defaults() {
    for resizable in [true, false] {
        let mut harness = late_fallback_restore_harness();
        let window = harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            10,
            IRect::new(40, 50, 440, 350),
        );
        harness.mock_state.update_window(10, |window| {
            window.title = "Window 0".into();
            window.identifier.clear();
        });
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.world().flush();
        let entity = crate::tests::harness::find_window_entity(10, harness.world());
        let world = harness.world();
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .any(|strip| strip.id() == TEST_WORKSPACE_ID && strip.contains(entity))
        );

        harness.mock_state.update_window(10, |window| {
            window.resizable = resizable;
            window.movable = !resizable;
        });
        harness.pump_frames(20);
        let world = harness.world();
        assert!(world.get::<crate::ecs::Floating>(entity).is_some());
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .all(|strip| !strip.contains(entity))
        );
        assert_eq!(harness.mock_state.frame_write_attempts(10), 0);
    }
}

#[test]
fn ambiguous_fallback_restore_keeps_initial_floating_rules() {
    let mut harness = late_fallback_restore_harness();
    let windows = [10, 11]
        .into_iter()
        .map(|id| {
            let window = harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                id,
                IRect::new(40, 50, 440, 350),
            );
            harness.mock_state.update_window(id, |window| {
                window.title = "Window 0".into();
                window.identifier.clear();
            });
            window
        })
        .collect();
    harness.world().trigger(SpawnWindowTrigger::new(windows));
    harness.pump_frames(10);

    for id in [10, 11] {
        let entity = crate::tests::harness::find_window_entity(id, harness.world());
        let world = harness.world();
        assert!(world.get::<crate::ecs::Floating>(entity).is_some());
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .all(|strip| !strip.contains(entity))
        );
    }
}

#[test]
fn restore_retries_transient_membership_failure_without_another_notification() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(30);
    let mut saved = target_restore_state(TEST_WORKSPACE_ID);
    saved.spaces[0].columns = vec![
        SavedColumn::Single(saved_window(1)),
        SavedColumn::Single(saved_window(0)),
    ];
    harness.world().insert_resource(saved);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Err(())]);
    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(40);
    let first = crate::tests::harness::find_window_entity(1, harness.world());
    let second = crate::tests::harness::find_window_entity(0, harness.world());
    let world = harness.world();
    let strip = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .expect("strip");
    assert_eq!(strip.all_windows(), vec![first, second]);
}

#[test]
fn restore_recovery_retries_unresolved_membership_without_another_event() {
    let other = TEST_WORKSPACE_ID + 1;
    for (space, members) in [
        (other, vec![0, 1]),
        (other, vec![1]),
        (TEST_WORKSPACE_ID, vec![]),
    ] {
        let mut harness = TestHarness::new().with_windows(2).with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, other],
        );
        harness.pump_frames(30);
        let mut saved = target_restore_state(TEST_WORKSPACE_ID);
        saved.spaces[0].columns = vec![
            SavedColumn::Single(saved_window(1)),
            SavedColumn::Single(saved_window(0)),
        ];
        harness.world().insert_resource(saved);
        harness
            .mock_state
            .script_workspace_membership_queries(space, [Ok(members)]);
        harness.world().trigger(RestoreWindowState);
        harness
            .mock_state
            .script_workspace_membership_queries(space, []);
        harness.pump_frames(15);
        let first = crate::tests::harness::find_window_entity(1, harness.world());
        let second = crate::tests::harness::find_window_entity(0, harness.world());
        let world = harness.world();
        let strip = world
            .query::<&LayoutStrip>()
            .iter(world)
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .unwrap();
        assert_eq!(strip.all_windows(), vec![first, second]);
    }
}

#[test]
fn restore_ignores_unrelated_membership_ambiguity() {
    let other = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new().with_windows(3).with_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID, other],
    );
    harness.pump_frames(30);
    let mut saved = target_restore_state(TEST_WORKSPACE_ID);
    saved.spaces[0].columns = vec![
        SavedColumn::Single(saved_window(1)),
        SavedColumn::Single(saved_window(0)),
    ];
    harness.world().insert_resource(saved);
    harness
        .mock_state
        .script_workspace_membership_queries(other, [Ok(vec![2])]);
    harness.world().trigger(RestoreWindowState);
    let expected =
        [1, 0, 2].map(|id| crate::tests::harness::find_window_entity(id, harness.world()));
    let world = harness.world();
    let strip = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap();
    assert_eq!(strip.all_windows(), expected);
}

#[test]
fn restore_membership_retry_expires_with_startup_grace() {
    let other = TEST_WORKSPACE_ID + 1;
    let mut harness = TestHarness::new().with_windows(2).with_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID, other],
    );
    harness.pump_frames(30);
    let mut saved = target_restore_state(TEST_WORKSPACE_ID);
    saved.spaces[0].columns = vec![
        SavedColumn::Single(saved_window(1)),
        SavedColumn::Single(saved_window(0)),
    ];
    harness.world().insert_resource(saved);
    harness
        .mock_state
        .script_workspace_membership_queries(other, std::iter::repeat_n(Ok(vec![0, 1]), 1000));
    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(30);
    assert!(
        !harness
            .world()
            .contains_resource::<crate::ecs::restore::SessionRestore>()
    );
    assert!(!harness.world().contains_resource::<SpoolState>());
    harness
        .mock_state
        .script_workspace_membership_queries(other, []);
    harness.pump_frames(30);
    let first = crate::tests::harness::find_window_entity(1, harness.world());
    let second = crate::tests::harness::find_window_entity(0, harness.world());
    let world = harness.world();
    let strip = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap();
    assert_eq!(
        strip.all_windows(),
        vec![second, first],
        "expired restore cannot reorder windows later"
    );
}

#[test]
fn suspended_windows_remain_in_the_persisted_layout() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(15);

    {
        let mut strips = harness.world().query::<&mut LayoutStrip>();
        let mut strip = strips
            .iter_mut(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .expect("test workspace strip");
        strip.swap(0, 1);
    }
    harness.pump_frames(2);
    for id in 0..3 {
        harness.mock_state.os_withdraw_window(id);
    }
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);

    let state = harness
        .world()
        .run_system_once(extract_spool_state)
        .expect("extract state");
    let saved = state
        .spaces
        .iter()
        .find(|space| space.space_id == TEST_WORKSPACE_ID)
        .expect("saved test workspace");
    let saved_ids = saved
        .columns
        .iter()
        .map(|column| match column {
            SavedColumn::Single(window) => window.window_id,
            _ => panic!("expected single-column fixture"),
        })
        .collect::<Vec<_>>();
    assert_eq!(saved_ids, vec![1, 0, 2]);
    for column in &saved.columns {
        let SavedColumn::Single(window) = column else {
            unreachable!("fixture only contains single columns");
        };
        assert_eq!(window.pid, TEST_PROCESS_ID);
        assert_eq!(window.bundle_id, "test");
        assert!(window.title.is_empty());
    }
}

fn extract_spool_state(
    workspaces: Query<(
        Option<&ChildOf>,
        &LayoutStrip,
        &NativeSpace,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Windows,
    apps: Query<&Application>,
) -> SpoolState {
    SpoolState::extract(&workspaces, &displays, &windows, &apps)
}

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

#[test]
fn startup_restore_reflows_windows_to_the_saved_column_order() {
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
            columns: vec![
                SavedColumn::Single(saved_window(0)),
                SavedColumn::Single(saved_window(2)),
                SavedColumn::Single(saved_window(1)),
            ],
        }],
    };

    let mut harness = TestHarness::new().with_windows(3).with_state(state);
    harness.pump_frames(20);

    let ordered_x = [0, 2, 1].map(|window_id| {
        harness
            .mock_state
            .actual_window_frame(window_id)
            .expect("restored window frame")
            .min
            .x
    });
    assert!(
        ordered_x.windows(2).all(|pair| pair[0] < pair[1]),
        "saved column order must drive the physical frame order: {ordered_x:?}"
    );
}

#[test]
fn pending_destroyed_space_blocks_restore_until_topology_drops_the_id() {
    const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;
    let state = fullscreen_restore_state(FULLSCREEN_SPACE_ID);
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_SPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_SPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .with_workspace_window(1, TEST_WORKSPACE_ID, |_| {})
        .with_state(state);
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_SPACE_ID, true);
    harness.pump_frames(8);

    let restored = crate::tests::harness::find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_SPACE_ID,
    });
    harness.pump_frames(2);

    let mut pending = harness
        .world()
        .query::<(&LayoutStrip, Has<PendingSpaceDestruction>)>();
    assert!(pending.iter(harness.world()).any(|(strip, pending)| {
        strip.id() == FULLSCREEN_SPACE_ID && pending && strip.all_windows().is_empty()
    }));

    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.id() == FULLSCREEN_SPACE_ID)
            .count(),
        1,
        "late restore must not recreate a Space pending OS-topology removal"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(restored))
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(restored)
            .is_none()
    );

    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_SPACE_ID);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(4);
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_SPACE_ID)
    );
}

#[test]
fn late_restore_cannot_despawn_pending_source_still_present_in_topology() {
    const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;
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
    harness.pump_frames(8);

    let window = crate::tests::harness::find_window_entity(0, harness.world());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_SPACE_ID,
    });
    harness.pump_frames(1);

    let mut pending = harness
        .world()
        .query::<(&LayoutStrip, Has<PendingSpaceDestruction>)>();
    assert!(pending.iter(harness.world()).any(|(strip, pending)| {
        strip.id() == FULLSCREEN_SPACE_ID && pending && strip.contains(window)
    }));

    harness
        .world()
        .insert_resource(target_restore_state(TEST_WORKSPACE_ID));
    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(4);

    let source = {
        let world = harness.world();
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .find(|strip| strip.id() == FULLSCREEN_SPACE_ID)
            .expect("retained source")
            .contains(window)
    };
    assert!(
        source,
        "restore must wait for native membership before consuming the source"
    );
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness.world().trigger(RestoreWindowState);
    harness.pump_frames(4);

    let mut strips = harness
        .world()
        .query::<(&LayoutStrip, Has<PendingSpaceDestruction>)>();
    assert!(
        strips.iter(harness.world()).any(|(strip, pending)| {
            strip.id() == FULLSCREEN_SPACE_ID && pending && strip.all_windows().is_empty()
        }),
        "restore must leave an empty pending source for topology reconciliation"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|(strip, _)| strip.contains(window))
            .map(|(strip, _)| strip.id())
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
}

#[test]
fn late_window_saved_on_destroyed_space_uses_its_live_space() {
    const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_SPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(1, TEST_WORKSPACE_ID, |_| {})
        .with_state(fullscreen_restore_state(FULLSCREEN_SPACE_ID));
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness.pump_frames(8);

    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_SPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_SPACE_ID,
    });
    harness.pump_frames(2);

    let frame = IRect::new(100, 100, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let window = harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 0, frame);
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(3);

    let entity = crate::tests::harness::find_window_entity(0, harness.world());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_SPACE_ID)
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(entity))
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID],
        "a late window must not be reserved for a saved Space absent from OS topology"
    );
}

fn fullscreen_restore_state(fullscreen_space_id: WorkspaceId) -> SpoolState {
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
            space_ids: vec![fullscreen_space_id, TEST_WORKSPACE_ID],
        }],
        spaces: vec![
            SavedSpace {
                space_id: fullscreen_space_id,
                display_id: Some(TEST_DISPLAY_ID),
                ordinal: Some(0),
                kind: SpaceKind::Fullscreen,
                active: true,
                columns: vec![SavedColumn::Fullscreen(saved_window(0))],
            },
            SavedSpace {
                space_id: TEST_WORKSPACE_ID,
                display_id: Some(TEST_DISPLAY_ID),
                ordinal: Some(1),
                kind: SpaceKind::User,
                active: false,
                columns: vec![SavedColumn::Single(saved_window(1))],
            },
        ],
    }
}

fn target_restore_state(target_space_id: WorkspaceId) -> SpoolState {
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
            space_ids: vec![target_space_id],
        }],
        spaces: vec![SavedSpace {
            space_id: target_space_id,
            display_id: Some(TEST_DISPLAY_ID),
            ordinal: Some(0),
            kind: SpaceKind::User,
            active: true,
            columns: vec![SavedColumn::Single(saved_window(0))],
        }],
    }
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

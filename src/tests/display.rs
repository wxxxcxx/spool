use std::collections::HashSet;
use std::time::Duration;

use bevy::ecs::observer::On;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use crate::commands::{Action, MouseMove, MoveFocus, Operation};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DockPosition, Floating, ObservedWindowFrame,
    ReadDisplayProperties, RefreshWindowSizes, Timeout,
};
use crate::events::Event;
use crate::manager::{Display, Origin, Size, Window};
use crate::{assert_not_on_workspace, assert_on_workspace, assert_window_at, assert_window_size};

use super::*;

fn discover_bottom_dock(displays: Query<Entity, Added<Display>>, mut commands: Commands) {
    for entity in &displays {
        commands.entity(entity).insert(DockPosition::Bottom(80));
    }
}

#[derive(Resource, Default)]
struct SimulatedDockRefresh(bool);

fn simulate_visible_dock_after_refresh(
    trigger: On<ReadDisplayProperties>,
    refresh: Res<SimulatedDockRefresh>,
    mut commands: Commands,
) {
    if refresh.0 {
        commands
            .entity(trigger.event().0)
            .insert(DockPosition::Bottom(80));
    }
}

#[test]
fn test_multi_display_lifecycle() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::DisplayRemoved {
            display_id: TEST_DISPLAY_ID,
        },
        Event::DisplayAdded {
            display_id: TEST_DISPLAY_ID,
        },
    ];

    let mut harness = TestHarness::new().with_windows(1);
    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    harness
        .on_iteration(1, |world, state| {
            let mut query = world.query_filtered::<Entity, With<Display>>();
            query.single(world).expect("should have one display");
            state.remove_display(TEST_DISPLAY_ID);
        })
        .on_iteration(2, |world, mut state| {
            assert!(
                world
                    .query_filtered::<Entity, With<Display>>()
                    .single(world)
                    .is_err(),
                "display should be despawned"
            );

            let workspace_entity = {
                let mut query = world.query_filtered::<Entity, With<LayoutStrip>>();
                query.single(world).expect("should have one workspace")
            };
            let workspace = world.entity(workspace_entity);
            assert!(
                workspace.get::<Timeout>().is_some(),
                "orphaned workspace should have a timeout"
            );
            assert!(
                workspace.get::<ChildOf>().is_none(),
                "orphaned workspace should have no parent"
            );
            state.add_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![TEST_WORKSPACE_ID],
            );
        })
        .on_iteration(3, |world, _state| {
            let new_display_entity = world
                .query_filtered::<Entity, With<Display>>()
                .single(world)
                .expect("display should be spawned again");

            let workspace_entity = {
                let mut query = world.query_filtered::<Entity, With<LayoutStrip>>();
                query.single(world).expect("should have one workspace")
            };
            let workspace = world.entity(workspace_entity);
            assert!(
                workspace.get::<Timeout>().is_none(),
                "re-parented workspace should no longer have a timeout"
            );
            let child_of: &ChildOf = workspace
                .get::<ChildOf>()
                .expect("re-parented workspace should have a parent");
            assert_eq!(
                child_of.parent(),
                new_display_entity,
                "workspace should be child of the new display"
            );
        })
        .run(commands);
}

#[test]
fn test_multi_workspace_orphaning() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::DisplayRemoved {
            display_id: TEST_DISPLAY_ID,
        },
    ];

    let workspaces = vec![TEST_WORKSPACE_ID, TEST_WORKSPACE_ID + 1];
    let harness = TestHarness::new().with_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        workspaces,
    );
    harness
        .on_iteration(1, |world, state| {
            let display_entity = world
                .query_filtered::<Entity, With<Display>>()
                .single(world)
                .expect("should have one display");

            let workspace_entities = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .iter(world)
                .collect::<Vec<_>>();
            assert_eq!(workspace_entities.len(), 2, "should have two workspaces");

            for &ws in &workspace_entities {
                let child_of: &ChildOf = world
                    .entity(ws)
                    .get::<ChildOf>()
                    .expect("workspace should have parent");
                assert_eq!(child_of.parent(), display_entity);
            }
            state.remove_display(TEST_DISPLAY_ID);
        })
        .on_iteration(2, |world, _state| {
            let workspace_entities = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .iter(world)
                .collect::<Vec<_>>();
            for &ws in &workspace_entities {
                let entity: EntityRef = world.entity(ws);
                assert!(
                    entity.get::<Timeout>().is_some(),
                    "each workspace should have a timeout"
                );
                assert!(
                    entity.get::<ChildOf>().is_none(),
                    "each workspace should have no parent"
                );
            }
        })
        .run(commands);
}

#[test]
fn test_multi_display_no_height_crosstalk() {
    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, ext_frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 200, frame);

    let ext_usable_height = EXT_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT;

    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::DisplayChanged,
        Event::MenuOpened { window_id: 100 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    harness
        .on_iteration(1, move |world, _state| {
            assert_window_size!(world, 100, TEST_WINDOW_WIDTH, ext_usable_height);
        })
        .on_iteration(2, |world, _state| {
            use crate::ecs::ActiveWorkspaceMarker;
            let mut strip_query =
                world.query_filtered::<&mut LayoutStrip, Without<ActiveWorkspaceMarker>>();
            for mut strip in strip_query.iter_mut(world) {
                strip.set_changed();
            }
        })
        .on_iteration(4, move |world, _state| {
            assert_window_size!(world, 100, TEST_WINDOW_WIDTH, ext_usable_height);
        })
        .run(commands);
}

#[test]
fn test_next_display_inserts_into_target_strip() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::ToNextDisplay(MoveFocus::Follow)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .on_iteration(1, move |world, _state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
        })
        .on_iteration(2, move |world, _state| {
            assert_on_workspace!(world, 0, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 0, TEST_WORKSPACE_ID);
        })
        .run(commands);
}

#[test]
fn test_floating_window_moves_to_next_display_without_becoming_tiled() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::ToNextDisplay(MoveFocus::Follow)),
        },
    ];

    let config = Config::try_from(
        r#"
[options]

[bindings]

[windows.test]
title = ".*"
floating = true
"#,
    )
    .expect("floating test config should parse");

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .on_iteration(1, move |world, _state| {
            let entity = find_window_entity(0, world);
            assert!(world.entity(entity).contains::<Floating>());

            let mut strips = world.query::<&LayoutStrip>();
            assert!(strips.iter(world).all(|strip| !strip.contains(entity)));

            assert_window_at!(
                world,
                0,
                (EXT_DISPLAY_WIDTH - TEST_WINDOW_WIDTH) / 2,
                -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT
            );
        })
        .run(commands);
}

#[test]
fn test_send_next_display_stays_on_source() {
    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 101, frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 100, frame);
    harness.mock_state.focus_window(100);

    let commands = vec![
        Event::MenuOpened { window_id: 101 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::ToNextDisplay(MoveFocus::Stay)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    harness
        .on_iteration(1, move |world, _state| {
            assert_on_workspace!(world, 100, TEST_WORKSPACE_ID);
        })
        .on_iteration(2, move |world, state| {
            assert_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_eq!(state.active_display(), TEST_DISPLAY_ID);
        })
        .run(commands);
}

#[test]
fn test_mouse_to_next_display() {
    let commands = vec![
        Event::MenuOpened { window_id: 101 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Mouse(MouseMove::ToNextDisplay),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];
    let origin = Origin::new(0, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let display_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);

    // harness
    //     .mock_state
    //     .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 101, frame);
    // harness
    //     .mock_state
    //     .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 100, frame);
    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, display_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| {
            data.pid = TEST_PROCESS_ID;
            data.workspace_id = TEST_WORKSPACE_ID;
            data.frame = frame;
        })
        .with_focused_window(100)
        .on_iteration(1, move |world, state| {
            let entity = find_window_entity(100, world);
            let window = world.get::<Window>(entity).expect("need window");
            assert_eq!(state.cursor_position(), window.frame().center());
        })
        .on_iteration(3, move |world, state| {
            let mut query = world.query::<(&Display, Option<&DockPosition>)>();
            let (display, dock) = query
                .iter(world)
                .find(|display| display.0.id() == EXT_DISPLAY_ID)
                .expect("need display");
            let config = world.resource::<Config>();
            let bounds = display.actual_display_bounds(dock, config);
            assert_eq!(state.cursor_position(), bounds.center());
        })
        .run(commands);
}

/// Regression test: spool's init pass must not drag windows that live on
/// inactive displays onto the active display. `apply_window_properties`
/// initially appends every observed window to the active strip; if the
/// layout writers run before `finish_setup` has reassigned them, they
/// cache active-display coordinates into `Position` and `commit_window_frame`
/// later pushes those to macOS, moving the windows.
#[test]
fn test_init_keeps_windows_on_their_real_displays() {
    // Internal (test) display is active. Window 100 lives on the external
    // display's space, window 200 lives on the active display's space.

    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 200, ext_frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, frame);

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    harness
        .on_iteration(0, move |world, _state| {
            assert_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_on_workspace!(world, 200, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 200, EXT_WORKSPACE_ID);
            // The OS frame for window 100 must stay within the external
            // display's vertical bounds (negative y); if init moved it
            // onto the active display the frame would land at y >= 0.
            assert_window_at!(world, 100, ext_origin.x, ext_origin.y);
        })
        .run(commands);
}

/// Waking from sleep (or a resolution/configuration change) with a monitor
/// gone should reconcile the ECS display set against the OS even though no
/// per-display `DisplayRemoved` flag arrives: the vanished display is removed
/// and its workspace is orphaned.
#[test]
fn test_wake_reconciles_unplugged_display() {
    let harness = TestHarness::new().with_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    // A window on the external display so its workspace strip actually exists.
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, ext_frame);

    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::SystemWoke { msg: String::new() },
    ];

    harness
        .on_iteration(1, |world, state| {
            let displays = world
                .query_filtered::<Entity, With<Display>>()
                .iter(world)
                .count();
            assert_eq!(displays, 2, "should start with two displays");

            // Unplug the external display behind spool's back — no
            // DisplayRemoved event is sent, mimicking a wake-from-sleep.
            state.remove_display(EXT_DISPLAY_ID);
        })
        .on_iteration(2, |world, _state| {
            let displays = world
                .query_filtered::<Entity, With<Display>>()
                .iter(world)
                .count();
            assert_eq!(displays, 1, "reconcile should despawn the vanished display");

            // The external display's workspace must be orphaned, not lost.
            let orphan = world
                .query::<(&LayoutStrip, Option<&ChildOf>, Has<Timeout>)>()
                .iter(world)
                .find(|(strip, _, _)| strip.id() == EXT_WORKSPACE_ID)
                .map(|(_, child, timeout)| (child.is_some(), timeout));
            let (has_parent, has_timeout) =
                orphan.expect("external workspace strip should still exist");
            assert!(!has_parent, "orphaned workspace should have no parent");
            assert!(has_timeout, "orphaned workspace should carry a timeout");
        })
        .run(commands);
}

/// Even when the display set is unchanged, waking from sleep must force the
/// active workspace to re-tile, because macOS relocates window frames across a
/// sleep/wake cycle.
#[test]
fn test_wake_refreshes_active_workspace() {
    let harness = TestHarness::new().with_windows(1);

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::SystemWoke { msg: String::new() },
    ];

    harness
        .on_iteration(1, |world, _state| {
            let refreshed = world
                .query_filtered::<Has<RefreshWindowSizes>, With<ActiveWorkspaceMarker>>()
                .iter(world)
                .any(|has| has);
            assert!(
                refreshed,
                "wake should mark the active workspace for a window-size refresh"
            );
        })
        .run(commands);
}

#[test]
fn native_spaces_track_one_visible_space_per_display_at_startup() {
    const EXT_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const EXT_SPACE_A: WorkspaceId = TEST_WORKSPACE_ID + 10;
    const EXT_SPACE_B: WorkspaceId = TEST_WORKSPACE_ID + 11;

    TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![EXT_SPACE_A, EXT_SPACE_B],
        )
        .on_iteration(0, |world, _state| {
            let visible = world
                .query_filtered::<(&LayoutStrip, &NativeSpace), With<VisibleNativeSpaceMarker>>()
                .iter(world)
                .map(|(strip, native)| (strip.id(), native.ordinal))
                .collect::<HashSet<_>>();
            assert_eq!(
                visible,
                HashSet::from([(TEST_WORKSPACE_ID, 0), (EXT_SPACE_A, 0)])
            );

            let active = world
                .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
                .single(world)
                .expect("one globally active layout strip");
            assert_eq!(active.id(), TEST_WORKSPACE_ID);
        })
        .run(vec![Event::ActionRequested {
            action: Action::PrintState,
        }]);
}

#[test]
fn native_spaces_observe_switches_on_an_inactive_display() {
    const EXT_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const EXT_SPACE_A: WorkspaceId = TEST_WORKSPACE_ID + 10;
    const EXT_SPACE_B: WorkspaceId = TEST_WORKSPACE_ID + 11;

    TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![EXT_SPACE_A, EXT_SPACE_B],
        )
        .on_iteration(0, |_world, state| {
            state.activate_workspace(EXT_DISPLAY_ID, EXT_SPACE_B, false);
        })
        .on_iteration(1, |world, _state| {
            let visible = world
                .query_filtered::<&LayoutStrip, With<VisibleNativeSpaceMarker>>()
                .iter(world)
                .map(LayoutStrip::id)
                .collect::<HashSet<_>>();
            assert_eq!(visible, HashSet::from([TEST_WORKSPACE_ID, EXT_SPACE_B]));

            let active = world
                .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
                .single(world)
                .expect("one globally active layout strip");
            assert_eq!(active.id(), TEST_WORKSPACE_ID);
        })
        .run(vec![
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::SpaceChanged,
        ]);
}

#[test]
fn visible_dock_reflows_the_active_strip_to_the_usable_height() {
    const DOCK_HEIGHT: i32 = 80;
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - DOCK_HEIGHT;
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);

    let display_entity = {
        let world = harness.world();
        world
            .query::<(Entity, &Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == TEST_DISPLAY_ID).then_some(entity))
            .expect("active display")
    };
    harness
        .world()
        .entity_mut(display_entity)
        .insert(DockPosition::Bottom(DOCK_HEIGHT));
    let viewport_height = {
        let world = harness.world();
        let display = world
            .get::<Display>(display_entity)
            .expect("active display");
        let dock = world
            .get::<DockPosition>(display_entity)
            .expect("visible dock");
        display
            .actual_display_bounds(Some(dock), world.resource::<Config>())
            .height()
    };
    assert_eq!(viewport_height, expected_height);
    harness.pump_frames(3);

    let entity = find_window_entity(0, harness.world());
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0.y,
        expected_height,
        "ECS bounds must be recomputed from the Dock-reduced viewport"
    );
    assert_window_size!(harness.world(), 0, TEST_WINDOW_WIDTH, expected_height);
}

#[test]
fn dock_preference_change_rereads_display_and_reflows_windows() {
    const DOCK_HEIGHT: i32 = 80;
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - DOCK_HEIGHT;
    let mut harness = TestHarness::new().with_windows(1);
    harness
        .app
        .insert_resource(SimulatedDockRefresh::default())
        .add_observer(simulate_visible_dock_after_refresh);
    harness.pump_frames(10);
    harness.world().resource_mut::<SimulatedDockRefresh>().0 = true;

    harness.world().write_message(Event::DockDidChangePref {
        msg: "test Dock auto-hide change".to_string(),
    });
    harness.pump_frames(3);

    let entity = find_window_entity(0, harness.world());
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0.y,
        expected_height,
        "a Dock visibility preference change must refresh the usable viewport"
    );
}

#[test]
fn dock_refresh_heartbeat_recovers_a_missed_notification() {
    const DOCK_HEIGHT: i32 = 80;
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - DOCK_HEIGHT;
    let mut harness = TestHarness::new().with_windows(1);
    harness
        .app
        .insert_resource(SimulatedDockRefresh::default())
        .add_observer(simulate_visible_dock_after_refresh);
    harness.pump_frames(10);
    harness.world().resource_mut::<SimulatedDockRefresh>().0 = true;

    // No Dock event is delivered. The periodic read must still converge to the
    // actual NSScreen.visibleFrame instead of retaining stale geometry forever.
    harness.pump_frames(12);

    let entity = find_window_entity(0, harness.world());
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0.y,
        expected_height,
        "the Dock reconciliation heartbeat must recover a dropped notification"
    );
}

#[test]
fn dock_height_animation_avoids_expensive_complete_frame_writes() {
    const DOCK_HEIGHT: i32 = 80;
    let config: Config = (
        crate::config::MainOptions {
            animation_speed: Some(12.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let initial_origin = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("observed frame")
        .0
        .min;
    let baseline_complete_writes = harness.mock_state.frame_write_attempts(0);
    let display_entity = {
        let world = harness.world();
        world
            .query::<(Entity, &Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == TEST_DISPLAY_ID).then_some(entity))
            .expect("active display")
    };

    harness
        .world()
        .entity_mut(display_entity)
        .insert(DockPosition::Bottom(DOCK_HEIGHT));
    harness.pump_frames(5);

    assert_eq!(
        harness.mock_state.frame_write_attempts(0),
        baseline_complete_writes,
        "a height-only Dock animation must not run the costly complete-frame AX sequence every tick"
    );
    assert!(
        harness.mock_state.resize_write_attempts(0) > 0,
        "the Dock animation must use the size-only AX path"
    );
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .expect("observed frame")
            .0
            .min,
        initial_origin,
        "the size-only fast path must preserve the confirmed origin"
    );
}

#[test]
fn startup_layout_uses_the_visible_dock_height_for_every_column() {
    const DOCK_HEIGHT: i32 = 80;
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - DOCK_HEIGHT;
    let mut harness = TestHarness::new().with_windows(2);
    harness.app.add_systems(PreUpdate, discover_bottom_dock);

    harness.pump_frames(10);

    assert_window_size!(harness.world(), 0, TEST_WINDOW_WIDTH, expected_height);
    assert_window_size!(harness.world(), 1, TEST_WINDOW_WIDTH, expected_height);
}

#[test]
fn wake_after_zero_display_startup_discovers_existing_windows() {
    let mut harness = TestHarness::new();
    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );

    harness.pump_frames(5);
    assert_eq!(
        harness
            .world()
            .query_filtered::<Entity, With<Display>>()
            .iter(harness.world())
            .count(),
        0,
        "startup while the display server is unavailable must not invent a display"
    );
    assert_eq!(
        harness
            .world()
            .query_filtered::<Entity, With<Window>>()
            .iter(harness.world())
            .count(),
        0,
        "the existing window cannot be projected before its display and Space exist"
    );

    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID],
    );
    harness.world().write_message(Event::SystemWoke {
        msg: "test display server recovery".to_string(),
    });
    harness.pump_frames(20);

    assert_eq!(
        harness
            .world()
            .query_filtered::<Entity, With<Display>>()
            .iter(harness.world())
            .count(),
        1,
        "wake reconciliation must restore the display projection"
    );
    let window = find_window_entity(0, harness.world());
    let strip = harness
        .world()
        .query::<&LayoutStrip>()
        .iter(harness.world())
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .expect("restored Space projection");
    assert!(
        strip.contains(window),
        "the lifecycle heartbeat must discover and tile windows that existed during zero-topology startup"
    );
}

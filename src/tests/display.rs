use std::collections::HashSet;
use std::time::Duration;

use bevy::ecs::observer::On;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use crate::commands::{Action, MouseMove, MoveFocus, Operation};
use crate::config::Config;
use crate::ecs::display::FloatingLayer;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DockPosition, Floating, ObservedWindowFrame,
    ReadDisplayProperties, RefreshWindowSizes,
};
use crate::events::Event;
use crate::manager::{Display, Origin, Size, Window};
use crate::{assert_not_on_workspace, assert_on_workspace, assert_window_at, assert_window_size};

use super::*;
use crate::ecs::native_space::DetachedSpace;

#[test]
fn long_display_disconnect_preserves_strip_identity_and_window_order() {
    let mut harness = TestHarness::new()
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH + EXT_DISPLAY_WIDTH,
                EXT_DISPLAY_HEIGHT,
            ),
            vec![EXT_WORKSPACE_ID],
        )
        .with_workspace_window(100, EXT_WORKSPACE_ID, |_| {})
        .with_workspace_window(101, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(30);
    let (source, order) = {
        let world = harness.world();
        let (entity, strip) = world
            .query::<(Entity, &LayoutStrip)>()
            .iter(world)
            .find(|(_, strip)| strip.id() == EXT_WORKSPACE_ID)
            .expect("external strip");
        (entity, strip.all_windows())
    };
    {
        let world = harness.world();
        world
            .get_mut::<FloatingLayer>(source)
            .expect("external floating layer")
            .front = true;
    }
    harness.mock_state.remove_display(EXT_DISPLAY_ID);
    harness.world().write_message(Event::DisplayRemoved {
        display_id: EXT_DISPLAY_ID,
    });
    harness.pump_frames(350);
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .expect("detached strip must outlive a timeout")
            .all_windows(),
        order
    );
    assert!(
        harness
            .world()
            .get::<FloatingLayer>(source)
            .expect("floating layer must share the retained Space lifetime")
            .front
    );
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(
            TEST_DISPLAY_WIDTH,
            0,
            TEST_DISPLAY_WIDTH + EXT_DISPLAY_WIDTH,
            EXT_DISPLAY_HEIGHT,
        ),
        vec![EXT_WORKSPACE_ID],
    );
    harness.world().write_message(Event::DisplayAdded {
        display_id: EXT_DISPLAY_ID,
    });
    harness.pump_frames(30);
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .expect("same strip must reattach")
            .all_windows(),
        order
    );
    let parent = harness
        .world()
        .get::<ChildOf>(source)
        .expect("reattached")
        .parent();
    assert_eq!(
        harness
            .world()
            .get::<Display>(parent)
            .expect("display")
            .id(),
        EXT_DISPLAY_ID
    );
    assert!(
        harness
            .world()
            .get::<FloatingLayer>(source)
            .expect("reattached floating layer")
            .front
    );
    for entity in order {
        assert!(harness.world().get::<Floating>(entity).is_none());
    }
}

#[test]
fn lifecycle_consumers_share_one_topology_observation_per_invalidation() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let before = harness.mock_state.display_observation_count();
    harness.world().write_message(Event::DisplayConfigured {
        display_id: TEST_DISPLAY_ID,
    });
    harness.pump_frames(1);
    assert_eq!(
        harness.mock_state.display_observation_count() - before,
        1,
        "display and Space lifecycle consumers must share the same observation"
    );
}

#[test]
fn failed_topology_epoch_preserves_projection_but_is_not_membership_evidence() {
    use crate::ecs::topology::NativeTopology;
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let generation = harness.world().resource::<NativeTopology>().generation();
    let display = {
        let world = harness.world();
        world
            .query::<(Entity, &Display)>()
            .iter(world)
            .next()
            .expect("display")
            .0
    };
    harness.mock_state.set_display_inventory_available(false);
    harness.world().write_message(Event::DisplayConfigured {
        display_id: TEST_DISPLAY_ID,
    });
    harness.pump_frames(1);
    assert!(harness.world().get::<Display>(display).is_some());
    let snapshot = harness.world().resource::<NativeTopology>();
    assert!(snapshot.generation() > generation);
    assert!(!snapshot.is_complete());
    assert_eq!(snapshot.known_displays().count(), 0);
    harness.mock_state.set_display_inventory_available(true);
    harness.pump_frames(30);
    assert!(harness.world().resource::<NativeTopology>().is_complete());
    assert!(harness.world().get::<Display>(display).is_some());
}

#[test]
fn partial_topology_is_shared_without_another_consumer_retrying_in_the_same_frame() {
    use crate::ecs::topology::NativeTopology;
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    harness
        .mock_state
        .script_present_display_topology_queries(TEST_DISPLAY_ID, [Err(()), Ok(())]);
    let before = harness.mock_state.display_observation_count();
    harness.world().write_message(Event::DisplayConfigured {
        display_id: TEST_DISPLAY_ID,
    });
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.display_observation_count() - before, 1);
    assert!(!harness.world().resource::<NativeTopology>().is_complete());
    let world = harness.world();
    assert_eq!(world.query::<&Display>().iter(world).count(), 1);
    assert_eq!(world.query::<&LayoutStrip>().iter(world).count(), 1);
    harness.pump_frames(30);
    assert!(harness.world().resource::<NativeTopology>().is_complete());
}

#[test]
fn position_verifier_does_not_overwrite_the_presented_frame() {
    use crate::ecs::{
        DesiredWindowFrame, PresentedWindowFrame, VerifyWindowPosition, WindowFrameMotion,
    };
    let config: Config = (
        crate::config::MainOptions {
            animation_speed: Some(2.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(40);
    let entity = find_window_entity(0, harness.world());
    let current = harness
        .world()
        .get::<DesiredWindowFrame>(entity)
        .expect("desired")
        .0;
    let target = IRect::from_corners(
        current.min + IVec2::new(200, 0),
        current.max + IVec2::new(200, 0),
    );
    harness.world().entity_mut(entity).insert((
        DesiredWindowFrame(target),
        PresentedWindowFrame(current),
        WindowFrameMotion,
        VerifyWindowPosition::default(),
    ));
    harness.world().run_schedule(PostUpdate);
    let presented = harness
        .world()
        .get::<PresentedWindowFrame>(entity)
        .expect("presented")
        .0;
    assert_ne!(presented, target, "fixture must still be animating");
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(presented));
}

#[test]
fn wake_preserves_valid_floating_window_position() {
    let config = Config::try_from(r#"{"windows": {"test": {"title": ".*", "floating": true}}}"#)
        .expect("floating config");
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_window(0, |window| {
            window.frame = IRect::new(220, 160, 620, 460);
        });
    harness.pump_frames(20);
    harness.mock_state.os_move_window(0, Origin::new(220, 160));
    harness.mock_state.os_resize_window(0, Size::new(400, 300));
    harness.pump_frames(10);
    let entity = find_window_entity(0, harness.world());
    let before = harness
        .world()
        .get::<Window>(entity)
        .expect("window")
        .frame();
    assert_eq!(before, IRect::new(220, 160, 620, 460));
    harness.world().write_message(Event::SystemWoke {
        msg: "same topology".into(),
    });
    harness.pump_frames(2);
    {
        let world = harness.world();
        for mut refresh in world.query::<&mut RefreshWindowSizes>().iter_mut(world) {
            refresh.0 = std::time::Instant::now()
                .checked_sub(Duration::from_secs(6))
                .expect("six seconds before now");
        }
    }
    harness.pump_frames(40);
    let after = harness
        .world()
        .get::<Window>(entity)
        .expect("window")
        .frame();
    assert_eq!(after, before);
}

#[test]
fn display_heartbeat_discovers_a_connection_without_an_os_notification() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(20);
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(
            TEST_DISPLAY_WIDTH,
            0,
            TEST_DISPLAY_WIDTH + EXT_DISPLAY_WIDTH,
            EXT_DISPLAY_HEIGHT,
        ),
        vec![EXT_WORKSPACE_ID],
    );
    harness.pump_frames(30);
    let world = harness.world();
    assert_eq!(world.query::<&Display>().iter(world).count(), 2);
    assert_eq!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .filter(|strip| strip.id() == EXT_WORKSPACE_ID)
            .count(),
        1
    );
}

#[test]
fn partial_display_topology_does_not_remove_a_connected_display() {
    use bevy::ecs::system::RunSystemOnce as _;
    let mut harness = TestHarness::new()
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_workspace_window(100, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(20);
    let original_display = {
        let world = harness.world();
        world
            .query::<(Entity, &Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == EXT_DISPLAY_ID).then_some(entity))
            .expect("external display")
    };
    harness
        .mock_state
        .script_present_display_topology_queries(EXT_DISPLAY_ID, [Err(())]);
    harness.world().write_message(Event::DisplayConfigured {
        display_id: EXT_DISPLAY_ID,
    });
    harness
        .world()
        .run_system_once(crate::ecs::topology::refresh_topology)
        .expect("sample partial display topology");
    harness
        .world()
        .run_system_once(crate::ecs::display::reconcile_displays)
        .expect("reconcile partial display topology");
    assert!(
        harness.world().get::<Display>(original_display).is_some(),
        "one failed Space read is not evidence that its physical display was removed"
    );
    harness.world().resource_mut::<Messages<Event>>().clear();
    harness.pump_frames(50);
    let world = harness.world();
    assert_eq!(world.query::<&Display>().iter(world).count(), 2);
}

#[test]
fn same_display_origin_changes_rebase_frames_without_losing_strip_offset() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(20);
    let window = find_window_entity(0, harness.world());
    let initial = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(window)
        .expect("initial desired frame")
        .0;
    for offset in [
        IVec2::new(0, -TEST_DISPLAY_HEIGHT),
        IVec2::new(TEST_DISPLAY_WIDTH, -TEST_DISPLAY_HEIGHT),
        IVec2::new(-TEST_DISPLAY_WIDTH, 0),
        IVec2::ZERO,
    ] {
        harness.mock_state.add_display(
            TEST_DISPLAY_ID,
            IRect::from_corners(
                offset,
                offset + IVec2::new(TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            ),
            vec![TEST_WORKSPACE_ID],
        );
        let expected = IRect::from_corners(initial.min + offset, initial.max + offset);
        harness.mock_state.os_set_window_frame_silently(0, expected);
        harness.world().write_message(Event::DisplayMoved {
            display_id: TEST_DISPLAY_ID,
        });
        harness.pump_frames(30);

        assert_eq!(
            harness
                .world()
                .get::<crate::ecs::DesiredWindowFrame>(window)
                .expect("desired")
                .0,
            expected,
            "display translation must rebase the layout at {offset:?}"
        );
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(expected));
    }
}

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
                workspace.get::<DetachedSpace>().is_some(),
                "orphaned workspace should retain its source display"
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
                workspace.get::<DetachedSpace>().is_none(),
                "re-parented workspace should no longer be detached"
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
                    entity.get::<DetachedSpace>().is_some(),
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
            assert_window_size!(world, 100, EXT_DISPLAY_WIDTH / 4, ext_usable_height);
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
            assert_window_size!(world, 100, EXT_DISPLAY_WIDTH / 4, ext_usable_height);
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
        .on_iteration(2, move |world, state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 0, EXT_WORKSPACE_ID);
            state.update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
        })
        .on_iteration(3, move |world, _state| {
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

    let config = Config::try_from(r#"{"windows": {"test": {"title": ".*", "floating": true}}}"#)
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
            assert_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            state.update_window(100, |window| window.workspace_id = EXT_WORKSPACE_ID);
        })
        .on_iteration(3, move |world, state| {
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
                .query::<(&LayoutStrip, Option<&ChildOf>, Has<DetachedSpace>)>()
                .iter(world)
                .find(|(strip, _, _)| strip.id() == EXT_WORKSPACE_ID)
                .map(|(_, child, timeout)| (child.is_some(), timeout));
            let (has_parent, detached) =
                orphan.expect("external workspace strip should still exist");
            assert!(!has_parent, "orphaned workspace should have no parent");
            assert!(
                detached,
                "orphaned workspace should retain its source display"
            );
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
    assert_window_size!(harness.world(), 0, TEST_DISPLAY_WIDTH / 4, expected_height);
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

    assert_window_size!(harness.world(), 0, TEST_DISPLAY_WIDTH / 4, expected_height);
    assert_window_size!(harness.world(), 1, TEST_DISPLAY_WIDTH / 4, expected_height);
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

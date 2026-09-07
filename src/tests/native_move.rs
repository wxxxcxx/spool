use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use super::*;
use crate::commands::{Action, MoveFocus};
use crate::config::{Config, MainOptions};
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::native_space::{self, NativeMoveOwner};
use crate::ecs::workspace::WindowSpaceReassignmentPending;
use crate::events::Event;

const TARGET: WorkspaceId = TEST_WORKSPACE_ID + 1;

fn column_harness() -> (TestHarness, Entity, [Entity; 2]) {
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET],
        );
    harness.mock_state.enable_native_space_control();
    harness.pump_frames(30);
    let members = [
        find_window_entity(0, harness.world()),
        find_window_entity(1, harness.world()),
    ];
    let source = {
        let world = harness.world();
        let (entity, mut strip) = world
            .query::<(Entity, &mut LayoutStrip)>()
            .iter_mut(world)
            .find(|(_, strip)| strip.id() == TEST_WORKSPACE_ID)
            .unwrap();
        strip.append_column(Column::Stack(
            members.into_iter().map(StackItem::Single).collect(),
        ));
        entity
    };
    harness.pump_frames(3);
    (harness, source, members)
}

fn submit(harness: &mut TestHarness, follow: MoveFocus) {
    harness.world().write_message(Event::ActionRequested {
        action: Action::MoveColumnToSpace {
            window_id: 0,
            space_id: TARGET,
            move_focus: follow,
        },
    });
    harness
        .world()
        .run_system_once(native_space::handle_native_space_commands)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
}

fn reconcile(harness: &mut TestHarness) {
    harness
        .world()
        .run_system_once(native_space::reconcile_native_space_transactions)
        .unwrap();
}

#[test]
fn native_move_keeps_floating_windows_outside_layout_strips() {
    for resizable in [true, false] {
        let config = Config::try_from(
            r#"{"options":{"experimental_space_control":true},
                "windows":{"float":{"title":"Floating","floating":true}}}"#,
        )
        .unwrap();
        let mut harness = TestHarness::new()
            .with_config(config)
            .with_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![TEST_WORKSPACE_ID, TARGET],
            )
            .with_window(0, |window| {
                window.title = "Floating".into();
                window.resizable = resizable;
            })
            .with_workspace_window(1, TARGET, |_| {});
        harness.mock_state.enable_native_space_control();
        harness.pump_frames(30);
        let entity = find_window_entity(0, harness.world());
        assert!(
            harness
                .world()
                .get::<crate::ecs::Floating>(entity)
                .is_some()
        );
        let frame = harness.mock_state.actual_window_frame(0);
        let writes = harness.mock_state.frame_write_attempts(0);

        harness.world().write_message(Event::ActionRequested {
            action: Action::MoveWindowToSpace {
                window_id: 0,
                space_id: TARGET,
                move_focus: MoveFocus::Stay,
            },
        });
        harness.pump_frames(20);

        assert_eq!(harness.mock_state.window_workspace(0), Some(TARGET));
        let world = harness.world();
        assert!(world.get::<crate::ecs::Floating>(entity).is_some());
        assert!(world.get::<NativeMoveOwner>(entity).is_none());
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .all(|strip| !strip.contains(entity)),
            "native movement cannot give a floating window a tiled slot"
        );
        assert_eq!(harness.mock_state.actual_window_frame(0), frame);
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    }
}

#[test]
fn native_move_waits_for_unique_membership_before_committing_a_column() {
    let (mut harness, source, members) = column_harness();
    submit(&mut harness, MoveFocus::Stay);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0])]);
    reconcile(&mut harness);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .contains(entity)
        );
        assert!(harness.world().get::<NativeMoveOwner>(entity).is_some());
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_some()
        );
    }
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, []);
    reconcile(&mut harness);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TARGET)
        .unwrap();
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("stack preserved");
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    for entity in members {
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
}

#[test]
fn native_move_does_not_append_a_column_with_a_replaced_member() {
    let (mut harness, source, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    harness.world().despawn(members[1]);
    let replacement =
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TARGET, 1, IRect::new(0, 20, 400, 700));
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    reconcile(&mut harness);
    let world = harness.world();
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .all(|strip| !strip.contains(members[1])),
        "captured column data must never resurrect a retired entity"
    );
    assert!(
        world
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(members[0])
    );
    assert!(world.get::<NativeMoveOwner>(members[0]).is_none());
    assert!(
        world
            .get::<WindowSpaceReassignmentPending>(members[0])
            .is_some()
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        1,
        "do not follow a broken transaction"
    );
}

#[test]
fn partial_column_move_times_out_into_membership_recovery_without_os_rollback() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    harness
        .mock_state
        .update_window(1, |window| window.workspace_id = TEST_WORKSPACE_ID);
    harness.pump_frames(15);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_some()
        );
    }
    harness.pump_frames(30);
    let world = harness.world();
    let mut strips = world.query::<&LayoutStrip>();
    assert!(
        strips
            .iter(world)
            .any(|strip| strip.id() == TARGET && strip.contains(members[0]))
    );
    assert!(
        strips
            .iter(world)
            .any(|strip| strip.id() == TEST_WORKSPACE_ID && strip.contains(members[1]))
    );
    for entity in members {
        assert!(world.get::<NativeMoveOwner>(entity).is_none());
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_recovery_preserves_source_stack_after_timeout() {
    let (mut harness, source, members) = column_harness();
    let Some(Column::Stack(before)) = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .column_containing(members[0])
    else {
        panic!("source stack")
    };
    submit(&mut harness, MoveFocus::Stay);
    for id in 0..2 {
        harness.mock_state.update_window(id, |window| {
            window.workspace_id = TEST_WORKSPACE_ID;
        });
    }
    harness.pump_frames(45);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
        assert!(harness.world().get::<NativeMoveOwner>(entity).is_none());
    }
    let strip = harness.world().get::<LayoutStrip>(source).unwrap();
    let Some(Column::Stack(after)) = strip.column_containing(members[0]) else {
        panic!("the unchanged source column must remain a stack: {strip:?}");
    };
    assert_eq!(after, before);
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_recovery_preserves_column_at_an_unrequested_destination() {
    const ACTUAL: WorkspaceId = TARGET + 1;
    let (mut harness, _, members) = column_harness();
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, ACTUAL, false);
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(3);
    submit(&mut harness, MoveFocus::Follow);
    for id in 0..2 {
        harness
            .mock_state
            .update_window(id, |window| window.workspace_id = ACTUAL);
    }
    harness.pump_frames(45);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == ACTUAL)
        .unwrap();
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("members sharing an actual destination must retain their column");
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    for entity in members {
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_follow_does_not_focus_a_reused_window_id() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.native_space_intents().len(), 2);
    harness.world().despawn(members[0]);
    let replacement =
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TARGET, 0, IRect::new(0, 20, 400, 700));
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    let target = {
        let world = harness.world();
        world
            .query::<(Entity, &LayoutStrip)>()
            .iter(world)
            .find(|(_, strip)| strip.id() == TARGET)
            .unwrap()
            .0
    };
    harness
        .world()
        .entity_mut(target)
        .insert(native_space::VisibleNativeSpaceMarker);
    harness.mock_state.take_focus_requests();
    reconcile(&mut harness);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use super::*;
use crate::commands::{Action, Direction, MoveFocus, Operation};
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::window_frame::DisplayTransferFrame;
use crate::ecs::{RepositionMarker, ResizeMarker, Timeout};
use crate::events::Event;

fn harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(2).with_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );
    harness.pump_frames(15);
    harness.mock_state.take_focus_requests();
    harness
}

fn strip(harness: &mut TestHarness, id: WorkspaceId) -> Entity {
    let world = harness.world();
    world
        .query::<(Entity, &LayoutStrip)>()
        .iter(world)
        .find(|(_, strip)| strip.id() == id)
        .unwrap()
        .0
}

fn dispatch(harness: &mut TestHarness, operation: Operation) {
    dispatch_action(harness, Action::Window(operation));
}

fn dispatch_action(harness: &mut TestHarness, action: Action) {
    harness
        .world()
        .write_message(Event::action_requested(action));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
}

#[test]
fn multi_display_directional_transfer_selects_the_nearest_peer() {
    let mut harness = harness().with_display(3, IRect::new(0, -3000, 1920, -1800), vec![30]);
    harness.world().write_message(Event::DisplayChanged);
    harness.pump_frames(5);
    let peers = harness
        .world()
        .query_filtered::<&crate::manager::Display, Without<crate::ecs::ActiveDisplayMarker>>()
        .iter(harness.world())
        .map(crate::manager::Display::id)
        .collect::<Vec<_>>();
    assert_eq!(peers.len(), 2);
    for (id, bounds) in [
        (peers[0], IRect::new(0, -3000, 1920, -1800)),
        (peers[1], IRect::new(0, -1200, 1920, 0)),
    ] {
        set_display_geometry(
            &mut harness,
            id,
            if id == EXT_DISPLAY_ID {
                EXT_WORKSPACE_ID
            } else {
                30
            },
            bounds,
        );
    }
    let entity = find_window_entity(0, harness.world());
    dispatch(&mut harness, Operation::Move(Direction::North));
    assert_eq!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .map(|request| request.display_id),
        Some(peers[1])
    );
}

#[test]
fn multi_display_directional_focus_uses_focus_display_not_cursor_display() {
    let mut harness = harness();
    harness
        .world()
        .resource::<crate::manager::WindowManager>()
        .warp_mouse(IVec2::new(500, -600));
    dispatch(&mut harness, Operation::Focus(Direction::North));
    assert_eq!(
        harness.mock_state.cursor_position(),
        IVec2::new(
            EXT_DISPLAY_WIDTH / 2,
            (-EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT).midpoint(0)
        )
    );
}

#[test]
fn multi_display_mouse_next_visits_every_display() {
    let mut harness = harness().with_display(3, IRect::new(0, 768, 1920, 1968), vec![30]);
    harness.world().write_message(Event::DisplayChanged);
    harness.pump_frames(5);
    harness
        .world()
        .resource::<crate::manager::WindowManager>()
        .warp_mouse(IVec2::new(500, 400));
    for expected in [3, EXT_DISPLAY_ID, TEST_DISPLAY_ID] {
        dispatch_action(
            &mut harness,
            Action::Mouse(crate::commands::MouseMove::ToNextDisplay),
        );
        let cursor = harness.mock_state.cursor_position();
        let actual = harness
            .world()
            .query::<&crate::manager::Display>()
            .iter(harness.world())
            .find(|display| display.bounds().contains(cursor))
            .unwrap()
            .id();
        assert_eq!(actual, expected);
    }
}

#[test]
fn multi_display_window_next_uses_spatial_order() {
    let mut harness = harness().with_display(3, IRect::new(1024, 0, 2944, 1200), vec![30]);
    harness.world().write_message(Event::DisplayChanged);
    harness.pump_frames(5);
    let peers = harness
        .world()
        .query_filtered::<&crate::manager::Display, Without<crate::ecs::ActiveDisplayMarker>>()
        .iter(harness.world())
        .map(crate::manager::Display::id)
        .collect::<Vec<_>>();
    for (id, bounds) in [
        (peers[0], IRect::new(-1920, 0, 0, 1200)),
        (peers[1], IRect::new(1024, 0, 2944, 1200)),
    ] {
        set_display_geometry(
            &mut harness,
            id,
            if id == EXT_DISPLAY_ID {
                EXT_WORKSPACE_ID
            } else {
                30
            },
            bounds,
        );
    }
    let entity = find_window_entity(0, harness.world());
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
    assert_eq!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .map(|request| request.display_id),
        Some(peers[1])
    );
}

#[test]
fn multi_display_mouse_rejects_unknown_or_stale_ownership() {
    for condition in 0..7 {
        let mut harness = harness();
        harness
            .world()
            .resource::<crate::manager::WindowManager>()
            .warp_mouse(IVec2::new(500, 400));
        match condition {
            0 => harness.mock_state.set_display_inventory_available(false),
            1 => harness
                .mock_state
                .script_active_space_queries(EXT_DISPLAY_ID, [Err(())]),
            2 => {
                let target = strip(&mut harness, EXT_WORKSPACE_ID);
                harness.world().despawn(target);
            }
            3 => {
                let source = strip(&mut harness, TEST_WORKSPACE_ID);
                let target = strip(&mut harness, EXT_WORKSPACE_ID);
                let parent = harness.world().get::<ChildOf>(source).unwrap().parent();
                harness.world().entity_mut(target).insert(ChildOf(parent));
            }
            4 => harness.mock_state.add_display(
                EXT_DISPLAY_ID,
                IRect::new(0, -1400, 1920, -200),
                vec![EXT_WORKSPACE_ID],
            ),
            5 => harness
                .world()
                .resource::<crate::manager::WindowManager>()
                .warp_mouse(IVec2::new(10000, 10000)),
            _ => set_target_geometry(&mut harness, IRect::new(0, 0, 1920, 1200)),
        }
        let cursor = harness.mock_state.cursor_position();
        dispatch_action(
            &mut harness,
            Action::Mouse(crate::commands::MouseMove::ToNextDisplay),
        );
        assert_eq!(harness.mock_state.cursor_position(), cursor);
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

#[test]
fn multi_display_mouse_ignores_invalid_window_targets() {
    for condition in 0..4 {
        let mut harness = harness().with_window(2, |window| window.workspace_id = EXT_WORKSPACE_ID);
        harness.pump_frames(5);
        harness.mock_state.focus_window(0);
        harness.pump_frames(3);
        harness.mock_state.take_focus_requests();
        let entity = find_window_entity(2, harness.world());
        match condition {
            0 => {
                harness
                    .world()
                    .entity_mut(entity)
                    .insert(crate::ecs::ObservedWindowFrame(IRect::new(0, 0, 400, 400)));
            }
            1 => {
                harness
                    .world()
                    .entity_mut(entity)
                    .insert(crate::ecs::ObservedWindowFrame(IRect::new(
                        0, -600, 0, -500,
                    )));
            }
            2 => harness
                .mock_state
                .update_window(2, |window| window.workspace_id = TEST_WORKSPACE_ID),
            _ => {
                harness
                    .world()
                    .entity_mut(entity)
                    .insert(crate::ecs::WindowVisibility::Minimized);
                let target = strip(&mut harness, EXT_WORKSPACE_ID);
                harness
                    .world()
                    .get_mut::<LayoutStrip>(target)
                    .unwrap()
                    .append_column(Column::Single(entity));
            }
        }
        harness
            .world()
            .resource::<crate::manager::WindowManager>()
            .warp_mouse(IVec2::new(500, 400));
        dispatch_action(
            &mut harness,
            Action::Mouse(crate::commands::MouseMove::ToNextDisplay),
        );
        assert_eq!(
            harness.mock_state.cursor_position(),
            IVec2::new(
                EXT_DISPLAY_WIDTH / 2,
                (-EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT).midpoint(0)
            )
        );
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

#[test]
fn multi_display_mouse_landing_does_not_overflow_integer_limits() {
    for (left, top) in [
        (i32::MIN, -1200),
        (i32::MAX - EXT_DISPLAY_WIDTH, -1200),
        (0, i32::MIN),
        (0, i32::MAX - EXT_DISPLAY_HEIGHT),
    ] {
        let mut harness = harness();
        let bounds = IRect::new(
            left,
            top,
            left + EXT_DISPLAY_WIDTH,
            top + EXT_DISPLAY_HEIGHT,
        );
        set_target_geometry(&mut harness, bounds);
        harness
            .world()
            .resource::<crate::manager::WindowManager>()
            .warp_mouse(IVec2::new(500, 400));
        dispatch_action(
            &mut harness,
            Action::Mouse(crate::commands::MouseMove::ToNextDisplay),
        );
        assert!(bounds.contains(harness.mock_state.cursor_position()));
    }
}

#[test]
fn cross_display_waits_for_native_membership_before_committing() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let target = strip(&mut harness, EXT_WORKSPACE_ID);
    let entity = find_window_entity(0, harness.world());
    let cursor = harness.mock_state.cursor_position();
    let source_frame = (
        harness
            .world()
            .get::<crate::ecs::Position>(entity)
            .unwrap()
            .0,
        harness.world().get::<crate::ecs::Bounds>(entity).unwrap().0,
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .column_state(0)
            .unwrap()
            .width,
    );
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
    for _ in 0..3 {
        assert!(
            harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .contains(entity)
        );
        assert!(
            !harness
                .world()
                .get::<LayoutStrip>(target)
                .unwrap()
                .contains(entity)
        );
        assert_eq!(harness.mock_state.cursor_position(), cursor);
        assert_eq!(
            (
                harness
                    .world()
                    .get::<crate::ecs::Position>(entity)
                    .unwrap()
                    .0,
                harness.world().get::<crate::ecs::Bounds>(entity).unwrap().0,
                harness
                    .world()
                    .get::<LayoutStrip>(source)
                    .unwrap()
                    .column_state(0)
                    .unwrap()
                    .width,
            ),
            source_frame
        );
        harness.pump_frames(1);
    }
    harness
        .mock_state
        .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(entity)
    );
    assert!(
        harness
            .world()
            .get::<LayoutStrip>(target)
            .unwrap()
            .contains(entity)
    );
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn cross_display_stay_restores_source_focus_only_after_confirmation() {
    let mut harness = harness();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
    harness.pump_frames(3);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness
        .mock_state
        .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn cross_display_follow_uses_the_current_target_viewport() {
    let mut harness = harness();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
    harness.pump_frames(2);
    let bounds = IRect::new(3000, -1200, 4500, 0);
    set_target_geometry(&mut harness, bounds);
    harness
        .mock_state
        .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness.world().write_message(Event::DisplayChanged);
    harness.pump_frames(3);
    assert!(bounds.contains(harness.mock_state.cursor_position()));
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn cross_display_floating_transfer_keeps_size_and_waits_for_membership() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::Floating);
    harness.pump_frames(5);
    harness.mock_state.focus_window(0);
    harness.pump_frames(3);
    harness.mock_state.take_focus_requests();
    let size = harness.world().get::<crate::ecs::Bounds>(entity).unwrap().0;
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
    assert_eq!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .unwrap()
            .target
            .size(),
        size
    );
    harness.pump_frames(3);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness
        .mock_state
        .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert_eq!(
        harness.world().get::<crate::ecs::Bounds>(entity).unwrap().0,
        size
    );
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    assert!(
        harness
            .world()
            .query::<&LayoutStrip>()
            .iter(harness.world())
            .all(|strip| !strip.contains(entity))
    );
}

#[test]
fn cross_display_late_focus_cancels_completion_focus_and_pointer() {
    for mode in [MoveFocus::Follow, MoveFocus::Stay] {
        let mut harness = harness();
        dispatch(&mut harness, Operation::ToNextDisplay(mode));
        harness.pump_frames(2);
        harness
            .world()
            .write_message(Event::action_requested(Action::FocusWindow {
                window_id: 1,
            }));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness.world().resource_mut::<Messages<Event>>().clear();
        harness.pump_frames(3);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
        let cursor = harness.mock_state.cursor_position();
        harness
            .mock_state
            .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
        harness.pump_frames(3);
        assert!(harness.mock_state.take_focus_requests().is_empty());
        assert_eq!(harness.mock_state.cursor_position(), cursor);
        let target = strip(&mut harness, EXT_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        assert!(
            harness
                .world()
                .get::<LayoutStrip>(target)
                .unwrap()
                .contains(entity)
        );
    }
}

#[test]
fn cross_display_revalidates_target_and_membership_before_ax_write() {
    for condition in 0..6 {
        let mut harness = harness();
        let entity = find_window_entity(0, harness.world());
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
        let writes = harness.mock_state.frame_write_attempts(0);
        match condition {
            0 => {
                let target = strip(&mut harness, EXT_WORKSPACE_ID);
                harness.world().despawn(target);
            }
            1 => harness.mock_state.set_display_inventory_available(false),
            2 => harness
                .mock_state
                .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID),
            3 => set_target_geometry(&mut harness, IRect::new(100, -1200, 1600, 0)),
            4 => {
                harness
                    .world()
                    .entity_mut(entity)
                    .insert(crate::ecs::WindowVisibility::Minimized);
            }
            _ => {
                harness
                    .world()
                    .entity_mut(entity)
                    .insert(crate::ecs::Floating);
            }
        }
        harness.world().run_schedule(PostUpdate);
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
        assert!(
            harness
                .world()
                .get::<DisplayTransferFrame>(entity)
                .is_none()
        );
        if condition < 4 {
            assert!(
                harness
                    .world()
                    .get::<LayoutStrip>(source)
                    .unwrap()
                    .contains(entity)
            );
        } else {
            let target = strip(&mut harness, EXT_WORKSPACE_ID);
            assert!(
                !harness
                    .world()
                    .get::<LayoutStrip>(target)
                    .unwrap()
                    .contains(entity)
            );
        }
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

#[test]
fn cross_display_timeout_releases_ownership_without_follow_or_replay() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    harness.mock_state.constrain_frame_writes(0, true);
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
    let writes = harness.mock_state.frame_write_attempts(0);
    harness.pump_frames(45);
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::window_frame::DisplayTransferReadback>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(entity)
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes + 1);
}

#[test]
fn cross_display_retired_tab_member_cancels_the_captured_transfer() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let a = find_window_entity(0, harness.world());
    let b = find_window_entity(1, harness.world());
    harness
        .world()
        .get_mut::<LayoutStrip>(source)
        .unwrap()
        .convert_to_tabs(a, b)
        .unwrap();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
    harness.pump_frames(2);
    harness.world().despawn(b);
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        1,
        IRect::new(0, 20, 400, 700),
    );
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(5);
    let replacement = find_window_entity(1, harness.world());
    assert_ne!(replacement, b);
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(a)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(replacement)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(replacement)
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn cross_display_rejected_or_constrained_write_does_not_complete() {
    for rejected in [false, true] {
        let mut harness = harness();
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let target = strip(&mut harness, EXT_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        let cursor = harness.mock_state.cursor_position();
        if rejected {
            harness.mock_state.reject_frame_writes(0, true);
        } else {
            harness.mock_state.constrain_frame_writes(0, true);
        }
        let writes = harness.mock_state.frame_write_attempts(0);
        dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
        harness.pump_frames(5);
        assert!(harness.mock_state.frame_write_attempts(0) > writes);
        assert!(
            harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .contains(entity)
        );
        assert!(
            !harness
                .world()
                .get::<LayoutStrip>(target)
                .unwrap()
                .contains(entity)
        );
        assert_eq!(harness.mock_state.cursor_position(), cursor);
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

#[test]
fn cross_display_local_stack_move_does_not_also_transfer() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let target = strip(&mut harness, EXT_WORKSPACE_ID);
    let a = find_window_entity(0, harness.world());
    let b = find_window_entity(1, harness.world());
    harness
        .world()
        .get_mut::<LayoutStrip>(source)
        .unwrap()
        .append_column(Column::Stack(vec![
            StackItem::Single(a),
            StackItem::Single(b),
        ]));
    harness.mock_state.focus_window(1);
    harness.pump_frames(5);
    let cursor = harness.mock_state.cursor_position();
    dispatch(&mut harness, Operation::Move(Direction::North));
    assert!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(b)
    );
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(target)
            .unwrap()
            .contains(b)
    );
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .all_windows(),
        vec![b, a]
    );
    assert_eq!(harness.mock_state.cursor_position(), cursor);
    dispatch(&mut harness, Operation::Move(Direction::North));
    assert!(harness.world().get::<DisplayTransferFrame>(b).is_some());
    harness.pump_frames(2);
    harness
        .mock_state
        .update_window(1, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(b)
    );
    assert!(
        harness
            .world()
            .get::<LayoutStrip>(target)
            .unwrap()
            .contains(b)
    );
}

#[test]
fn cross_display_unknown_target_preserves_source_and_pending_geometry() {
    for (missing_strip, floating) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut harness = harness();
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        if floating {
            harness
                .world()
                .entity_mut(entity)
                .insert(crate::ecs::Floating);
            harness.pump_frames(5);
        }
        let before = harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .all_windows();
        let cursor = harness.mock_state.cursor_position();
        if missing_strip {
            let target = strip(&mut harness, EXT_WORKSPACE_ID);
            harness.world().despawn(target);
        } else {
            harness
                .mock_state
                .script_active_space_queries(EXT_DISPLAY_ID, [Err(())]);
        }
        dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
        assert_eq!(
            harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .all_windows(),
            before
        );
        assert!(harness.world().get::<RepositionMarker>(entity).is_none());
        assert!(harness.world().get::<ResizeMarker>(entity).is_none());
        assert_eq!(harness.mock_state.cursor_position(), cursor);
    }
}

#[test]
fn cross_display_cannot_overwrite_a_newer_size_request() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    let existing_timers = harness
        .world()
        .query_filtered::<Entity, With<Timeout>>()
        .iter(harness.world())
        .collect::<Vec<_>>();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
    let callbacks = harness
        .world()
        .query::<(Entity, &Timeout)>()
        .iter(harness.world())
        .filter(|(timer, _)| !existing_timers.contains(timer))
        .filter_map(|(_, timeout)| timeout.system_id)
        .collect::<Vec<_>>();
    let requested = IVec2::new(900, 600);
    harness
        .world()
        .entity_mut(entity)
        .insert(ResizeMarker(requested));
    for callback in callbacks {
        harness.world().run_system(callback).unwrap();
    }
    assert_eq!(
        harness.world().get::<ResizeMarker>(entity).unwrap().0,
        requested
    );
}

#[test]
fn cross_display_rejects_stale_ambiguous_or_failed_native_membership() {
    for condition in 0..3 {
        let mut harness = harness();
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        match condition {
            0 => harness
                .mock_state
                .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID),
            1 => harness
                .mock_state
                .script_workspace_membership_queries(EXT_WORKSPACE_ID, [Ok(vec![0])]),
            _ => harness
                .mock_state
                .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Err(())]),
        }
        assert_rejected(&mut harness, entity, source);
    }
}

#[test]
fn cross_display_size_is_part_of_the_transfer_not_an_unbound_timer() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    let before = harness
        .world()
        .query::<&Timeout>()
        .iter(harness.world())
        .count();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
    assert_eq!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .map(|v| v.target.size()),
        Some(IVec2::new(
            TEST_WINDOW_WIDTH,
            EXT_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT
        ))
    );
    assert_eq!(
        harness
            .world()
            .query::<&Timeout>()
            .iter(harness.world())
            .count(),
        before
    );
}

#[test]
fn cross_display_native_tabs_are_not_split_between_displays() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let target = strip(&mut harness, EXT_WORKSPACE_ID);
    let a = find_window_entity(0, harness.world());
    let b = find_window_entity(1, harness.world());
    harness
        .world()
        .get_mut::<LayoutStrip>(source)
        .unwrap()
        .convert_to_tabs(a, b)
        .unwrap();
    let group = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .tab_group(a)
        .unwrap();
    dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
    harness.pump_frames(2);
    harness
        .mock_state
        .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(2);
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .tab_group(a),
        Some(group.clone())
    );
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(target)
            .unwrap()
            .contains(a)
    );
    harness
        .mock_state
        .update_window(1, |window| window.workspace_id = EXT_WORKSPACE_ID);
    harness.pump_frames(3);
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(a)
    );
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(b)
    );
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(target)
            .unwrap()
            .tab_group(a),
        Some(group)
    );
}

fn assert_rejected(harness: &mut TestHarness, entity: Entity, source: Entity) {
    let before = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .all_windows();
    let cursor = harness.mock_state.cursor_position();
    let position = harness.world().get::<RepositionMarker>(entity).map(|v| v.0);
    let size = harness.world().get::<ResizeMarker>(entity).map(|v| v.0);
    dispatch(harness, Operation::ToNextDisplay(MoveFocus::Follow));
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .all_windows(),
        before
    );
    assert_eq!(
        harness.world().get::<RepositionMarker>(entity).map(|v| v.0),
        position
    );
    assert_eq!(
        harness.world().get::<ResizeMarker>(entity).map(|v| v.0),
        size
    );
    assert_eq!(harness.mock_state.cursor_position(), cursor);
    assert!(
        harness
            .world()
            .get::<DisplayTransferFrame>(entity)
            .is_none()
    );
}

#[test]
fn cross_display_waits_for_initial_window_defaults() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let entity = find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::WindowDefaultsPending);
    assert_rejected(&mut harness, entity, source);
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(entity)
            .is_none()
    );
}

#[test]
fn cross_display_rejects_a_target_strip_with_the_wrong_owner() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let target = strip(&mut harness, EXT_WORKSPACE_ID);
    let owner = harness.world().get::<ChildOf>(source).unwrap().parent();
    harness.world().entity_mut(target).insert(ChildOf(owner));
    let entity = find_window_entity(0, harness.world());
    assert_rejected(&mut harness, entity, source);
}

#[test]
fn cross_display_rejects_incomplete_fresh_topology_and_recovers() {
    for missing_inventory in [false, true] {
        let mut harness = harness();
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        if missing_inventory {
            harness.mock_state.set_display_inventory_available(false);
        } else {
            harness
                .mock_state
                .script_present_display_topology_queries(EXT_DISPLAY_ID, [Err(())]);
        }
        assert_rejected(&mut harness, entity, source);
        harness.mock_state.set_display_inventory_available(true);
        dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Stay));
        assert!(
            harness
                .world()
                .get::<DisplayTransferFrame>(entity)
                .is_some()
        );
        harness.pump_frames(2);
        harness
            .mock_state
            .update_window(0, |window| window.workspace_id = EXT_WORKSPACE_ID);
        harness.pump_frames(3);
        assert!(
            !harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .contains(entity)
        );
    }
}

#[test]
fn cross_display_tab_transfer_waits_for_every_member_to_be_available() {
    let mut harness = harness();
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let a = find_window_entity(0, harness.world());
    let b = find_window_entity(1, harness.world());
    harness
        .world()
        .get_mut::<LayoutStrip>(source)
        .unwrap()
        .convert_to_tabs(a, b)
        .unwrap();
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.world().run_schedule(Update);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(b)
            .is_some()
    );
    assert_rejected(&mut harness, a, source);
}

#[test]
fn cross_display_rejects_an_overflowing_destination_strip() {
    let mut harness = harness()
        .with_workspace_window(2, EXT_WORKSPACE_ID, |_| {})
        .with_workspace_window(3, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(10);
    let destination = strip(&mut harness, EXT_WORKSPACE_ID);
    for id in [2, 3] {
        let entity = find_window_entity(id, harness.world());
        let mut layout = harness.world().get_mut::<LayoutStrip>(destination).unwrap();
        let column = layout.column_id(entity).unwrap();
        layout
            .set_width_intent(
                column,
                crate::ecs::layout::WidthIntent::Absolute(f64::from(i32::MAX / 2)),
            )
            .unwrap();
    }
    let source = strip(&mut harness, TEST_WORKSPACE_ID);
    let entity = find_window_entity(0, harness.world());
    assert_rejected(&mut harness, entity, source);
}

fn set_target_geometry(harness: &mut TestHarness, bounds: IRect) {
    set_display_geometry(harness, EXT_DISPLAY_ID, EXT_WORKSPACE_ID, bounds);
}

fn set_display_geometry(harness: &mut TestHarness, id: u32, space: WorkspaceId, bounds: IRect) {
    harness.mock_state.add_display(id, bounds, vec![space]);
    let world = harness.world();
    let mut displays = world.query::<&mut crate::manager::Display>();
    let mut display = displays.iter_mut(world).find(|d| d.id() == id).unwrap();
    display.update_geometry(&crate::manager::Display::new(
        id,
        bounds,
        TEST_MENUBAR_HEIGHT,
    ));
}

#[test]
fn cross_display_plans_geometry_near_both_integer_limits_without_overflow() {
    for (left, top) in [
        (i32::MIN, -EXT_DISPLAY_HEIGHT),
        (i32::MAX - EXT_DISPLAY_WIDTH, -EXT_DISPLAY_HEIGHT),
        (0, i32::MIN),
        (0, i32::MAX - EXT_DISPLAY_HEIGHT),
    ] {
        let mut harness = harness();
        let bounds = IRect::new(
            left,
            top,
            left + EXT_DISPLAY_WIDTH,
            top + EXT_DISPLAY_HEIGHT,
        );
        set_target_geometry(&mut harness, bounds);
        let entity = find_window_entity(0, harness.world());
        dispatch(&mut harness, Operation::ToNextDisplay(MoveFocus::Follow));
        let request = harness.world().get::<DisplayTransferFrame>(entity).unwrap();
        let origin = request.target.min;
        let size = request.target.size();
        assert!(
            origin.x >= bounds.min.x
                && i64::from(origin.x) + i64::from(size.x) <= i64::from(bounds.max.x)
        );
        assert!(bounds.contains(request.viewport.min));
    }
}

#[test]
fn cross_display_rejects_invalid_or_overflowing_viewports_without_writes() {
    for top in [1200, u16::MAX] {
        let mut harness = harness();
        let config: crate::config::Config = (
            crate::config::MainOptions {
                padding_top: Some(top),
                ..Default::default()
            },
            vec![],
        )
            .into();
        harness.world().insert_resource(config);
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        assert_rejected(&mut harness, entity, source);
    }
    for dock in [
        crate::ecs::DockPosition::Left(i32::MAX),
        crate::ecs::DockPosition::Right(i32::MAX),
        crate::ecs::DockPosition::Bottom(i32::MAX),
        crate::ecs::DockPosition::Left(-1),
    ] {
        let mut harness = harness();
        set_target_geometry(
            &mut harness,
            IRect::new(
                100,
                i32::MAX - EXT_DISPLAY_HEIGHT,
                100 + EXT_DISPLAY_WIDTH,
                i32::MAX,
            ),
        );
        let target = strip(&mut harness, EXT_WORKSPACE_ID);
        let display = harness.world().get::<ChildOf>(target).unwrap().parent();
        harness.world().entity_mut(display).insert(dock);
        let source = strip(&mut harness, TEST_WORKSPACE_ID);
        let entity = find_window_entity(0, harness.world());
        assert_rejected(&mut harness, entity, source);
    }
}

use bevy::prelude::*;

use super::*;
use crate::commands::{Action, Direction, Operation};
use crate::ecs::{Floating, FocusedMarker, FullWidthMarker, RepositionMarker, ResizeMarker};
use crate::events::Event;

fn harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(10);
    harness.mock_state.focus_window(0);
    harness.pump_frames(5);
    harness.mock_state.take_focus_requests();
    harness
}

fn dispatch(harness: &mut TestHarness, actions: impl IntoIterator<Item = Action>) {
    for action in actions {
        harness
            .world()
            .write_message(Event::action_requested(action));
    }
    harness.world().run_schedule(PreUpdate);
}

#[test]
fn command_batch_consumes_every_directional_focus_before_os_confirmation() {
    let mut harness = harness();
    dispatch(
        &mut harness,
        std::iter::repeat_n(Action::Window(Operation::Focus(Direction::East)), 3),
    );
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1, 2, 3]);
    harness.world().run_schedule(PreUpdate);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn command_batch_applies_both_maximize_toggles() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    dispatch(
        &mut harness,
        std::iter::repeat_n(Action::Window(Operation::Maximize), 2),
    );
    assert!(harness.world().get::<FullWidthMarker>(entity).is_none());
}

#[test]
fn command_batch_preserves_named_and_directional_focus_order() {
    for named_first in [true, false] {
        let mut harness = harness();
        let mut actions = vec![
            Action::FocusWindow { window_id: 1 },
            Action::Window(Operation::Focus(Direction::East)),
        ];
        if !named_first {
            actions.reverse();
        }
        dispatch(&mut harness, actions);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            if named_first { vec![1, 2] } else { vec![1, 1] },
            "named_first={named_first}"
        );
    }
}

#[test]
fn command_batch_resize_uses_requested_focus_without_forging_confirmed_focus() {
    let mut harness = harness();
    let old = find_window_entity(0, harness.world());
    let next = find_window_entity(1, harness.world());
    dispatch(
        &mut harness,
        [
            Action::Window(Operation::Focus(Direction::East)),
            Action::Window(Operation::SetWidth(0.75)),
        ],
    );
    assert_eq!(
        harness.world().get::<ResizeMarker>(next).map(|v| v.0.x),
        Some(768)
    );
    assert!(harness.world().get::<ResizeMarker>(old).is_none());
    assert!(harness.world().get::<FocusedMarker>(old).is_some());
    assert!(harness.world().get::<FocusedMarker>(next).is_none());
}

#[test]
fn command_batch_invalid_action_does_not_block_later_actions() {
    let mut harness = harness();
    dispatch(
        &mut harness,
        [
            Action::Window(Operation::Focus(Direction::Nth(usize::MAX))),
            Action::FocusWindow { window_id: -1 },
            Action::Window(Operation::SetWidth(f64::NAN)),
            Action::Window(Operation::Focus(Direction::East)),
            Action::Window(Operation::SetWidth(0.5)),
        ],
    );
    let next = find_window_entity(1, harness.world());
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
    assert_eq!(
        harness.world().get::<ResizeMarker>(next).map(|v| v.0.x),
        Some(512)
    );
}

fn float(harness: &mut TestHarness, id: i32) -> Entity {
    let entity = find_window_entity(id, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(5);
    harness.mock_state.take_focus_requests();
    entity
}

#[test]
fn command_batch_floating_moves_accumulate_pending_geometry() {
    use bevy::ecs::system::RunSystemOnce;
    let mut harness = harness();
    let entity = float(&mut harness, 0);
    let before = harness
        .world()
        .run_system_once(move |windows: crate::ecs::params::Windows| {
            windows.requested_frame(entity).unwrap()
        })
        .unwrap();
    let step = harness
        .world()
        .resource::<crate::config::Config>()
        .floating_window_move_step();
    dispatch(
        &mut harness,
        std::iter::repeat_n(Action::Window(Operation::Move(Direction::East)), 2),
    );
    assert_eq!(
        harness.world().get::<RepositionMarker>(entity).unwrap().0.x,
        before.min.x + 2 * step
    );
}

#[test]
fn command_batch_other_layer_toggles_use_pending_focus() {
    let mut harness = harness();
    float(&mut harness, 3);
    dispatch(
        &mut harness,
        std::iter::repeat_n(Action::Window(Operation::FocusOtherLayer), 2),
    );
    assert_eq!(harness.mock_state.take_focus_requests(), vec![3, 0]);
}

#[test]
fn command_batch_does_not_redirect_hidden_pending_focus_to_old_window() {
    use crate::ecs::WindowVisibility;
    for visibility in [WindowVisibility::Hidden, WindowVisibility::Minimized] {
        let mut harness = harness();
        let old = find_window_entity(0, harness.world());
        let next = find_window_entity(1, harness.world());
        harness.world().entity_mut(next).insert(visibility);
        let before =
            [old, next].map(|entity| harness.world().get::<ResizeMarker>(entity).map(|v| v.0));
        dispatch(
            &mut harness,
            [
                Action::FocusWindow { window_id: 1 },
                Action::Window(Operation::SetWidth(0.75)),
            ],
        );
        assert_eq!(
            harness
                .world()
                .resource::<crate::ecs::focus::FocusCoordinator>()
                .snapshot()
                .requested_entity(),
            Some(next)
        );
        let after =
            [old, next].map(|entity| harness.world().get::<ResizeMarker>(entity).map(|v| v.0));
        assert_eq!(after, before);
    }
}

#[test]
fn command_batch_cancelled_focus_request_resumes_confirmed_target() {
    let mut harness = harness();
    let old = find_window_entity(0, harness.world());
    let next = find_window_entity(1, harness.world());
    dispatch(&mut harness, [Action::FocusWindow { window_id: 1 }]);
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.world().run_schedule(Update);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(next)
            .is_some()
    );
    assert_eq!(
        harness
            .world()
            .resource::<crate::ecs::focus::FocusCoordinator>()
            .snapshot()
            .requested_entity(),
        None
    );
    dispatch(&mut harness, [Action::Window(Operation::SetWidth(0.75))]);
    assert_eq!(
        harness.world().get::<ResizeMarker>(old).map(|v| v.0.x),
        Some(768)
    );
    assert!(harness.world().get::<ResizeMarker>(next).is_none());
}

#[test]
fn command_batch_pending_focus_on_another_display_does_not_use_source_geometry() {
    for floating in [false, true] {
        let mut harness = harness()
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
            .with_workspace_window(4, EXT_WORKSPACE_ID, |window| {
                window.frame =
                    IRect::new(TEST_DISPLAY_WIDTH + 100, 100, TEST_DISPLAY_WIDTH + 500, 600);
            });
        harness.pump_frames(5);
        let target = find_window_entity(4, harness.world());
        if floating {
            float(&mut harness, 4);
        }
        let source = find_window_entity(0, harness.world());
        dispatch(
            &mut harness,
            [
                Action::FocusWindow { window_id: 4 },
                Action::Window(Operation::SetWidth(0.75)),
                Action::Window(Operation::Center),
                Action::Window(Operation::Balance),
            ],
        );
        for entity in [source, target] {
            assert!(
                harness.world().get::<ResizeMarker>(entity).is_none(),
                "floating={floating}"
            );
            assert!(
                harness.world().get::<RepositionMarker>(entity).is_none(),
                "floating={floating}"
            );
        }
    }
}

#[cfg(feature = "lua")]
#[test]
fn command_batch_preserves_local_script_and_window_operation_order() {
    use bevy::ecs::system::RunSystemOnce;
    for script_first in [true, false] {
        let mut harness = harness();
        let snapshot = harness
            .world()
            .run_system_once(|state: crate::ecs::state::QueryStateParams| {
                state.extract_window_set()
            })
            .unwrap();
        let mut actions = vec![
            Action::Layout(snapshot.float(0).plan()),
            Action::Window(Operation::ToggleFloating),
        ];
        if !script_first {
            actions.reverse();
        }
        dispatch(&mut harness, actions);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        assert_eq!(
            harness.world().get::<Floating>(entity).is_some(),
            !script_first
        );
    }
}

#[cfg(feature = "lua")]
#[test]
fn command_batch_preserves_script_and_named_focus_order() {
    use bevy::ecs::system::RunSystemOnce;
    for script_first in [true, false] {
        let mut harness = harness();
        let snapshot = harness
            .world()
            .run_system_once(|state: crate::ecs::state::QueryStateParams| {
                state.extract_window_set()
            })
            .unwrap();
        let mut actions = vec![
            Action::Layout(snapshot.focus(1).plan()),
            Action::FocusWindow { window_id: 2 },
        ];
        if !script_first {
            actions.reverse();
        }
        dispatch(&mut harness, actions);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            if script_first { vec![1, 2] } else { vec![2, 1] },
            "script_first={script_first}"
        );
    }
}

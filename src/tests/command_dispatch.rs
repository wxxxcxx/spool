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

fn width_intent(harness: &mut TestHarness, entity: Entity) -> crate::ecs::layout::WidthIntent {
    let world = harness.world();
    world
        .query::<&crate::ecs::layout::LayoutStrip>()
        .iter(world)
        .find_map(|strip| {
            strip
                .column_state(strip.index_of(entity).ok()?)
                .map(|state| state.width)
        })
        .unwrap()
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
        width_intent(&mut harness, next),
        crate::ecs::layout::WidthIntent::ViewportRatio(0.75)
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
        width_intent(&mut harness, next),
        crate::ecs::layout::WidthIntent::ViewportRatio(0.5)
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
fn explicit_center_changes_only_the_selected_window_without_focus() {
    let mut harness = harness();
    let selected = float(&mut harness, 1);
    let focused = find_window_entity(0, harness.world());
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 1,
            operation: Operation::Center,
        }],
    );
    assert!(harness.world().get::<RepositionMarker>(selected).is_some());
    assert!(harness.world().get::<RepositionMarker>(focused).is_none());
    assert!(harness.world().get::<FocusedMarker>(focused).is_some());
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn explicit_resize_does_not_resize_the_focused_window() {
    let mut harness = harness();
    let selected = float(&mut harness, 1);
    let focused = find_window_entity(0, harness.world());
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 1,
            operation: Operation::Resize {
                axis: crate::commands::ResizeAxis::Width,
                direction: crate::commands::ResizeDirection::Grow,
            },
        }],
    );
    assert!(harness.world().get::<ResizeMarker>(selected).is_some());
    assert!(harness.world().get::<ResizeMarker>(focused).is_none());
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn explicit_maximize_restores_selected_floating_window_without_focus() {
    let mut harness = harness();
    let selected = float(&mut harness, 1);
    let focused = find_window_entity(0, harness.world());
    let action = Action::TargetedWindow {
        window_id: 1,
        operation: Operation::Maximize,
    };
    dispatch(&mut harness, [action.clone()]);
    assert!(harness.world().get::<FullWidthMarker>(selected).is_some());
    assert!(harness.world().get::<FullWidthMarker>(focused).is_none());
    dispatch(&mut harness, [action]);
    assert!(harness.world().get::<FullWidthMarker>(selected).is_none());
    assert!(harness.world().get::<FocusedMarker>(focused).is_some());
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn explicit_stack_toggle_changes_selected_column_and_preserves_focus() {
    let mut harness = harness();
    let selected = find_window_entity(2, harness.world());
    let focused = find_window_entity(0, harness.world());
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 2,
            operation: Operation::ToggleStack,
        }],
    );
    let world = harness.world();
    let mut strips = world.query::<&crate::ecs::layout::LayoutStrip>();
    let strip = strips
        .iter(world)
        .find(|strip| strip.contains(selected))
        .unwrap();
    assert!(matches!(
        strip.column_containing(selected),
        Some(crate::ecs::layout::Column::Stack(_))
    ));
    assert!(world.get::<FocusedMarker>(focused).is_some());
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn layout_balance_uses_explicit_reference_column_not_focus() {
    use spool_shared_types::commands::SpaceLayoutOperation;
    let mut harness = harness();
    let selected = find_window_entity(2, harness.world());
    {
        let world = harness.world();
        for mut strip in world
            .query::<&mut crate::ecs::layout::LayoutStrip>()
            .iter_mut(world)
        {
            if let Some(id) = strip.column_id(selected) {
                strip
                    .set_width_intent(id, crate::ecs::layout::WidthIntent::Absolute(300.0))
                    .unwrap();
            }
        }
    }
    dispatch(
        &mut harness,
        [Action::SpaceLayout {
            space_id: Some(TEST_WORKSPACE_ID),
            operation: SpaceLayoutOperation::Balance {
                reference_column: Some(3),
            },
        }],
    );
    for id in 0..4 {
        let entity = find_window_entity(id, harness.world());
        assert_eq!(
            width_intent(&mut harness, entity),
            crate::ecs::layout::WidthIntent::Absolute(300.0)
        );
    }
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn layout_column_out_of_range_does_not_fall_back_to_focus() {
    use spool_shared_types::commands::SpaceLayoutOperation;
    let mut harness = harness();
    dispatch(
        &mut harness,
        [Action::SpaceLayout {
            space_id: Some(TEST_WORKSPACE_ID),
            operation: SpaceLayoutOperation::Balance {
                reference_column: Some(99),
            },
        }],
    );
    for id in 0..4 {
        let entity = find_window_entity(id, harness.world());
        assert!(harness.world().get::<ResizeMarker>(entity).is_none());
    }
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn explicit_layout_visibility_toggle_parks_and_restores_target_space() {
    use spool_shared_types::commands::SpaceLayoutOperation;
    let mut harness = harness();
    let action = Action::SpaceLayout {
        space_id: Some(TEST_WORKSPACE_ID),
        operation: SpaceLayoutOperation::ToggleTiledVisibility,
    };
    dispatch(&mut harness, [action.clone()]);
    let entity = find_window_entity(1, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::tiled_visibility::ParkedTile>(entity)
            .is_some()
    );
    dispatch(&mut harness, [action]);
    assert!(
        harness
            .world()
            .get::<crate::ecs::tiled_visibility::ParkedTile>(entity)
            .is_none()
    );
}

#[test]
fn explicit_display_transfer_admits_only_selected_window_without_focusing_it() {
    let mut harness = TestHarness::new()
        .with_display(2, IRect::new(1024, 0, 2048, 768), vec![3])
        .with_windows(4);
    harness.pump_frames(10);
    harness.mock_state.focus_window(0);
    harness.pump_frames(5);
    harness.mock_state.take_focus_requests();
    let selected = find_window_entity(1, harness.world());
    let focused = find_window_entity(0, harness.world());
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 1,
            operation: Operation::ToNextDisplay(crate::commands::MoveFocus::Stay),
        }],
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(selected)
            .is_some()
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::native_space::NativeMoveOwner>(focused)
            .is_none()
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn explicit_floating_moves_accumulate_on_selected_window_without_focus() {
    let mut harness = harness();
    let selected = float(&mut harness, 1);
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 1,
            operation: Operation::Move(Direction::East),
        }],
    );
    let first = harness
        .world()
        .get::<RepositionMarker>(selected)
        .expect("selected window moved")
        .0;
    dispatch(
        &mut harness,
        [Action::TargetedWindow {
            window_id: 1,
            operation: Operation::Move(Direction::East),
        }],
    );
    let second = harness.world().get::<RepositionMarker>(selected).unwrap().0;
    assert!(second.x > first.x);
    assert_eq!(second.y, first.y);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn checked_command_receipt_waits_for_execution_and_rejects_missing_target() {
    use spool_shared_types::wire::{AdmissionStatus, CheckedAction, Response};
    let mut harness = harness();
    let (reply, received) = async_channel::bounded(1);
    harness
        .world()
        .write_message(Event::CheckedActionRequested {
            request: CheckedAction {
                request_id: "missing-window".into(),
                action: Action::TargetedWindow {
                    window_id: 999,
                    operation: Operation::Center,
                },
            },
            respond_to: reply,
        });
    assert!(received.try_recv().is_err());
    harness.world().run_schedule(PreUpdate);
    let Response::Admission(receipt) = received.try_recv().expect("execution receipt") else {
        panic!("admission response")
    };
    assert_eq!(receipt.request_id, "missing-window");
    assert_eq!(receipt.status, AdmissionStatus::Rejected);
    assert_eq!(receipt.code.as_deref(), Some("window_not_found"));
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn checked_native_focus_receipt_follows_actual_focus_admission() {
    use spool_shared_types::wire::{AdmissionStatus, CheckedAction, Response};
    let mut harness = harness();
    for (id, expected) in [
        (1, AdmissionStatus::Accepted),
        (999, AdmissionStatus::Rejected),
    ] {
        let (reply, received) = async_channel::bounded(1);
        harness
            .world()
            .write_message(Event::CheckedActionRequested {
                request: CheckedAction {
                    request_id: id.to_string(),
                    action: Action::FocusWindow { window_id: id },
                },
                respond_to: reply,
            });
        harness.world().run_schedule(PreUpdate);
        let Response::Admission(receipt) = received.try_recv().unwrap() else {
            panic!("admission response")
        };
        assert_eq!(receipt.status, expected);
    }
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
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
fn command_batch_withdrawn_focus_intent_does_not_redirect_to_confirmed_target() {
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
        Some(next)
    );
    let old_width = width_intent(&mut harness, old);
    dispatch(&mut harness, [Action::Window(Operation::SetWidth(0.75))]);
    assert_eq!(width_intent(&mut harness, old), old_width);
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

#[test]
fn dispatched_center_uses_the_active_display_and_wraps_the_mouse() {
    use bevy::ecs::system::RunSystemOnce;
    let mut harness = harness();
    let expected = harness
        .world()
        .run_system_once(
            |active: crate::ecs::params::ActiveDisplay, config: Res<crate::config::Config>| {
                active.actual_bounds(&config).center()
            },
        )
        .unwrap();
    dispatch(&mut harness, [Action::Window(Operation::Center)]);
    assert_eq!(harness.mock_state.cursor_position(), expected);
}

#[test]
fn dispatched_toggle_floating_flips_the_focused_window() {
    let mut harness = harness();
    let entity = find_window_entity(0, harness.world());
    dispatch(&mut harness, [Action::Window(Operation::ToggleFloating)]);
    assert!(harness.world().get::<Floating>(entity).is_some());
}

#[test]
fn dispatched_window_toggle_tiled_visibility_parks_and_restores() {
    let mut harness = harness();
    let entity = find_window_entity(1, harness.world());
    dispatch(
        &mut harness,
        [Action::Window(Operation::ToggleTiledVisibility)],
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::tiled_visibility::ParkedTile>(entity)
            .is_some()
    );
    dispatch(
        &mut harness,
        [Action::Window(Operation::ToggleTiledVisibility)],
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::tiled_visibility::ParkedTile>(entity)
            .is_none()
    );
}

#[test]
fn dispatched_quit_reaches_the_manager_once() {
    use crate::manager::{MockWindowManagerApi, WindowManager};
    use std::sync::{Arc, Mutex};

    let calls = Arc::new(Mutex::new(0));
    let recorded = Arc::clone(&calls);
    let mut manager = MockWindowManagerApi::new();
    manager.expect_quit().times(1).returning(move || {
        *recorded.lock().unwrap() += 1;
        Ok(())
    });
    let mut app = App::new();
    app.add_message::<Event>()
        .init_resource::<crate::lifecycle::Lifecycle>()
        .insert_resource(WindowManager(Box::new(manager)))
        .add_systems(PreUpdate, crate::commands::dispatch_actions);
    app.world_mut()
        .write_message(Event::action_requested(Action::Quit));
    app.update();
    assert_eq!(*calls.lock().unwrap(), 1);
}

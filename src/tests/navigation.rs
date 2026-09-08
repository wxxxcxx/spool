use super::{TestHarness, find_window_entity};
use crate::commands::{Action, Direction, FocusStep, Operation};
use crate::ecs::FocusedMarker;
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::params::Windows;
use crate::ecs::reconcile::WindowUnavailable;
use crate::events::Event;
use bevy::ecs::system::RunSystemOnce as _;

#[test]
fn directional_navigation_skips_retained_unavailable_middle_window() {
    for (start, operation, expected) in [
        (2, Operation::Focus(Direction::West), 0),
        (0, Operation::Focus(Direction::East), 2),
        (2, Operation::Focus(Direction::First), 0),
        (0, Operation::Focus(Direction::Last), 2),
        (0, Operation::Focus(Direction::Nth(1)), 2),
        (0, Operation::FocusStep(FocusStep::Next), 2),
        (2, Operation::FocusStep(FocusStep::Previous), 0),
    ] {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(20);
        let unavailable = find_window_entity(1, harness.world());
        harness.mock_state.os_withdraw_window(1);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        assert!(
            harness
                .world()
                .get::<WindowUnavailable>(unavailable)
                .is_some()
        );
        let world = harness.world();
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .any(|strip| strip.contains(unavailable))
        );
        harness.mock_state.focus_window(start);
        harness.world().write_message(Event::window_focused(start));
        harness.pump_frames(5);
        harness.world().write_message(Event::ActionRequested {
            action: Action::Window(operation),
        });
        harness.pump_frames(10);
        let target = find_window_entity(expected, harness.world());
        assert!(
            harness.world().get::<FocusedMarker>(target).is_some(),
            "directional navigation stopped at the unavailable middle identity"
        );
    }
}

#[test]
fn navigation_projection_preserves_stack_and_tab_structure_without_mutating_layout() {
    let mut harness = TestHarness::new().with_windows(6);
    harness.pump_frames(20);
    let entities = (0..6)
        .map(|id| find_window_entity(id, harness.world()))
        .collect::<Vec<_>>();
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    let mut original = LayoutStrip::new(super::TEST_WORKSPACE_ID);
    original.append_column(Column::Stack(vec![
        StackItem::Single(entities[0]),
        StackItem::Tabs(vec![entities[1], entities[2], entities[3]]),
        StackItem::Single(entities[4]),
    ]));
    original.append(entities[5]);
    let retained = original.clone();
    let projected = harness
        .world()
        .run_system_once(move |windows: Windows| windows.navigable_strip(&original))
        .unwrap();
    assert!(retained.contains(entities[1]));
    assert!(!projected.contains(entities[1]));
    assert_eq!(projected.len(), 2);
    assert_eq!(
        projected.tab_group(entities[2]),
        Some(vec![entities[2], entities[3]])
    );
    assert_eq!(projected.index_of(entities[4]).unwrap(), 0);
    assert_eq!(projected.right_neighbour(entities[4]), Some(entities[5]));
}

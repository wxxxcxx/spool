use super::{TestHarness, find_window_entity};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{Floating, MissionControlActive, RaiseWindow};
use crate::events::Event;

#[test]
fn confirmed_tiled_focus_raises_columns_from_far_to_near() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(0);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    harness.mock_state.focus_window(2);
    harness.pump_frames(20);
    assert_eq!(
        harness.mock_state.take_raise_requests(),
        vec![0, 4, 1, 3, 2]
    );
    harness.pump_frames(20);
    assert!(harness.mock_state.take_raise_requests().is_empty());
}

#[test]
fn raising_tiled_layer_includes_edge_slivers_in_distance_order() {
    use bevy::math::{IRect, IVec2};
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(20);
    let frames = [
        IRect::new(-391, 20, 9, 720),
        IRect::new(-391, 20, 9, 720),
        IRect::new(200, 20, 600, 720),
        IRect::new(1015, 20, 1415, 720),
        IRect::new(1015, 20, 1415, 720),
    ];
    for (id, frame) in (0..5).zip(frames) {
        let entity = find_window_entity(id, harness.world());
        harness
            .world()
            .entity_mut(entity)
            .insert(crate::ecs::ObservedWindowFrame(frame));
        harness
            .mock_state
            .update_window(id, |window| window.frame = frame);
    }
    harness.mock_state.take_raise_requests();
    let entity = find_window_entity(2, harness.world());
    harness.world().trigger(RaiseWindow {
        entity,
        with_strip: true,
    });
    let order = harness.mock_state.take_raise_requests();
    assert_eq!(order, vec![0, 4, 1, 3, 2]);
    let hit = |point| {
        order.iter().rev().copied().find(|&id| {
            harness
                .mock_state
                .actual_window_frame(id)
                .is_some_and(|frame| frame.contains(point))
        })
    };
    assert_eq!(hit(IVec2::new(4, 200)), Some(1));
    assert_eq!(hit(IVec2::new(1020, 200)), Some(3));
}

#[test]
fn layout_reordering_updates_stacking_without_a_focus_change() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    let mut strips = harness.world().query::<&mut LayoutStrip>();
    strips.single_mut(harness.world()).unwrap().swap(0, 4);
    harness.pump_frames(20);
    assert_eq!(
        harness.mock_state.take_raise_requests(),
        vec![4, 0, 1, 3, 2]
    );
}

#[test]
fn duplicate_and_stale_focus_notifications_do_not_repeat_stacking() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    for _ in 0..5 {
        harness.world().write_message(Event::window_focused(0));
        harness.world().write_message(Event::window_focused(2));
        harness.pump_frames(2);
    }
    assert!(harness.mock_state.take_raise_requests().is_empty());
    let focused = find_window_entity(2, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::FocusedMarker>(focused)
            .is_some()
    );
}

#[test]
fn tiled_stacking_excludes_floating_minimized_unavailable_and_moving_windows() {
    let mut harness = TestHarness::new().with_windows(7).with_focused_window(0);
    harness.pump_frames(20);
    harness.mock_state.os_minimize_window(1, true);
    harness.mock_state.os_withdraw_window(2);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(5);
    let floating = find_window_entity(3, harness.world());
    harness.world().entity_mut(floating).insert(Floating);
    let moving = find_window_entity(4, harness.world());
    harness
        .world()
        .entity_mut(moving)
        .insert(crate::ecs::native_space::NativeMoveOwner);
    harness.mock_state.take_raise_requests();
    let entity = find_window_entity(0, harness.world());
    harness.world().trigger(RaiseWindow {
        entity,
        with_strip: true,
    });
    assert_eq!(harness.mock_state.take_raise_requests(), vec![6, 5, 0]);
}

#[test]
fn floating_focus_leaves_the_tiled_layer_alone() {
    use crate::config::{MainOptions, WindowParams};
    let mut rule = WindowParams::new("^Window 4$", None);
    rule.floating = Some(true);
    let mut harness = TestHarness::new()
        .with_config((MainOptions::default(), vec![rule]).into())
        .with_windows(5)
        .with_focused_window(2);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    harness.mock_state.focus_window(4);
    harness.pump_frames(20);
    assert!(harness.mock_state.take_raise_requests().is_empty());
    let mut strips = harness.world().query::<&mut LayoutStrip>();
    strips.single_mut(harness.world()).unwrap().swap(0, 3);
    harness.pump_frames(20);
    assert!(harness.mock_state.take_raise_requests().is_empty());
}

#[test]
fn mission_control_defers_stacking_until_it_closes() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    harness.world().resource_mut::<MissionControlActive>().0 = true;
    let mut strips = harness.world().query::<&mut LayoutStrip>();
    strips.single_mut(harness.world()).unwrap().swap(0, 4);
    harness.pump_frames(5);
    assert!(harness.mock_state.take_raise_requests().is_empty());
    harness.world().resource_mut::<MissionControlActive>().0 = false;
    harness.pump_frames(20);
    assert_eq!(
        harness.mock_state.take_raise_requests(),
        vec![4, 0, 1, 3, 2]
    );
}

#[test]
fn stacking_never_raises_another_spaces_windows() {
    use super::{TEST_DISPLAY_ID, TEST_WORKSPACE_ID};
    use bevy::math::IRect;
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1024, 768),
            vec![TEST_WORKSPACE_ID, 99],
        )
        .with_windows(5)
        .with_workspace_window(99, 99, |_| {})
        .with_focused_window(2);
    harness.pump_frames(20);
    harness.mock_state.take_raise_requests();
    let entity = find_window_entity(2, harness.world());
    harness.world().trigger(RaiseWindow {
        entity,
        with_strip: true,
    });
    assert_eq!(
        harness.mock_state.take_raise_requests(),
        vec![0, 4, 1, 3, 2]
    );
}

#[test]
fn changing_focus_updates_both_edge_priorities_without_focus_requests() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(20);
    for (target, expected) in [(1, vec![4, 3, 0, 2, 1]), (3, vec![0, 1, 2, 4, 3])] {
        harness.mock_state.take_raise_requests();
        harness.mock_state.take_focus_requests();
        harness.mock_state.focus_window(target);
        harness.pump_frames(20);
        assert_eq!(harness.mock_state.take_raise_requests(), expected);
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

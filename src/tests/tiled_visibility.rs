use super::{TestHarness, find_window_entity};
use crate::commands::{Action, Operation};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::tiled_visibility::ParkedTile;
use crate::ecs::{DesiredWindowFrame, Floating, WindowVisibility};
use crate::events::Event;

fn toggle(harness: &mut TestHarness) {
    harness.world().write_message(Event::ActionRequested {
        action: Action::Window(Operation::ToggleTiledVisibility),
    });
    harness.pump_frames(30);
}

#[test]
fn tile_visibility_splits_at_focused_column() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(30);
    let focus_frame = harness.mock_state.actual_window_frame(2).unwrap();
    toggle(&mut harness);
    for id in [0, 1] {
        let frame = harness.mock_state.actual_window_frame(id).unwrap();
        assert!(frame.max.x <= 24, "left tile {id}: {frame:?}");
    }
    for id in [3, 4] {
        let frame = harness.mock_state.actual_window_frame(id).unwrap();
        assert!(frame.min.x >= 1000, "right tile {id}: {frame:?}");
    }
    let frame = harness.mock_state.actual_window_frame(2).unwrap();
    if focus_frame.center().x <= 512 {
        assert!(frame.max.x <= 24, "focus should park left: {frame:?}");
    } else {
        assert!(frame.min.x >= 1000, "focus should park right: {frame:?}");
    }
    let parked_frames = (0..5)
        .map(|id| harness.mock_state.actual_window_frame(id))
        .collect::<Vec<_>>();
    harness.mock_state.focus_window(4);
    harness.pump_frames(30);
    assert_eq!(
        (0..5)
            .map(|id| harness.mock_state.actual_window_frame(id))
            .collect::<Vec<_>>(),
        parked_frames
    );
}

#[test]
fn tile_visibility_with_floating_focus_uses_last_tiled_column() {
    let mut harness = TestHarness::new().with_windows(5).with_focused_window(2);
    harness.pump_frames(30);
    let floating = find_window_entity(4, harness.world());
    harness.world().entity_mut(floating).insert(Floating);
    harness.mock_state.focus_window(4);
    harness.pump_frames(30);
    toggle(&mut harness);
    for id in [0, 1] {
        assert!(harness.mock_state.actual_window_frame(id).unwrap().max.x <= 24);
    }
    assert!(harness.mock_state.actual_window_frame(3).unwrap().min.x >= 1000);
    assert!(harness.world().get::<ParkedTile>(floating).is_none());
}

#[test]
fn tile_visibility_parks_without_minimizing_and_restores_layout() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.pump_frames(30);
    let entities = (0..3)
        .map(|id| find_window_entity(id, harness.world()))
        .collect::<Vec<_>>();
    let original = entities
        .iter()
        .map(|&entity| harness.world().get::<DesiredWindowFrame>(entity).unwrap().0)
        .collect::<Vec<_>>();
    let mut strips = harness.world().query::<&LayoutStrip>();
    let before = format!("{:?}", strips.single(harness.world()).unwrap());
    toggle(&mut harness);
    for &entity in &entities {
        let frame = harness.world().get::<DesiredWindowFrame>(entity).unwrap().0;
        assert!(frame.min.x >= 1000 || frame.max.x <= 24, "{frame:?}");
        assert!(harness.world().get::<WindowVisibility>(entity).is_none());
        assert!(harness.world().get::<ParkedTile>(entity).is_some());
        let id = harness
            .world()
            .get::<crate::manager::Window>(entity)
            .unwrap()
            .id();
        assert_eq!(harness.mock_state.actual_window_frame(id), Some(frame));
    }
    assert_eq!(
        format!("{:?}", strips.single(harness.world()).unwrap()),
        before
    );
    toggle(&mut harness);
    for (&entity, expected) in entities.iter().zip(original) {
        assert_eq!(
            harness.world().get::<DesiredWindowFrame>(entity).unwrap().0,
            expected
        );
    }
    assert_eq!(
        format!("{:?}", strips.single(harness.world()).unwrap()),
        before
    );
}

#[test]
fn tile_visibility_preserves_stack_and_tab_structure() {
    use crate::ecs::layout::{Column, StackItem};
    let mut harness = TestHarness::new().with_windows(4).with_focused_window(0);
    harness.pump_frames(30);
    let entities = (0..4)
        .map(|id| find_window_entity(id, harness.world()))
        .collect::<Vec<_>>();
    let mut strips = harness.world().query::<&mut LayoutStrip>();
    {
        let mut strip = strips.single_mut(harness.world()).unwrap();
        for &entity in &entities {
            strip.remove(entity);
        }
        strip.append_column(Column::Stack(vec![
            StackItem::Tabs(vec![entities[0], entities[1]]),
            StackItem::Single(entities[2]),
        ]));
        strip.append_column(Column::Single(entities[3]));
    }
    harness.pump_frames(30);
    let before = format!("{:?}", strips.single(harness.world()).unwrap());
    toggle(&mut harness);
    assert_eq!(
        format!("{:?}", strips.single(harness.world()).unwrap()),
        before
    );
    let side = harness.world().get::<ParkedTile>(entities[0]).unwrap().side;
    for &entity in &entities[..3] {
        assert_eq!(
            harness.world().get::<ParkedTile>(entity).unwrap().side,
            side
        );
    }
    toggle(&mut harness);
    assert_eq!(
        format!("{:?}", strips.single(harness.world()).unwrap()),
        before
    );
}

#[test]
fn tile_visibility_excludes_parked_tiles_from_navigation_and_state() {
    use crate::ecs::params::{ActiveDisplay, Windows};
    use bevy::ecs::system::RunSystemOnce;
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.pump_frames(30);
    toggle(&mut harness);
    harness
        .world()
        .run_system_once(|windows: Windows, active: ActiveDisplay| {
            assert_eq!(windows.navigable_strip(active.active_strip()).len(), 0);
            assert_eq!(windows.tiled_iter().count(), 0);
            for (window, entity) in windows.iter() {
                assert!(!windows.get_tracked(entity).unwrap().2.is_visible());
                assert!(windows.find_tiled(window.id()).is_none());
            }
        })
        .unwrap();
    harness.mock_state.take_focus_requests();
    harness.world().write_message(Event::ActionRequested {
        action: Action::Window(Operation::FocusTiled),
    });
    harness.pump_frames(10);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn tile_visibility_does_not_touch_other_spaces() {
    use bevy::math::IRect;
    let mut harness = TestHarness::new()
        .with_windows(2)
        .with_display(2, IRect::new(-1200, 0, 0, 900), vec![20])
        .with_workspace_window(9, 20, |window| {
            window.frame = IRect::new(-1100, 20, -700, 768);
        })
        .with_focused_window(0);
    harness.pump_frames(30);
    let other = find_window_entity(9, harness.world());
    let original = harness.mock_state.actual_window_frame(9);
    toggle(&mut harness);
    assert!(harness.world().get::<ParkedTile>(other).is_none());
    assert_eq!(harness.mock_state.actual_window_frame(9), original);
}

#[test]
fn tile_visibility_ownership_does_not_survive_close_or_include_new_windows() {
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.pump_frames(30);
    toggle(&mut harness);
    let old = find_window_entity(1, harness.world());
    harness.mock_state.os_close_window(1);
    harness.pump_frames(30);
    assert!(harness.world().get::<ParkedTile>(old).is_none());
    harness = harness.with_window(1, |_| {});
    harness.pump_frames(30);
    let replacement = find_window_entity(1, harness.world());
    assert_ne!(old, replacement);
    assert!(harness.world().get::<ParkedTile>(replacement).is_none());
    toggle(&mut harness);
    assert!(harness.world().get::<ParkedTile>(replacement).is_none());
}

#[test]
fn tile_visibility_rapid_double_toggle_cancels_pending_parking() {
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.pump_frames(30);
    let original = harness.mock_state.actual_window_frame(0);
    for _ in 0..2 {
        harness.world().write_message(Event::ActionRequested {
            action: Action::Window(Operation::ToggleTiledVisibility),
        });
    }
    harness.pump_frames(30);
    let mut parked = harness.world().query::<&ParkedTile>();
    assert_eq!(parked.iter(harness.world()).count(), 0);
    assert_eq!(harness.mock_state.actual_window_frame(0), original);
}

#[test]
fn tile_visibility_leaves_floating_and_pre_minimized_windows_alone() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.pump_frames(30);
    let floating = find_window_entity(1, harness.world());
    harness.world().entity_mut(floating).insert(Floating);
    harness.mock_state.os_minimize_window(2, true);
    harness.pump_frames(30);
    let minimized = find_window_entity(2, harness.world());
    let original = harness.mock_state.actual_window_frame(1);
    toggle(&mut harness);
    toggle(&mut harness);
    assert_eq!(harness.mock_state.actual_window_frame(1), original);
    assert!(matches!(
        harness.world().get::<WindowVisibility>(minimized),
        Some(WindowVisibility::Minimized)
    ));
}

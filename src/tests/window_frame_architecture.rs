use bevy::prelude::*;

use crate::commands::{Action, Operation};
use crate::config::{Config, MainOptions, WindowParams};
use crate::ecs::{DesiredWindowFrame, ObservedWindowFrame, PresentedWindowFrame};
use crate::events::Event;

use super::*;

#[test]
fn moving_frames_validate_desired_and_pending_geometry_without_overflow() {
    use crate::ecs::params::Windows;
    use crate::ecs::{Bounds, Position, RepositionMarker, ResizeMarker};
    use bevy::ecs::system::RunSystemOnce;

    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    let entity = find_window_entity(0, harness.world());
    let world = harness.world();
    world.entity_mut(entity).insert((
        DesiredWindowFrame(IRect::new(100, 100, 400, 300)),
        RepositionMarker(IVec2::new(200, 200)),
        ResizeMarker(IVec2::new(600, 300)),
    ));
    assert_eq!(
        world
            .run_system_once(move |windows: Windows| windows.moving_frame(entity))
            .unwrap(),
        Some(IRect::new(200, 200, 800, 500))
    );
    world
        .entity_mut(entity)
        .insert(RepositionMarker(IVec2::new(i32::MAX, 200)));
    assert_eq!(
        world
            .run_system_once(move |windows: Windows| windows.moving_frame(entity))
            .unwrap(),
        None
    );
    world
        .entity_mut(entity)
        .remove::<(RepositionMarker, ResizeMarker)>();
    world
        .entity_mut(entity)
        .insert(DesiredWindowFrame(IRect::new(i32::MIN, 0, i32::MAX, 100)));
    assert_eq!(
        world
            .run_system_once(move |windows: Windows| windows.moving_frame(entity))
            .unwrap(),
        None
    );
    world.entity_mut(entity).remove::<DesiredWindowFrame>();
    world.entity_mut(entity).insert((
        Position(IVec2::new(i32::MAX, 0)),
        Bounds(IVec2::new(1, 100)),
    ));
    assert_eq!(
        world
            .run_system_once(move |windows: Windows| windows.moving_frame(entity))
            .unwrap(),
        None
    );
}

#[test]
fn unrepresentable_frame_requests_preserve_layout_inputs_and_os_geometry() {
    use crate::ecs::{Bounds, Position, RepositionMarker, ResizeMarker};

    for (origin, size) in [
        (IVec2::new(i32::MAX, 100), IVec2::new(1, 100)),
        (IVec2::new(100, i32::MAX), IVec2::new(100, 1)),
        (IVec2::new(100, 100), IVec2::new(0, 100)),
        (IVec2::new(100, 100), IVec2::new(100, -1)),
    ] {
        let mut rule = WindowParams::new(".*", None);
        rule.floating = Some(true);
        let mut harness = TestHarness::new()
            .with_config((MainOptions::default(), vec![rule]).into())
            .with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        let inputs = |world: &World| {
            (
                world.get::<Position>(entity).unwrap().0,
                world.get::<Bounds>(entity).unwrap().0,
                *world.get::<DesiredWindowFrame>(entity).unwrap(),
            )
        };
        let before = inputs(harness.world());
        let frame = harness.mock_state.actual_window_frame(0).unwrap();
        let writes = harness.mock_state.frame_write_attempts(0);
        harness
            .world()
            .entity_mut(entity)
            .insert((RepositionMarker(origin), ResizeMarker(size)));
        harness.pump_frames(5);
        assert_eq!(inputs(harness.world()), before);
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(frame));
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
        assert!(harness.world().get::<RepositionMarker>(entity).is_none());
        assert!(harness.world().get::<ResizeMarker>(entity).is_none());
    }
}

#[test]
fn desired_layout_precedes_presentation_and_os_commit() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(1.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let initial = harness
        .world()
        .get::<PresentedWindowFrame>(entity)
        .expect("presented frame")
        .0;

    harness.world().write_message(Event::ActionRequested {
        action: Action::Window(Operation::SetWidth(0.75)),
    });
    harness.pump_frames(1);

    let desired = harness
        .world()
        .get::<DesiredWindowFrame>(entity)
        .expect("desired frame")
        .0;
    let presented = harness
        .world()
        .get::<PresentedWindowFrame>(entity)
        .expect("presented frame")
        .0;
    let observed = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("observed frame")
        .0;

    assert_eq!(desired.width(), 768, "layout state owns the final target");
    assert!(
        presented.width() > initial.width() && presented.width() < desired.width(),
        "presentation should be an intermediate animation frame"
    );
    assert_eq!(observed, presented, "AX readback follows presentation");
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(presented),
        "the OS commit must use presentation, not jump to desired"
    );
}

#[test]
fn transient_fullscreen_probe_does_not_skip_window_padding_defaults() {
    let mut params = WindowParams::new(".*", None);
    params.horizontal_padding = Some(8);
    params.vertical_padding = Some(8);
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.mock_state.fail_full_screen_queries(0, 1);
    harness.mock_state.fail_full_screen_queries(1, 1);

    harness.pump_frames(10);

    assert_eq!(harness.mock_state.applied_horizontal_padding(0), Some(8));
    assert_eq!(harness.mock_state.applied_horizontal_padding(1), Some(8));
}

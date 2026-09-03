use bevy::prelude::*;

use crate::commands::{Command, Operation};
use crate::config::{Config, MainOptions, WindowParams};
use crate::ecs::{DesiredWindowFrame, ObservedWindowFrame, PresentedWindowFrame};
use crate::events::Event;

use super::*;

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

    harness.world().write_message(Event::Command {
        command: Command::Window(Operation::SetWidth(0.75)),
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

use bevy::prelude::*;
use objc2_core_foundation::CGPoint;

use crate::commands::{Command, Direction, MoveFocus, Operation};
use crate::config::{Config, MainOptions, WindowParams, parse_command};
use crate::ecs::display::FloatingLayer;
use crate::ecs::{
    ActiveWorkspaceMarker, Floating, FocusedMarker, NativeFullscreenMarker, Position,
    WindowVisibility, layout::LayoutStrip,
};
use crate::ecs::{RepositionMarker, SpawnWindowTrigger};
use crate::events::{Event, FocusSource};
use crate::manager::{Origin, Size, Window};
use crate::platform::Modifiers;
use crate::{assert_focused, assert_window_at, assert_window_size};

use super::*;

#[test]
fn native_fullscreen_transition_removes_window_from_original_strip_without_focus_marker() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |world, state| {
            let focused = world
                .query_filtered::<Entity, With<FocusedMarker>>()
                .iter(world)
                .collect::<Vec<_>>();
            for entity in focused {
                world.entity_mut(entity).remove::<FocusedMarker>();
            }

            state.update_window(0, |window| {
                window.workspace_id = FULLSCREEN_WORKSPACE_ID;
                window.is_full_screen = true;
            });
            state.activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
        })
        .on_iteration(1, |world, _state| {
            let fullscreen_window = find_window_entity(0, world);
            let sibling_window = find_window_entity(1, world);
            let mut strips = world.query::<(&LayoutStrip, Option<&NativeFullscreenMarker>)>();

            let original_strip = strips
                .iter(world)
                .find_map(|(strip, marker)| {
                    (strip.id() == TEST_WORKSPACE_ID && marker.is_none()).then_some(strip)
                })
                .expect("original strip");
            assert!(
                !original_strip.contains(fullscreen_window),
                "fullscreen window must not leave a reserved column in the original strip"
            );
            assert!(original_strip.contains(sibling_window));

            let (fullscreen_strip, fullscreen_marker) = strips
                .iter(world)
                .find(|(strip, _)| strip.id() == FULLSCREEN_WORKSPACE_ID)
                .expect("fullscreen strip");
            assert!(fullscreen_strip.contains(fullscreen_window));
            assert!(fullscreen_marker.is_some());
        })
        .on_iteration(2, |world, _state| {
            let fullscreen_window = find_window_entity(0, world);
            let sibling_window = find_window_entity(1, world);
            let mut strips = world.query::<&LayoutStrip>();

            let original_strip = strips
                .iter(world)
                .find(|strip| strip.id() == TEST_WORKSPACE_ID)
                .expect("original strip");
            assert!(original_strip.contains(fullscreen_window));
            assert!(original_strip.contains(sibling_window));
            assert_eq!(
                original_strip
                    .index_of(fullscreen_window)
                    .expect("restored fullscreen window index"),
                0
            );
            assert!(
                strips
                    .iter(world)
                    .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
            );
        })
        .run(vec![
            Event::Command {
                command: Command::PrintState,
            },
            Event::SpaceChanged,
            Event::SpaceDestroyed {
                space_id: FULLSCREEN_WORKSPACE_ID,
            },
        ]);
}

#[test]
fn native_window_move_submits_stable_space_intent_and_reconciles_os_membership() {
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            ..MainOptions::default()
        },
        Vec::new(),
    )
        .into();
    let harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET_SPACE_ID],
        )
        .with_config(config)
        .with_windows(1);
    harness.mock_state.enable_native_space_control();

    harness
        .on_iteration(1, |world, state| {
            assert_eq!(
                state.native_space_intents(),
                vec![crate::manager::NativeSpaceIntent::MoveWindows {
                    window_ids: vec![0],
                    space_id: TARGET_SPACE_ID,
                }]
            );
            assert_eq!(state.window_workspace(0), Some(TARGET_SPACE_ID));

            let window_entity = find_window_entity(0, world);
            let mut strips = world.query::<&LayoutStrip>();
            let source = strips
                .iter(world)
                .find(|strip| strip.id() == TEST_WORKSPACE_ID)
                .expect("source Space strip");
            assert!(!source.contains(window_entity));
            let target = strips
                .iter(world)
                .find(|strip| strip.id() == TARGET_SPACE_ID)
                .expect("target Space strip");
            assert!(target.contains(window_entity));
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: Command::MoveWindowToSpace {
                    window_id: 0,
                    space_id: TARGET_SPACE_ID,
                    move_focus: MoveFocus::Stay,
                },
            },
        ]);
}

#[test]
fn native_window_move_follow_switches_space_and_refocuses_window() {
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            space_switch_animation: Some(true),
            ..MainOptions::default()
        },
        Vec::new(),
    )
        .into();
    let harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET_SPACE_ID],
        )
        .with_config(config)
        .with_windows(1);
    harness.mock_state.enable_native_space_control();

    harness
        .on_iteration(1, |world, state| {
            assert_eq!(
                state.native_space_intents(),
                vec![
                    crate::manager::NativeSpaceIntent::MoveWindows {
                        window_ids: vec![0],
                        space_id: TARGET_SPACE_ID,
                    },
                    crate::manager::NativeSpaceIntent::Focus {
                        space_id: TARGET_SPACE_ID,
                        animate: true,
                    },
                ]
            );
            assert_eq!(state.window_workspace(0), Some(TARGET_SPACE_ID));
            let active_space = world
                .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
                .single(world)
                .expect("one active Space");
            assert_eq!(active_space.id(), TARGET_SPACE_ID);
            assert_focused!(world, 0);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: Command::MoveWindowToSpace {
                    window_id: 0,
                    space_id: TARGET_SPACE_ID,
                    move_focus: MoveFocus::Follow,
                },
            },
        ]);
}

#[test]
fn native_space_focus_submits_stable_space_id() {
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            space_switch_animation: Some(true),
            ..MainOptions::default()
        },
        Vec::new(),
    )
        .into();
    let harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET_SPACE_ID],
        )
        .with_config(config);
    harness.mock_state.enable_native_space_control();

    harness
        .on_iteration(0, |_world, state| {
            assert_eq!(
                state.native_space_intents(),
                vec![crate::manager::NativeSpaceIntent::Focus {
                    space_id: TARGET_SPACE_ID,
                    animate: true,
                }]
            );
        })
        .run(vec![Event::Command {
            command: Command::FocusSpace {
                space_id: TARGET_SPACE_ID,
            },
        }]);
}

#[test]
fn frontmost_floating_window_is_focused_after_setup() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    let config: Config = (MainOptions::default(), vec![params]).into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .with_focused_window(0)
        .on_iteration(0, |world, _state| {
            assert_focused!(world, 0);
            let entity = find_window_entity(0, world);
            assert!(world.entity(entity).contains::<Floating>());
        })
        .run(vec![Event::MenuOpened { window_id: 0 }]);
}

#[test]
fn floating_window_stays_floating_after_minimize_restore() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    let config: Config = (MainOptions::default(), vec![params]).into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .on_iteration(0, |world, _state| {
            let entity = find_window_entity(0, world);
            assert!(world.entity(entity).contains::<Floating>());
        })
        .on_iteration(1, |world, _state| {
            let entity = find_window_entity(0, world);
            let window = world.entity(entity);
            assert!(window.contains::<Floating>());
            assert!(window.contains::<WindowVisibility>());
        })
        .on_iteration(2, |world, _state| {
            let entity = find_window_entity(0, world);
            let window = world.entity(entity);
            assert!(window.contains::<Floating>());
            assert!(!window.contains::<WindowVisibility>());
            let mut strips = world.query::<&LayoutStrip>();
            assert!(strips.iter(world).all(|strip| !strip.contains(entity)));
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::WindowMinimized { window_id: 0 },
            Event::WindowDeminimized { window_id: 0 },
        ]);
}

/// Regression: a floating window placed by a grid rule must land at the active
/// display's usable origin (menubar + padding offset), not at (0, 0). Dropping
/// the display bounds origin previously sent grid windows to the primary
/// display's top-left corner (and onto the wrong display in multi-display
/// setups).
#[test]
fn floating_grid_window_uses_active_display_usable_origin() {
    let options = MainOptions {
        padding_left: Some(40),
        padding_top: Some(15),
        ..MainOptions::default()
    };

    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    // Cell (0,0) spanning the full 1x1 grid: origin should equal the usable
    // top-left, independent of the display size.
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (options, vec![params]).into();

    TestHarness::new()
        .with_config(config)
        .on_iteration(1, |world, state| {
            let origin = Origin::new(0, 0);
            let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
            let frame = IRect::from_corners(origin, origin + size);
            let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 0, frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(3, |world, _state| {
            // usable origin = (pad_left, menubar + pad_top) = (40, 20 + 15).
            assert_window_at!(world, 0, 40, TEST_MENUBAR_HEIGHT + 15);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: Command::PrintState,
            },
            Event::Command {
                command: Command::PrintState,
            },
            Event::Command {
                command: Command::PrintState,
            },
        ]);
}

#[test]
fn test_dont_focus() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 }, // 0
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        }, // 1
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::First)),
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
    ];

    let offscreen_right = TEST_DISPLAY_WIDTH - 5;

    let mut params = WindowParams::new(".*", None);
    params.dont_focus = Some(true);
    params.index = Some(100);
    let config: Config = (MainOptions::default(), vec![params]).into();

    let harness = TestHarness::new().with_config(config).with_windows(3);

    harness
        .on_iteration(1, move |world, state| {
            let origin = Origin::new(0, 0);
            let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
            let frame = IRect::from_corners(origin, origin + size);
            let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 3, frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(3, move |world, _| {
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 1, 400, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 2, 800, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 3, offscreen_right, TEST_MENUBAR_HEIGHT);
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn test_focus_window_by_number() {
    assert!(parse_command(&["window", "focus", "0"]).is_err());
    let command = parse_command(&["window", "focus", "2"]).unwrap();

    TestHarness::new()
        .with_windows(3)
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| assert_focused!(world, 1))
        .on_iteration(3, |world, _state| assert_focused!(world, 2))
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: command.clone(),
            },
            Event::Command {
                command: Command::Window(Operation::ToggleFloating),
            },
            Event::Command { command },
        ]);
}

#[test]
fn test_offscreen_windows_preserve_height() {
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT;

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::First)),
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(1, move |world, _state| {
            assert_window_size!(world, 4, TEST_WINDOW_WIDTH, expected_height);
            assert_window_size!(world, 3, TEST_WINDOW_WIDTH, expected_height);
            assert_window_size!(world, 2, TEST_WINDOW_WIDTH, expected_height);
            assert_window_size!(world, 1, TEST_WINDOW_WIDTH, expected_height);
            assert_window_size!(world, 0, TEST_WINDOW_WIDTH, expected_height);
        })
        .run(commands);
}

#[test]
fn test_sliver_smaller_than_edge_padding() {
    const PADDING: u16 = 8;
    const SLIVER: u16 = 1;

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::First)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
    ];

    let top_edge = TEST_MENUBAR_HEIGHT + i32::from(PADDING);
    let right_edge = TEST_DISPLAY_WIDTH - i32::from(PADDING);
    let offscreen_right = TEST_DISPLAY_WIDTH - i32::from(SLIVER);
    let offscreen_left = i32::from(SLIVER) - TEST_WINDOW_WIDTH;
    let left_edge = i32::from(PADDING);

    let config: Config = (
        MainOptions {
            sliver_width: Some(SLIVER),
            padding_top: Some(PADDING),
            padding_bottom: Some(PADDING),
            padding_left: Some(PADDING),
            padding_right: Some(PADDING),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(5)
        .on_iteration(2, move |world, _state| {
            assert_window_at!(world, 0, left_edge, top_edge);
            assert_window_at!(world, 1, left_edge + TEST_WINDOW_WIDTH, top_edge);
            assert_window_at!(world, 2, left_edge + 2 * TEST_WINDOW_WIDTH, top_edge);
            assert_window_at!(world, 3, offscreen_right, top_edge);
            assert_window_at!(world, 4, offscreen_right, top_edge);
        })
        .on_iteration(3, move |world, _state| {
            assert_window_at!(world, 0, offscreen_left, top_edge);
            assert_window_at!(world, 1, offscreen_left, top_edge);
            assert_window_at!(world, 2, right_edge - 3 * TEST_WINDOW_WIDTH, top_edge);
            assert_window_at!(world, 3, right_edge - 2 * TEST_WINDOW_WIDTH, top_edge);
            assert_window_at!(world, 4, right_edge - TEST_WINDOW_WIDTH, top_edge);
        })
        .run(commands);
}

#[test]
fn test_scrolling() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::First)),
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Swipe {
            delta: 0.2,
            fingers: 3,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    let config: Config = (
        MainOptions {
            swipe_gesture_fingers: Some(3),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(3)
        .on_iteration(3, move |world, _state| {
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 1, 400, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 2, 800, TEST_MENUBAR_HEIGHT);
        })
        .on_iteration(5, move |world, _state| {
            assert_window_at!(world, 0, -352, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 1, 48, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 2, 448, TEST_MENUBAR_HEIGHT);
        })
        .run(commands);
}

#[test]
#[allow(clippy::float_cmp)]
fn test_scrolling_stop() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Swipe {
            delta: 0.3,
            fingers: 3,
        },
        Event::TouchpadDown,
    ];

    let config: Config = (
        MainOptions {
            swipe_gesture_fingers: Some(3),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(3)
        .on_iteration(3, |world, _state| {
            use crate::ecs::Scrolling;
            let mut query = world.query::<&Scrolling>();
            let scroll = query.single(world).unwrap();
            assert_eq!(scroll.velocity, 0.0);
            assert!(scroll.is_user_swiping);
        })
        .run(commands);
}

#[test]
fn test_window_hidden_ratio() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Swipe {
            delta: 0.3,
            fingers: 3,
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::First)),
        },
    ];

    let config: Config = (
        MainOptions {
            window_hidden_ratio: Some(0.5),
            animation_speed: Some(10000.0),
            swipe_gesture_fingers: Some(3),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .on_iteration(2, |world, _state| {
            let entity = find_window_entity(0, world);
            let window = world.get::<Window>(entity).expect("finding window");
            assert!(window.frame().min.x < 0);
        })
        .run(commands);
}

#[test]
fn test_window_swap_brings_focused_into_view() {
    // After Center, id=4 is at the centered position. Swap(Last) bubbles
    // id=4 to column 4 (layout x=1600); with the strip at +312 that would
    // put id=4 off-screen to the right (1912). ensure_visible_in_strip
    // scrolls the strip by exactly the shortfall so id=4 sits at the right
    // edge of the viewport (max.x - width = 624). The strip does NOT
    // re-anchor id=4 to its old centered position — there was room to the
    // right, so it slides there. id=0 takes the slot immediately to the
    // left.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::Center),
        },
        Event::Command {
            command: Command::Window(Operation::Swap(Direction::Last)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    let centered = (TEST_DISPLAY_WIDTH - TEST_WINDOW_WIDTH) / 2;
    let right_edge = TEST_DISPLAY_WIDTH - TEST_WINDOW_WIDTH;

    TestHarness::new()
        .with_config(config)
        .with_windows(5)
        .on_iteration(2, move |world, _state| {
            assert_window_at!(world, 0, centered, TEST_MENUBAR_HEIGHT);
        })
        .on_iteration(4, move |world, _state| {
            assert_window_at!(world, 0, right_edge, TEST_MENUBAR_HEIGHT);
            assert_window_at!(
                world,
                4,
                right_edge - TEST_WINDOW_WIDTH,
                TEST_MENUBAR_HEIGHT
            );
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn test_window_swap_keeps_strip_when_in_view() {
    // Two windows fit the viewport. Swap(West) on the focused (right)
    // window swaps the columns: both new layout slots are still inside the
    // viewport with the strip where it is, so ensure_visible_in_strip does
    // nothing. The per-window animation slides each window into the other's
    // old position.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::Window(Operation::Swap(Direction::West)),
        },
    ];

    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .on_iteration(2, |world, _state| {
            assert_window_at!(world, 1, 0, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 0, TEST_WINDOW_WIDTH, TEST_MENUBAR_HEIGHT);
            assert_focused!(world, 1);
        })
        .run(commands);
}

#[test]
fn test_rapid_focus_not_swallowed() {
    let mut harness = TestHarness::new().with_windows(5);

    harness.run(vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ]);

    assert_focused!(harness.world(), 4);

    let focus_west = Event::Command {
        command: Command::Window(Operation::Focus(Direction::West)),
    };
    for _ in 0..3 {
        harness
            .app
            .world_mut()
            .write_message::<Event>(focus_west.clone());
        harness.app.update();
    }

    assert_focused!(harness.world(), 1);
}

#[test]
fn test_stale_focus_event_ignored() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::window_focused(4),
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(1, |world, _state| {
            assert_focused!(world, 1);
        })
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 1);
        })
        .run(commands);
}

#[test]
fn stale_known_focus_event_does_not_leave_resolution_pending() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::window_focused(4),
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 1);
            let focused = world
                .query_filtered::<Entity, With<FocusedMarker>>()
                .single(world)
                .expect("focus anchor before simulated marker loss");
            world.entity_mut(focused).remove::<FocusedMarker>();
        })
        .on_iteration(5, |world, _state| {
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn unknown_focus_event_preserves_the_tracked_focus_anchor() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::window_focused(999),
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(1, |world, _state| {
            assert_focused!(world, 0);
        })
        .on_iteration(4, |world, _state| {
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn ui_element_focus_notification_revalidates_the_app_focused_window() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::FocusRevalidationRequested {
            pid: TEST_PROCESS_ID,
            source: FocusSource::AccessibilityUiElement,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = Some(1);
            });
        })
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 1);
        })
        .run(commands);
}

#[test]
fn stale_focus_retry_cannot_override_a_newer_app_focus_resolution() {
    const SECOND_PID: i32 = TEST_PROCESS_ID + 1;
    const SECOND_WINDOW_ID: i32 = 10;

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::FocusRevalidationRequested {
            pid: TEST_PROCESS_ID,
            source: FocusSource::ApplicationFrontSwitch,
        },
        Event::FocusRevalidationRequested {
            pid: SECOND_PID,
            source: FocusSource::AccessibilityUiElement,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_app(SECOND_PID, "test.second", "SecondApp", |app| {
            app.focused_window_id = Some(SECOND_WINDOW_ID);
        })
        .with_windows(1)
        .with_app_window(SECOND_PID, SECOND_WINDOW_ID, |_| {})
        .on_iteration(0, |_world, state| {
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = None;
            });
        })
        .on_iteration(2, |world, state| {
            assert_focused!(world, SECOND_WINDOW_ID);
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = Some(0);
            });
        })
        .on_iteration(3, |world, _state| {
            assert_focused!(world, SECOND_WINDOW_ID);
        })
        .run(commands);
}

#[test]
fn mouse_hit_on_an_untracked_window_confirms_focus_outside_spool() {
    const EXTERNAL_WINDOW_ID: i32 = 999;
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |_world, state| {
            state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                EXTERNAL_WINDOW_ID,
                IRect::new(800, 100, 900, 200),
            );
            state.simulate_window_click(EXTERNAL_WINDOW_ID);
        })
        .on_iteration(1, |world, _state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(focused.iter(world).count(), 0);
        })
        .on_iteration(2, |world, _state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(
                focused.iter(world).count(),
                0,
                "confirmed external focus must suppress tiled-focus recovery"
            );
        })
        .run(commands);
}

#[test]
fn focus_query_timeout_does_not_suppress_later_focus_recovery() {
    let mut commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::FocusRevalidationRequested {
            pid: TEST_PROCESS_ID,
            source: FocusSource::AccessibilityWindow,
        },
    ];
    commands.extend((0..7).map(|_| Event::Command {
        command: Command::PrintState,
    }));

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |_world, state| {
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = None;
            });
        })
        .on_iteration(6, |world, _state| {
            let focused = world
                .query_filtered::<Entity, With<FocusedMarker>>()
                .single(world)
                .expect("focus anchor before simulated marker loss");
            world.entity_mut(focused).remove::<FocusedMarker>();
        })
        .on_iteration(8, |world, _state| {
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn test_repeated_external_focus_reshuffles_already_focused_window() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(1, |world, _state| {
            assert_focused!(world, 0);

            let mut query = world.query::<(Entity, &LayoutStrip, Has<ActiveWorkspaceMarker>)>();
            let (entity, _, _) = query
                .iter(world)
                .find(|(_, _, active)| *active)
                .expect("active strip");
            world.commands().entity(entity).insert((
                Position(Origin::new(0, 0)),
                RepositionMarker(Origin::new(-TEST_DISPLAY_WIDTH, 0)),
            ));
        })
        .on_iteration(2, |_world, state| {
            state.focus_window(0);
        })
        .on_iteration(4, |world, _state| {
            assert_focused!(world, 0);
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
        })
        .run(commands);
}

// When the focused window leaves the active strip (e.g. it just became
// floating, or the OS handed focus to an off-strip window), window_focus
// east/west must enter the strip from the appropriate side rather than
// silently doing nothing.
fn focused_window_id(world: &mut World) -> i32 {
    let mut q = world.query::<(&Window, Has<crate::ecs::FocusedMarker>)>();
    q.iter(world)
        .find_map(|(w, f)| f.then_some(w.id()))
        .expect("a focused window")
}

fn entity_to_window_id(world: &mut World, entity: Entity) -> i32 {
    let mut q = world.query::<(&Window, Entity)>();
    q.iter(world)
        .find_map(|(w, e)| (e == entity).then_some(w.id()))
        .expect("entity must be a Window")
}

fn active_strip_first_id(world: &mut World) -> i32 {
    let entity = {
        let mut q = world.query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
        let strip = q.single(world).expect("a single active strip");
        strip
            .first()
            .expect("strip should have a column")
            .top()
            .expect("column should have a top entity")
    };
    entity_to_window_id(world, entity)
}

fn active_strip_last_id(world: &mut World) -> i32 {
    let entity = {
        let mut q = world.query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
        let strip = q.single(world).expect("a single active strip");
        strip
            .last()
            .expect("strip should have a column")
            .top()
            .expect("column should have a top entity")
    };
    entity_to_window_id(world, entity)
}

// Strip the currently focused entity out of every LayoutStrip so the
// "focused window not in active strip" condition is reproduced regardless
// of how the harness happened to populate the strip. Without this, the
// init-time duplicate-insertion in the test scheduler keeps the entity in
// the strip and the bug is masked.
fn remove_focused_from_all_strips(world: &mut World) {
    let entity = {
        let mut q = world.query_filtered::<Entity, With<crate::ecs::FocusedMarker>>();
        q.single(world).expect("a single focused entity")
    };
    let mut q = world.query::<&mut LayoutStrip>();
    for mut strip in q.iter_mut(world) {
        while strip.contains(entity) {
            strip.remove(entity);
        }
    }
}

#[test]
fn test_focus_recovers_when_focused_window_is_outside_strip() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            // Make the focused entity genuinely live outside any strip,
            // mirroring the state the user reported: the OS handed focus
            // to a window Spool doesn't track on its active strip.
            remove_focused_from_all_strips(world);
        })
        .on_iteration(1, |world, _state| {
            // Before the fix: get_window_in_direction returns None because
            // active_strip.index_of(focused) fails for a window that's not
            // in the strip, so East is a silent no-op and focus stays on 0.
            let focused = focused_window_id(world);
            assert_ne!(
                focused, 0,
                "focus must leave the off-strip window 0 when pressing East",
            );
            let expected = active_strip_first_id(world);
            assert_eq!(
                focused, expected,
                "East from outside the strip enters at the first (leftmost) column",
            );
        })
        .run(commands);
}

#[test]
fn test_focus_west_from_outside_strip_enters_at_last_column() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::West)),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            remove_focused_from_all_strips(world);
        })
        .on_iteration(1, |world, _state| {
            let focused = focused_window_id(world);
            let expected = active_strip_last_id(world);
            assert_ne!(focused, 0);
            assert_eq!(
                focused, expected,
                "West from outside the strip enters at the last (rightmost) column",
            );
        })
        .run(commands);
}

#[test]
fn mouse_in_bottom_right_corner_does_not_change_focus() {
    // Focus window 2 explicitly, then move cursor into the bottom-right 30x30
    // dead zone. The corner gate should suppress the focus-follow-mouse event,
    // so focus stays on window 2.
    //
    // Test display is 1024x768 with no Dock, so the dead zone is
    // x >= 994, y >= 738. Cursor at (1010, 750) is inside it. The mock's
    // find_window_at_point always returns window 0, so without the gate the
    // FFM event would shift focus to window 0; with the gate it should not.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::West)),
        },
        Event::MouseMoved {
            point: CGPoint {
                x: 1010.0,
                y: 750.0,
            },
            modifiers: Modifiers::empty(),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(2, |world, _state| {
            // After MouseMoved into corner dead zone: focus should remain on window 2
            // because the corner gate suppressed the focus-follow-mouse event.
            assert_focused!(world, 0);
        })
        .run(commands);
}

#[test]
fn mouse_outside_corner_still_changes_focus() {
    use crate::events::Event;
    use crate::platform::Modifiers;
    use objc2_core_foundation::CGPoint;

    // Cursor at (500, 400), middle of the display, outside the dead zone.
    // FFM should fire normally and switch focus.
    //
    // Focus window 2 first, then move cursor away from the corner. The mock's
    // find_window_at_point always returns window 0, so FFM lands focus on
    // window 0.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::West)),
        },
        Event::MouseMoved {
            point: CGPoint { x: 500.0, y: 400.0 },
            modifiers: Modifiers::empty(),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(2, |world, _state| {
            // After MouseMoved outside corner: FFM should have fired and changed focus.
            assert_focused!(world, 1);
        })
        .run(commands);
}

#[test]
fn toggle_floating_layer_flips_state() {
    fn current_layer(world: &mut World) -> FloatingLayer {
        let mut query = world.query::<&FloatingLayer>();
        *query
            .query(world)
            .iter()
            .find(|layer| layer.workspace_id == TEST_WORKSPACE_ID)
            .expect("active workspace has FloatingLayer")
    }

    let commands = vec![
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::ToggleFloatingLayer),
        },
        Event::Command {
            command: Command::Window(Operation::ToggleFloatingLayer),
        },
    ];

    TestHarness::new()
        .with_config(Config::default())
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            assert!(!current_layer(world).front);
        })
        .on_iteration(1, |world, _state| {
            assert!(current_layer(world).front);
        })
        .on_iteration(2, |world, _state| {
            assert!(!current_layer(world).front);
        })
        .run(commands);
}

#[test]
fn focus_floating_ignores_floats_from_other_spaces() {
    let workspaces = vec![TEST_WORKSPACE_ID, TEST_WORKSPACE_ID + 1];
    let harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            workspaces,
        )
        .with_workspace_window(0, TEST_WORKSPACE_ID, |_| {})
        .with_workspace_window(99, TEST_WORKSPACE_ID + 1, |w| {
            w.frame = IRect::new(600, 0, 600 + TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
        });

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::FocusFloating),
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
    ];

    harness
        .on_iteration(2, |world, _state| {
            let off_workspace_float = find_window_entity(99, world);
            world.entity_mut(off_workspace_float).insert(Floating);
            assert_focused!(world, 0);
        })
        .on_iteration(3, |world, _state| {
            let active_float = find_window_entity(0, world);
            world.entity_mut(active_float).insert(Floating);
            assert_focused!(world, 0);
        })
        .on_iteration(4, |world, _state| {
            assert_focused!(world, 0);
        })
        .run(commands);
}

/// With `auto_center` off, a reshuffle around the leftmost window of a
/// scrollable strip must pin the strip to the left edge — the leftmost
/// window's left edge must touch the display's left edge, never leaving empty
/// space to its left.
///
/// A delayed OS frame can still show a window at the right-edge sliver after
/// its Space becomes visible. This test injects that stale frame and verifies
/// the reshuffle clamps the strip back to the left edge.
#[test]
fn test_reshuffle_leftmost_pins_strip_to_left_edge_with_stale_frame() {
    use crate::ecs::{Position, ReshuffleAroundMarker};

    let config: Config = (
        MainOptions {
            auto_center: Some(false),
            animation_speed: Some(30.0),
            continuous_swipe: Some(false),
            ..Default::default()
        },
        vec![],
    )
        .into();

    // 5 windows @ 400px = 2000px strip on a 1024px display → scrollable.
    let mut h = TestHarness::new().with_config(config).with_windows(5);

    let pump = |h: &mut TestHarness, c: Command| {
        h.app
            .world_mut()
            .write_message::<Event>(Event::Command { command: c });
        for _ in 0..10 {
            h.app.update();
            for e in h.mock_state.drain_events() {
                h.app.world_mut().write_message::<Event>(e);
            }
        }
    };

    // Boot the strip; column 0 (window id 0) sits at layout x 0.
    pump(&mut h, Command::PrintState);

    let leftmost = find_window_entity(0, h.app.world_mut());

    // Simulate a stale Space-transition frame: the leftmost window's on-screen
    // frame is parked at the right-edge sliver while its layout position is
    // still 0. Clear any in-flight animation so moving_frame reads the origin.
    {
        let world = h.app.world_mut();
        if let Ok(mut e) = world.get_entity_mut(leftmost) {
            e.insert(Position(Origin::new(
                TEST_DISPLAY_WIDTH - 5,
                TEST_MENUBAR_HEIGHT,
            )));
            e.remove::<RepositionMarker>();
            // Trigger a reshuffle around the leftmost window, as focus would.
            e.insert(ReshuffleAroundMarker);
        }
    }

    for _ in 0..15 {
        h.app.update();
        for e in h.mock_state.drain_events() {
            h.app.world_mut().write_message::<Event>(e);
        }
    }

    // The strip must be pinned to the left edge (offset 0): column 0 has
    // layout x 0, so its on-screen left edge lands at the display's left edge.
    let world = h.app.world_mut();
    let mut q = world.query_filtered::<&Position, With<ActiveWorkspaceMarker>>();
    let strip_x = q.single(world).expect("exactly one active strip").0.x;
    assert_eq!(
        strip_x, 0,
        "reshuffle around leftmost window must pin strip to left edge (offset 0), got {strip_x}"
    );
}

/// Stacking or unstacking the focused window must bring it fully back into
/// view. Regression: `stack_windows_handler` mutated the strip but never
/// reshuffled, so when the strip was scrolled such that the focused window's
/// new column slot fell off-screen, the window stayed partially or fully
/// invisible even though it kept focus. It now reshuffles around the focused
/// window; the edge-clamp in `reshuffle_layout_strip` keeps the strip pinned to
/// the edges.
#[test]
fn test_stack_unstack_brings_focused_window_into_view() {
    fn check_if_offscreen(world: &mut World, _state: MockState) {
        let mut q = world.query_filtered::<(&Window, &Position), With<crate::ecs::FocusedMarker>>();
        let (_, position) = q.single(world).expect("a focused window");

        assert!(
            position.x < -(TEST_WINDOW_WIDTH / 4),
            "focused window should be somewhat offscreen after the scroll."
        );
    }

    let config: Config = (
        MainOptions {
            focus_follows_mouse: Some(false),
            auto_center: Some(false),
            animation_speed: Some(10000.0),
            swipe_gesture_fingers: Some(3),
            ..Default::default()
        },
        vec![],
    )
        .into();

    // 5 windows @ 400px = 2000px strip on a 1024px display → scrollable.
    let harness = TestHarness::new().with_config(config).with_windows(5);

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        // Swipe windows 0 and 1 off screen.
        Event::Swipe {
            delta: 0.3,
            fingers: 3,
        },
        // Noop to let the scroll settle.
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Stack(true)),
        },
        // Now swipe the stacked windows off screen again.
        Event::Swipe {
            delta: 0.1,
            fingers: 3,
        },
        // Noop to let the scroll settle.
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Stack(false)),
        },
    ];

    harness
        .on_iteration(3, check_if_offscreen)
        .on_iteration(4, |world, _state| {
            // Check that both window are stacked and moved into view.
            assert_window_at!(world, 0, 0, 20);
            assert_window_at!(world, 1, 0, 394);
        })
        .on_iteration(5, check_if_offscreen)
        .on_iteration(7, |world, _state| {
            // Check that both window are stacked and moved into view.
            assert_window_at!(world, 1, 0, 20);
        })
        .run(commands);
}

/// A window parked on a hidden virtual row must stay parked when its app
/// hides and re-shows itself (e.g. 1Password self-activating periodically),
/// which runs the whole hide/show cycle unprompted. Regression: the restore
/// path used to reshuffle around the window's popped frame, dragging
/// the hidden strip back on screen and making the window unreachable to
/// commands that only act on the active strip.
/// A `WindowMoved` notification for a window spool is not currently moving is
/// the app (or the user) moving it, and the layout must take that new origin on
/// board.
#[test]
fn test_foreign_window_move_is_adopted() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    let config: Config = (
        MainOptions {
            // Snappy, so no `RepositionMarker` is still in flight when the
            // notification below arrives.
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.os_move_window(0, Origin::new(77, 88));
        })
        .on_iteration(2, |world, _state| {
            let entity = find_window_entity(0, world);
            let position = world.get::<Position>(entity).expect("window position");
            assert_eq!(
                position.0,
                Origin::new(77, 88),
                "a move spool did not make must be read back into the layout"
            );
        })
        .run(commands);
}

/// A `WindowMoved` echo of a move spool itself just made must not perturb the
/// in-flight animation — reading it back naively made the animation and the
/// echo chase each other, causing jitter on every reflow.
///
/// Driven directly at the system instead of through the harness loop: the mock
/// applies its reposition synchronously, so a normal frame would resolve the
/// move before the notification could ever be read back.
#[test]
fn test_own_window_move_echo_is_ignored() {
    use bevy::ecs::system::RunSystemOnce as _;

    let mut harness = TestHarness::new().with_windows(2);
    harness.app.update();

    let state = harness.mock_state.clone();
    let world = harness.world();
    let entity = find_window_entity(0, world);
    let before = world.get::<Position>(entity).expect("window position").0;

    // A move of ours is in flight, and the app reports a frame we didn't ask
    // for. Displaced on the axis the animation leaves alone, so the assertion
    // can't be confused by how far the lerp has run.
    world
        .entity_mut(entity)
        .insert(RepositionMarker(Origin::new(5000, before.y)));
    state.os_move_window(0, Origin::new(before.x, before.y + 888));
    world.write_message(Event::WindowMoved { window_id: 0 });

    world
        .run_system_once(crate::ecs::systems::window_moved_update_frame)
        .expect("running window_moved_update_frame");

    assert_eq!(
        world.get::<Position>(entity).expect("window position").0,
        before,
        "the echo of our own move must not be read back over the animation"
    );
}

#[test]
fn targeted_window_focus_uses_window_id() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(1, |world, _state| {
            assert_focused!(world, 1);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: Command::FocusWindow { window_id: 1 },
            },
        ]);
}

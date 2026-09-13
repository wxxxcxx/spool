use crate::commands::{Action, Direction, Operation, ResizeAxis, ResizeDirection};
use crate::config::{Config, MainOptions, WindowParams};
use crate::ecs::Floating;
use crate::ecs::layout::LayoutStrip;
use crate::events::Event;
use crate::{assert_window_at, assert_window_size};
use bevy::prelude::*;

use super::*;

// Inherited first preset is one quarter of the available viewport.
const DEFAULT_TILED_WIDTH: i32 = TEST_DISPLAY_WIDTH / 4;

#[cfg(feature = "lua")]
#[test]
fn edge_padding_reload_repositions_and_resizes_existing_windows() {
    let config_with_padding = |padding| {
        let lua = mlua::Lua::new();
        let value = lua
            .load(format!(
                "return {{ options = {{ auto_center = false }}, \
                 swipe = {{ continuous = false }}, \
                 padding = {{ top = {padding}, bottom = {padding}, \
                 left = {padding}, right = {padding} }} }}"
            ))
            .eval()
            .unwrap();
        crate::config::config_from_lua(&lua, value).unwrap()
    };
    let mut harness = TestHarness::new()
        .with_config(config_with_padding(8))
        .with_windows(3);
    harness.pump_frames(20);
    assert_window_at!(harness.world(), 0, 8, TEST_MENUBAR_HEIGHT + 8);
    assert_window_size!(
        harness.world(),
        0,
        (TEST_DISPLAY_WIDTH - 16) / 4,
        TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - 16
    );

    {
        let mut config = harness.world().resource_mut::<Config>();
        config.replace_inner_from(&config_with_padding(4));
        config.set_changed();
    }
    harness.pump_frames(20);
    assert_window_at!(harness.world(), 0, 4, TEST_MENUBAR_HEIGHT + 4);
    assert_window_size!(
        harness.world(),
        0,
        (TEST_DISPLAY_WIDTH - 8) / 4,
        TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT - 8
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_window_shuffle() {
    const PADDING_LEFT: u16 = 3;
    const PADDING_RIGHT: u16 = 5;
    const PADDING_TOP: u16 = 7;
    const PADDING_BOTTOM: u16 = 9;
    const SLIVER_WIDTH: u16 = 5;

    let commands = vec![
        Event::MenuOpened { window_id: 0 }, // 0
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        }, // 2
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
        }, // 3
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        }, // 4
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
        }, // 5
        Event::ActionRequested {
            action: Action::Window(Operation::Center),
        }, // 6
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        }, // 7
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
        }, // 8
        Event::ActionRequested {
            action: Action::Window(Operation::Center),
        }, // 9
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 10
    ];

    // Logical width includes padding expansion on each side.
    let logical_width = TEST_WINDOW_WIDTH;
    let top_edge = TEST_MENUBAR_HEIGHT + i32::from(PADDING_TOP);
    let left_edge = i32::from(PADDING_LEFT);
    let right_edge = TEST_DISPLAY_WIDTH - i32::from(PADDING_RIGHT);
    let offscreen_right = right_edge - i32::from(SLIVER_WIDTH) + i32::from(PADDING_RIGHT);
    let offscreen_left =
        left_edge - logical_width + i32::from(SLIVER_WIDTH) - i32::from(PADDING_LEFT);
    let centered = (TEST_DISPLAY_WIDTH - logical_width) / 2;

    let mut params = WindowParams::new(".*", None);
    params.vertical_padding = Some(3);
    params.horizontal_padding = Some(2);
    // This clipping/stacking scenario deliberately uses 400-point columns.
    params.width = Some(
        f64::from(logical_width)
            / f64::from(TEST_DISPLAY_WIDTH - i32::from(PADDING_LEFT + PADDING_RIGHT)),
    );
    let config: Config = (
        MainOptions {
            padding_left: Some(PADDING_LEFT),
            padding_right: Some(PADDING_RIGHT),
            padding_top: Some(PADDING_TOP),
            padding_bottom: Some(PADDING_BOTTOM),
            ..Default::default()
        },
        vec![params],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(5)
        .on_iteration(2, move |world, _state| {
            assert_window_at!(world, 0, offscreen_left, top_edge);
            assert_window_at!(world, 1, offscreen_left, top_edge);
            assert_window_at!(world, 2, right_edge - 3 * logical_width, top_edge);
            assert_window_at!(world, 3, right_edge - 2 * logical_width, top_edge);
            assert_window_at!(world, 4, right_edge - logical_width, top_edge);
        })
        .on_iteration(3, move |world, _state| {
            assert_window_at!(world, 0, left_edge, top_edge);
            assert_window_at!(world, 1, left_edge + logical_width, top_edge);
            assert_window_at!(world, 2, left_edge + 2 * logical_width, top_edge);
            assert_window_at!(world, 3, offscreen_right, top_edge);
            assert_window_at!(world, 4, offscreen_right, top_edge);
        })
        .on_iteration(6, move |world, _state| {
            assert_window_at!(world, 0, centered, top_edge);
            assert_window_at!(world, 1, centered, 393);
            assert_window_at!(world, 2, centered + logical_width, top_edge);
            assert_window_at!(world, 3, offscreen_right, top_edge);
            assert_window_at!(world, 4, offscreen_right, top_edge);
        })
        .on_iteration(10, move |world, _state| {
            assert_window_at!(world, 0, centered, top_edge);
            assert_window_at!(world, 1, centered, 271);
            assert_window_at!(world, 2, centered, 515);
            assert_window_at!(world, 3, centered + logical_width, top_edge);
            assert_window_at!(world, 4, offscreen_right, top_edge);
        })
        .run(commands);
}

#[test]
fn test_window_balance() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Balance),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(1, |world, _state| {
            // After grow, window 0 should be 341 (one third of 1024).
            assert_window_size!(world, 0, 341, 748);
        })
        .on_iteration(2, |world, _state| {
            // After balance, all windows should match window 0's width.
            assert_window_size!(world, 0, 341, 748);
            assert_window_size!(world, 1, 341, 748);
            assert_window_size!(world, 2, 341, 748);
        })
        .run(commands);
}

#[test]
fn test_startup_windows() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(4, |world, _state| {
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 1, DEFAULT_TILED_WIDTH, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 2, 2 * DEFAULT_TILED_WIDTH, TEST_MENUBAR_HEIGHT);
        })
        .run(commands);
}

#[test]
fn test_window_resize_grow_and_shrink_cycle() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Shrink,
            }),
        },
    ];

    let config: Config = (
        MainOptions {
            preset_column_widths: vec![0.25, 0.5, 0.75],
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .on_iteration(1, |world, _state| {
            assert_window_size!(world, 0, 512, 748);
        })
        .on_iteration(2, |world, _state| {
            assert_window_size!(world, 0, 768, 748);
        })
        .on_iteration(3, |world, _state| {
            assert_window_size!(world, 0, 256, 748);
        })
        .on_iteration(4, |world, _state| {
            assert_window_size!(world, 0, 768, 748);
        })
        .run(commands);
}

#[test]
fn test_window_can_resize_to_two_display_widths_and_scroll() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::SetWidth(2.0)),
        },
        Event::Swipe {
            delta: 0.3,
            fingers: 3,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Snap),
        },
    ];

    let config: Config = (
        MainOptions {
            swipe_gesture_fingers: Some(3),
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .on_iteration(1, |world, _state| {
            assert_window_size!(world, 0, 2048, 748);
            assert_oversized_window_is_pannable(world, 0);
        })
        .on_iteration(2, |world, _state| {
            assert_window_size!(world, 0, 2048, 748);
            assert_oversized_window_is_pannable(world, 0);
        })
        .on_iteration(3, |world, _state| {
            assert_window_size!(world, 0, 2048, 748);
            assert_oversized_window_is_pannable(world, 0);
        })
        .run(commands);
}

fn assert_oversized_window_is_pannable(world: &mut World, id: i32) {
    let mut query = world.query::<&crate::manager::Window>();
    let window = query
        .iter(world)
        .find(|window| window.id() == id)
        .expect("window not found");
    let x = window.frame().min.x;
    assert!(
        (-TEST_DISPLAY_WIDTH..=0).contains(&x),
        "oversized window must stay within its pannable range, got x={x}"
    );
}

/// A floating window is out of the tiling layout, so it must not keep a slot in
/// the strip: the tiler lays columns out left to right by accumulated width, so
/// a floating member reserves space no tiled window occupies — a gap.
#[test]
fn test_floating_window_does_not_hold_a_slot_in_the_strip() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 0
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleFloating),
        }, // 1 — float the focused window
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 2
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            // Window 0 holds the focus, so it is the one about to float.
            assert_eq!(window_x(world, 0), 0);
            assert_eq!(window_x(world, 1), DEFAULT_TILED_WIDTH);
            assert_eq!(window_x(world, 2), 2 * DEFAULT_TILED_WIDTH);
        })
        .on_iteration(2, |world, _state| {
            let entity = find_window_entity(0, world);
            let mut query = world.query::<&LayoutStrip>();
            assert!(
                !query.iter(world).any(|strip| strip.contains(entity)),
                "a floating window must not stay in any layout strip"
            );
            // The slot it vacated has to close up: window 1 slides to the left
            // edge rather than leaving an empty column where window 0 was.
            assert_eq!(
                window_x(world, 1),
                0,
                "the tiled windows must close the gap the floating one left"
            );
            assert_eq!(window_x(world, 2), DEFAULT_TILED_WIDTH);
        })
        .run(commands);
}

#[test]
fn shared_move_resize_and_maximize_actions_dispatch_for_floating_windows() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleFloating),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Move(Direction::East)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Resize {
                axis: ResizeAxis::Height,
                direction: ResizeDirection::Shrink,
            }),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Maximize),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Maximize),
        },
    ];

    let config: Config = (
        MainOptions {
            animation_speed: Some(1_000_000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .on_iteration(2, |world, _state| {
            assert_window_at!(world, 0, 52, 52);
        })
        .on_iteration(3, |world, _state| {
            assert_window_size!(world, 0, DEFAULT_TILED_WIDTH + 40, 598);
        })
        .on_iteration(4, |world, _state| {
            assert_window_size!(world, 0, DEFAULT_TILED_WIDTH + 40, 558);
        })
        .on_iteration(5, |world, _state| {
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
            assert_window_size!(world, 0, TEST_DISPLAY_WIDTH, 748);
        })
        .on_iteration(6, |world, _state| {
            assert_window_at!(world, 0, 32, 72);
            assert_window_size!(world, 0, DEFAULT_TILED_WIDTH + 40, 558);
        })
        .run(commands);
}

#[test]
fn toggle_stack_uses_the_current_layout_state() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
        },
    ];

    TestHarness::new()
        .with_windows(2)
        .on_iteration(2, |world, _state| {
            let mut strips = world.query::<&LayoutStrip>();
            let strip = strips.single(world).expect("one layout strip");
            assert_eq!(strip.len(), 1, "the focused column should stack left");
        })
        .on_iteration(3, |world, _state| {
            let mut strips = world.query::<&LayoutStrip>();
            let strip = strips.single(world).expect("one layout strip");
            assert_eq!(strip.len(), 2, "the focused item should unstack again");
        })
        .run(commands);
}

/// The same invariant for a window floated by a config rule rather than by the
/// toggle. This is the path that runs while the window is being spawned, so it
/// races the strip insertion the tiling path is doing at the same time.
#[test]
fn test_rule_floated_window_does_not_hold_a_slot_in_the_strip() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    // Only window 1 floats; 0 and 2 tile as usual.
    let mut params = WindowParams::new("^Window 1$", None);
    params.floating = Some(true);
    let config: Config = (MainOptions::default(), vec![params]).into();

    TestHarness::new()
        .with_config(config)
        .with_windows(3)
        .on_iteration(1, |world, _state| {
            let entity = find_window_entity(1, world);
            let mut query = world.query::<&LayoutStrip>();
            assert!(
                !query.iter(world).any(|strip| strip.contains(entity)),
                "a rule-floated window must not stay in any layout strip"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                DEFAULT_TILED_WIDTH,
                "the tiled windows must close the gap the floating one left"
            );
        })
        .run(commands);
}

#[test]
fn fixed_size_window_floats_instead_of_disturbing_the_tiled_strip() {
    let harness = TestHarness::new().with_windows(2);
    harness
        .mock_state
        .update_window(0, |window| window.resizable = false);

    harness
        .on_iteration(1, |world, state| {
            let fixed = find_window_entity(0, world);
            assert!(
                world.get::<Floating>(fixed).is_some(),
                "a window whose AXSize attribute is not settable cannot be a tiled layout target"
            );
            let mut strips = world.query::<&LayoutStrip>();
            assert!(
                strips.iter(world).all(|strip| !strip.contains(fixed)),
                "a fixed-size floating window must not reserve a layout column"
            );
            assert_eq!(
                state.frame_write_attempts(0),
                0,
                "Spool must not issue impossible frame writes to a fixed-size window"
            );
        })
        .run(vec![
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::ActionRequested {
                action: Action::PrintState,
            },
        ]);
}

#[test]
fn closed_retained_surface_releases_its_tile_slot() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.pump_frames(20);
    let before = window_x(harness.world(), 2) - window_x(harness.world(), 0);
    assert_eq!(before, 2 * DEFAULT_TILED_WIDTH);
    harness.mock_state.update_window(1, |window| {
        window.visible = false;
        window.ordered_out = true;
    });
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(20);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        DEFAULT_TILED_WIDTH,
        "a closed retained surface must not leave an empty tile slot"
    );
    let retained = find_window_entity(1, harness.world());
    assert!(
        harness
            .world()
            .query::<&LayoutStrip>()
            .iter(harness.world())
            .any(|strip| strip.contains(retained)),
        "retained identity is restoration data, not an occupied slot"
    );
    let writes = harness.mock_state.frame_write_attempts(1);
    harness.pump_frames(20);
    assert_eq!(harness.mock_state.frame_write_attempts(1), writes);
    harness.mock_state.os_restore_withdrawn_window(1);
    harness.mock_state.update_window(1, |window| {
        window.visible = true;
        window.ordered_out = false;
    });
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(20);
    assert_eq!(find_window_entity(1, harness.world()), retained);
    assert_eq!(
        window_x(harness.world(), 1) - window_x(harness.world(), 0),
        DEFAULT_TILED_WIDTH
    );
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        2 * DEFAULT_TILED_WIDTH
    );
}

#[test]
fn retained_surface_slot_updates_after_delayed_order_out() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.pump_frames(20);
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(5);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        2 * DEFAULT_TILED_WIDTH
    );
    harness.mock_state.os_order_out_withdrawn_surface(1);
    // The heartbeat must upgrade an existing suspension even without a close event.
    harness.pump_frames(30);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        DEFAULT_TILED_WIDTH
    );
}

#[test]
fn failed_presentation_observation_preserves_tile_until_recovery() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.pump_frames(20);
    harness
        .mock_state
        .set_presentation_inventory_available(false);
    harness.mock_state.update_window(1, |window| {
        window.visible = false;
        window.ordered_out = true;
    });
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(20);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        2 * DEFAULT_TILED_WIDTH
    );
    harness
        .mock_state
        .set_presentation_inventory_available(true);
    harness.pump_frames(30);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        DEFAULT_TILED_WIDTH
    );
}

/// Closing a window while its application stays alive must free its slot in
/// the strip. The AX element of such a window often keeps answering queries
/// after the window is gone, which used to make `window_destroyed_trigger`
/// mistake the destroy event for a space change and leave a gap where the
/// window had been.
#[test]
fn test_closing_window_of_live_app_closes_the_gap() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 0
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 1
        Event::ActionRequested {
            action: Action::PrintState,
        }, // 2
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, state| {
            let left = window_x(world, 0);
            assert_eq!(
                window_x(world, 2) - left,
                2 * DEFAULT_TILED_WIDTH,
                "three windows should tile side by side before the close"
            );
            state.os_close_window(1);
        })
        .on_iteration(2, |world, _state| {
            assert!(
                !window_exists(world, 1),
                "closed window must be dropped from the world"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                DEFAULT_TILED_WIDTH,
                "surviving windows must close the gap left by window 1"
            );
        })
        .run(commands);
}

#[test]
fn test_window_server_close_of_live_app_closes_the_gap() {
    use crate::events::DestroySource;

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::WindowDestroyed {
            window_id: 1,
            source: DestroySource::WindowServer,
            incarnation: None,
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.os_vanish_window(1))
        .on_iteration(1, |world, _state| {
            assert!(
                !window_exists(world, 1),
                "WindowServer close must remove the tracked window"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                DEFAULT_TILED_WIDTH,
                "surviving windows must close the WindowServer-confirmed gap"
            );
        })
        .run(commands);
}

/// Reconciliation is the recovery path for applications which withdraw a
/// window without emitting either `AXUIElementDestroyed` or an SLS destroy event.
#[test]
fn test_reconcile_removes_a_vanished_window_and_closes_the_gap() {
    use crate::events::ReconcileScope;

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ReconcileWindows {
            scope: ReconcileScope::Application(TEST_PROCESS_ID),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, state| {
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                2 * DEFAULT_TILED_WIDTH
            );
            state.os_vanish_window(1);
        })
        .on_iteration(1, |world, _state| {
            assert!(
                !window_exists(world, 1),
                "reconciliation must remove a window missing from the OS inventory"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                DEFAULT_TILED_WIDTH,
                "surviving windows must close the reconciled gap"
            );
        })
        .run(commands);
}

/// A cached AX element can keep answering after its window has disappeared
/// from both the application's inventory and `WindowServer`. That stale handle
/// must not keep a navigable layout slot alive indefinitely.
#[test]
fn test_reconcile_removes_vanished_window_with_responsive_stale_ax_handle() {
    use crate::events::ReconcileScope;

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ReconcileWindows {
            scope: ReconcileScope::Application(TEST_PROCESS_ID),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, state| {
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                2 * DEFAULT_TILED_WIDTH
            );
            state.os_vanish_window_with_stale_ax_handle(1);
        })
        .on_iteration(1, |world, _state| {
            assert!(
                !window_exists(world, 1),
                "a responsive stale AX handle must not keep a vanished window alive"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                DEFAULT_TILED_WIDTH,
                "surviving windows must close the vanished window's gap"
            );
        })
        .run(commands);
}

#[test]
fn test_reconcile_suspends_ax_withdrawn_window_without_reflowing_live_surface() {
    use crate::events::ReconcileScope;

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ReconcileWindows {
            scope: ReconcileScope::Application(TEST_PROCESS_ID),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.os_withdraw_window(1))
        .on_iteration(1, |world, _state| {
            assert!(
                window_exists(world, 1),
                "CG-retained surface must not be destroyed immediately"
            );
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                2 * DEFAULT_TILED_WIDTH,
                "a live WindowServer surface must retain its declarative layout slot"
            );
        })
        .run(commands);
}

#[test]
fn reconciling_a_withdrawn_focused_window_invalidates_confirmed_focus() {
    use crate::ecs::focus::FocusCoordinator;
    use crate::events::ReconcileScope;

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.take_focus_requests();
            state.os_withdraw_window(0);
        })
        .on_iteration(1, |world, state| {
            assert!(
                world
                    .resource::<FocusCoordinator>()
                    .snapshot()
                    .confirmed_entity()
                    .is_none(),
                "an unavailable window cannot remain confirmed focus"
            );
            assert!(state.take_focus_requests().is_empty());
        })
        .run(vec![
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::ReconcileWindows {
                scope: ReconcileScope::Application(TEST_PROCESS_ID),
            },
        ]);
}

#[test]
fn test_reconcile_restores_a_temporarily_withdrawn_window() {
    use crate::events::ReconcileScope;

    let reconcile = || Event::ReconcileWindows {
        scope: ReconcileScope::Application(TEST_PROCESS_ID),
    };
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        reconcile(),
        reconcile(),
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.os_withdraw_window(1))
        .on_iteration(1, |world, state| {
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                2 * DEFAULT_TILED_WIDTH
            );
            state.os_restore_withdrawn_window(1);
        })
        .on_iteration(2, |world, _state| {
            assert!(window_exists(world, 1));
            assert_eq!(
                window_x(world, 2) - window_x(world, 0),
                2 * DEFAULT_TILED_WIDTH
            );
        })
        .run(commands);
}

#[test]
fn test_reconcile_destroys_withdrawn_window_after_cg_confirmation() {
    use crate::events::ReconcileScope;

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ReconcileWindows {
            scope: ReconcileScope::Application(TEST_PROCESS_ID),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.os_withdraw_window(1))
        .on_iteration(1, |world, state| {
            assert!(window_exists(world, 1));
            state.os_settle_withdrawn_surface(1);
        })
        .on_iteration(2, |world, _state| {
            assert!(
                !window_exists(world, 1),
                "window must be destroyed after AX and CG agree"
            );
        })
        .run(commands);
}

#[test]
fn test_reconcile_discovers_window_missing_from_ecs() {
    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::ReconcileWindows,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |_world, state| {
            let frame = bevy::math::IRect::new(0, 0, DEFAULT_TILED_WIDTH, TEST_WINDOW_HEIGHT);
            _ = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 99, frame);
        })
        .on_iteration(1, |world, _state| {
            assert!(
                window_exists(world, 99),
                "full reconciliation must discover an OS-only window"
            );
        })
        .run(commands);
}

fn window_x(world: &mut World, id: i32) -> i32 {
    let mut query = world.query::<&crate::manager::Window>();
    query
        .iter(world)
        .find(|window| window.id() == id)
        .unwrap_or_else(|| panic!("window {id} not found"))
        .frame()
        .min
        .x
}

fn window_exists(world: &mut World, id: i32) -> bool {
    let mut query = world.query::<&crate::manager::Window>();
    query.iter(world).any(|window| window.id() == id)
}

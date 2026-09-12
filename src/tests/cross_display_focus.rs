//! Focus stability when the user works across physical displays.
//!
//! macOS moves the menu bar (the "active display") to whatever display owns the
//! key window, but `AppKit` posts no notification for it, so Spool has to notice
//! from its own observation of `SLSCopyActiveMenuBarDisplayIdentifier`.

use bevy::prelude::*;

use crate::ecs::FocusedMarker;
use crate::ecs::layout::LayoutStrip;
use crate::manager::Window;
use crate::platform::WinID;
use crate::tests::*;

/// Window 0 lives on the active display, window 2 on the second one.
fn two_display_harness() -> TestHarness {
    let mut harness = TestHarness::new()
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
        .with_windows(1)
        .with_workspace_window(2, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(20);
    harness.mock_state.take_focus_requests();
    harness
}

fn focused_id(world: &mut World) -> Option<WinID> {
    let mut query = world.query_filtered::<&Window, With<FocusedMarker>>();
    query.iter(world).next().map(|window| window.id())
}

fn active_workspace_ids(world: &mut World) -> Vec<WorkspaceId> {
    let mut query = world.query_filtered::<&LayoutStrip, With<crate::ecs::ActiveWorkspaceMarker>>();
    query.iter(world).map(LayoutStrip::id).collect()
}

#[test]
fn clicking_a_window_on_another_display_keeps_focus_there() {
    let mut harness = two_display_harness();
    assert_eq!(focused_id(harness.world()), Some(0));
    assert_eq!(
        active_workspace_ids(harness.world()),
        vec![TEST_WORKSPACE_ID]
    );

    // The user clicks window 2 on the second display. macOS makes that display
    // the menu bar display and hands it the key window; no AppKit notification
    // announces the active-display change.
    harness.mock_state.set_active_display(EXT_DISPLAY_ID);
    harness.mock_state.focus_window(2);
    harness.pump_frames(15);

    assert_eq!(
        focused_id(harness.world()),
        Some(2),
        "focus must stay on the window the user clicked"
    );
    assert_eq!(
        active_workspace_ids(harness.world()),
        vec![EXT_WORKSPACE_ID],
        "the active Space projection must follow the menu bar display"
    );
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        Vec::<WinID>::new(),
        "Spool must not push focus back to the display the user left"
    );

    // Clicking back must follow the same rule in the other direction.
    harness.mock_state.set_active_display(TEST_DISPLAY_ID);
    harness.mock_state.focus_window(0);
    harness.pump_frames(15);

    assert_eq!(focused_id(harness.world()), Some(0));
    assert_eq!(
        active_workspace_ids(harness.world()),
        vec![TEST_WORKSPACE_ID]
    );
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        Vec::<WinID>::new()
    );
}

#[test]
fn confirmed_focus_on_another_display_survives_the_topology_heartbeat() {
    let mut harness = two_display_harness();

    // Only the confirmed-focus half of the click: macOS already reports window 2
    // as the key window, but the active-display observation has not caught up.
    harness.mock_state.set_active_display(EXT_DISPLAY_ID);
    harness.mock_state.focus_window(2);
    harness.pump_frames(3);
    assert_eq!(focused_id(harness.world()), Some(2));
    harness.mock_state.take_focus_requests();

    // Let the topology heartbeat run at least once.
    harness.pump_frames(15);

    assert_eq!(
        focused_id(harness.world()),
        Some(2),
        "a topology heartbeat must not undo a confirmed cross-display focus"
    );
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        Vec::<WinID>::new()
    );
}

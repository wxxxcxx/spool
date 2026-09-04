use bevy::prelude::*;
use objc2_core_foundation::CGPoint;

use crate::commands::{Action, Direction, MoveFocus, Operation};
use crate::config::{Config, MainOptions, WindowParams, parse_action};
use crate::ecs::display::FloatingLayer;
use crate::ecs::native_space::VisibleNativeSpaceMarker;
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveWorkspaceMarker, Floating, FocusedMarker, NativeFullscreenMarker, Position,
    PreviousTiledStrip, WindowVisibility, layout::LayoutStrip,
};
use crate::ecs::{RepositionMarker, SpawnWindowTrigger};
use crate::events::{DestroySource, Event, FocusSource, ReconcileScope};
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
        .on_iteration(1, |world, state| {
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

            state.update_window(0, |window| {
                window.workspace_id = TEST_WORKSPACE_ID;
                window.is_full_screen = false;
            });
            state.activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
            state.destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
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
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::SpaceChanged,
            Event::SpaceDestroyed {
                space_id: FULLSCREEN_WORKSPACE_ID,
            },
        ]);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the regression follows the complete startup fullscreen to windowed lifecycle"
)]
fn startup_fullscreen_window_reenters_layout_strip_when_fullscreen_space_is_destroyed() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .with_workspace_window(1, TEST_WORKSPACE_ID, |_| {});
    let native_fullscreen_frame = harness
        .mock_state
        .actual_window_frame(0)
        .expect("native fullscreen frame before Spool layout");
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    let mut strips = harness.world().query::<&LayoutStrip>();
    let fullscreen_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
        .expect("startup fullscreen strip");
    assert!(fullscreen_strip.contains(fullscreen_window));
    assert!(harness.world().get::<Floating>(fullscreen_window).is_none());
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(fullscreen_window)
            .is_some(),
        "fullscreen physical geometry must remain deferred layout input"
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(native_fullscreen_frame),
        "normal layout and animation commits must yield while macOS owns native fullscreen"
    );

    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(5);

    let fullscreen_window = find_window_entity(0, harness.world());
    let sibling_window = find_window_entity(1, harness.world());
    let mut strips = harness.world().query::<&LayoutStrip>();
    let normal_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .expect("normal strip");
    assert!(
        normal_strip.contains(fullscreen_window),
        "window that leaves startup fullscreen must reenter a layout strip"
    );
    assert!(normal_strip.contains(sibling_window));
    assert!(
        harness
            .world()
            .get::<crate::ecs::FullscreenDefaultsDeferred>(fullscreen_window)
            .is_none(),
        "leaving fullscreen must release the deferred defaults transaction"
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowDefaultsPending>(fullscreen_window)
            .is_none(),
        "windowed geometry must be incorporated before the transaction completes"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1,
        "a restored window must belong to exactly one layout strip"
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID],
        "fullscreen exit must leave exactly one usable active strip"
    );
}

#[test]
fn startup_fullscreen_exit_recovers_when_space_destroyed_notification_is_missing() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(fullscreen_window)),
        "topology reconciliation must recover when SpaceDestroyed is lost"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
    let mut visible_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<VisibleNativeSpaceMarker>>();
    assert_eq!(
        visible_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
}

#[test]
fn startup_fullscreen_exit_recovers_without_any_space_notification() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);

    // The harness advances virtual time by 100 ms per frame. No Space event is
    // emitted; only the periodic topology audit can observe this transition.
    harness.pump_frames(12);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(fullscreen_window)),
        "the topology heartbeat must recover a completely silent fullscreen exit"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
    let mut visible_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<VisibleNativeSpaceMarker>>();
    assert_eq!(
        visible_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
}

#[test]
fn startup_fullscreen_exit_is_order_independent_when_space_events_share_a_frame() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    for destroyed_first in [false, true] {
        let mut harness = TestHarness::new()
            .with_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
            )
            .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
                window.is_full_screen = true;
            });
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
        harness.pump_frames(10);

        let fullscreen_window = find_window_entity(0, harness.world());
        harness.mock_state.update_window(0, |window| {
            window.workspace_id = TEST_WORKSPACE_ID;
            window.is_full_screen = false;
        });
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
        harness
            .mock_state
            .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
        let destroyed = Event::SpaceDestroyed {
            space_id: FULLSCREEN_WORKSPACE_ID,
        };
        if destroyed_first {
            harness.world().write_message(destroyed);
            harness.world().write_message(Event::SpaceChanged);
        } else {
            harness.world().write_message(Event::SpaceChanged);
            harness.world().write_message(destroyed);
        }
        harness.pump_frames(1);

        let mut strips = harness.world().query::<&LayoutStrip>();
        let normal_strip = strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .expect("normal strip");
        assert!(normal_strip.contains(fullscreen_window));
        assert_eq!(
            strips
                .iter(harness.world())
                .filter(|strip| strip.contains(fullscreen_window))
                .count(),
            1,
            "message order must not create a transient orphan"
        );
        assert!(
            strips
                .iter(harness.world())
                .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
        );
    }
}

#[test]
fn repeated_space_destruction_preserves_pending_active_state() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    // Each destruction frame reads the active Space once during topology
    // reconciliation and once while reconciling pending membership.
    harness.mock_state.script_active_space_queries(
        TEST_DISPLAY_ID,
        [
            Ok(TEST_WORKSPACE_ID),
            Err(()),
            Ok(TEST_WORKSPACE_ID),
            Ok(TEST_WORKSPACE_ID),
        ],
    );
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID),
        "an active-state read failure must keep the pending source recoverable"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1,
        "active-state uncertainty must not orphan or duplicate the window"
    );

    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(fullscreen_window))
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID],
        "a repeated invalidation must not erase the original active-state obligation"
    );
}

#[test]
fn startup_fullscreen_exit_waits_for_live_space_membership_before_destroying_strip() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .with_workspace_window(1, TEST_WORKSPACE_ID, |_| {});
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let pending_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
        .expect("unsettled fullscreen strip must remain recoverable");
    assert!(pending_strip.contains(fullscreen_window));
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );

    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness.pump_frames(2);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let normal_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .expect("normal strip");
    assert!(normal_strip.contains(fullscreen_window));
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID]
    );
}

#[test]
fn startup_fullscreen_exit_retries_after_membership_query_failure() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const SECOND_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![FULLSCREEN_WORKSPACE_ID, SECOND_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = SECOND_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, SECOND_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness
        .mock_state
        .script_workspace_membership_queries(SECOND_WORKSPACE_ID, [Err(()), Err(()), Ok(vec![0])]);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID
                && strip.contains(fullscreen_window)),
        "a failed membership read must preserve the recoverable source strip"
    );

    harness.pump_frames(8);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let normal_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == SECOND_WORKSPACE_ID)
        .expect("normal strip");
    assert!(normal_strip.contains(fullscreen_window));
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID],
        "retrying an inactive display must not steal global activation"
    );
}

#[test]
fn destroyed_space_tombstone_waits_for_source_display_topology_after_transient_omission() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const SECOND_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![FULLSCREEN_WORKSPACE_ID, SECOND_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = SECOND_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, SECOND_WORKSPACE_ID, false);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut pending = harness
        .world()
        .query::<(&LayoutStrip, Has<PendingSpaceDestruction>)>();
    assert!(pending.iter(harness.world()).any(|(strip, pending)| {
        strip.id() == FULLSCREEN_WORKSPACE_ID && pending && strip.all_windows().is_empty()
    }));
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == SECOND_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(fullscreen_window))
    );

    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness
        .mock_state
        .script_present_display_topology_queries(SECOND_DISPLAY_ID, [Err(())]);

    // The first frame consumes the reconciliation backoff. On the second, the
    // source display's one failed topology query must not look like proof that
    // its Space disappeared.
    harness.pump_frames(2);
    let mut pending = harness
        .world()
        .query::<(&LayoutStrip, Has<PendingSpaceDestruction>)>();
    assert!(
        pending.iter(harness.world()).any(|(strip, pending)| {
            strip.id() == FULLSCREEN_WORKSPACE_ID && pending && strip.all_windows().is_empty()
        }),
        "a missing source-display observation must preserve the tombstone"
    );

    // Once the next successful observation confirms that the source display no
    // longer contains the Space, the empty tombstone can be removed.
    harness.pump_frames(4);
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
}

#[test]
fn destroyed_space_waits_for_a_complete_membership_snapshot() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const FIRST_TARGET_ID: WorkspaceId = TEST_WORKSPACE_ID + 10;
    const SECOND_TARGET_ID: WorkspaceId = TEST_WORKSPACE_ID + 11;
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![FULLSCREEN_WORKSPACE_ID, FIRST_TARGET_ID, SECOND_TARGET_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = SECOND_TARGET_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, SECOND_TARGET_ID, false);
    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    // Repeat the first result so debug-only membership logging cannot consume
    // the state transition this test is meant to exercise.
    harness.mock_state.script_workspace_membership_queries(
        FIRST_TARGET_ID,
        [Ok(vec![0]), Ok(vec![0]), Ok(vec![])],
    );
    harness
        .mock_state
        .script_workspace_membership_queries(SECOND_TARGET_ID, [Err(()), Err(()), Ok(vec![0])]);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID
                && strip.contains(fullscreen_window)),
        "one failed candidate query must keep the source recoverable"
    );
    assert!(
        strips
            .iter(harness.world())
            .filter(|strip| [FIRST_TARGET_ID, SECOND_TARGET_ID].contains(&strip.id()))
            .all(|strip| !strip.contains(fullscreen_window)),
        "a partial snapshot must not choose a stale successful candidate"
    );

    harness.pump_frames(8);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let target = strips
        .iter(harness.world())
        .find(|strip| strip.id() == SECOND_TARGET_ID)
        .expect("settled target strip");
    assert!(target.contains(fullscreen_window));
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
}

#[test]
fn destroyed_space_waits_for_unique_membership() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const FIRST_TARGET_ID: WorkspaceId = TEST_WORKSPACE_ID + 10;
    const SECOND_TARGET_ID: WorkspaceId = TEST_WORKSPACE_ID + 11;
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![FULLSCREEN_WORKSPACE_ID, FIRST_TARGET_ID, SECOND_TARGET_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = SECOND_TARGET_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, SECOND_TARGET_ID, false);
    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.mock_state.script_workspace_membership_queries(
        FIRST_TARGET_ID,
        [Ok(vec![0]), Ok(vec![0]), Ok(vec![])],
    );
    harness.mock_state.script_workspace_membership_queries(
        SECOND_TARGET_ID,
        [Ok(vec![0]), Ok(vec![0]), Ok(vec![0])],
    );
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID
                && strip.contains(fullscreen_window)),
        "ambiguous membership must keep the source recoverable"
    );
    assert!(
        strips
            .iter(harness.world())
            .filter(|strip| [FIRST_TARGET_ID, SECOND_TARGET_ID].contains(&strip.id()))
            .all(|strip| !strip.contains(fullscreen_window)),
        "a window reported by two Spaces must not be guessed into either one"
    );

    harness.pump_frames(8);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let target = strips
        .iter(harness.world())
        .find(|strip| strip.id() == SECOND_TARGET_ID)
        .expect("settled target strip");
    assert!(target.contains(fullscreen_window));
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );
}

#[test]
fn hidden_window_during_space_reassignment_retiles_into_its_live_target() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let entity = find_window_entity(0, harness.world());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_some(),
        "the window must be pending before visibility changes"
    );

    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .world()
        .entity_mut(entity)
        .insert(WindowVisibility::Minimized);
    harness.pump_frames(2);

    let previous = harness
        .world()
        .get::<PreviousTiledStrip>(entity)
        .copied()
        .expect("hidden window must remember its live destination");
    assert_eq!(previous.workspace_id, TEST_WORKSPACE_ID);
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_none(),
        "resolved hidden windows must leave the Space-reassignment state"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| !strip.contains(entity)),
        "a hidden window must not be inserted into a live layout"
    );
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID)
    );

    harness
        .world()
        .entity_mut(entity)
        .remove::<WindowVisibility>();
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity)),
        "showing the window must retile it into the live destination"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(entity))
            .count(),
        1
    );
    assert!(harness.world().get::<PreviousTiledStrip>(entity).is_none());
}

#[test]
fn hidden_before_space_destroy_rehomes_from_live_membership() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let entity = find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(WindowVisibility::Hidden);
    harness.pump_frames(1);

    assert_eq!(
        harness
            .world()
            .get::<PreviousTiledStrip>(entity)
            .expect("hidden window must remember its source Space")
            .workspace_id,
        FULLSCREEN_WORKSPACE_ID
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| !strip.contains(entity))
    );

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_some(),
        "destroy invalidation must include hidden windows detached from the source strip"
    );

    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness.pump_frames(2);

    let previous = harness
        .world()
        .get::<PreviousTiledStrip>(entity)
        .copied()
        .expect("live membership must replace the destroyed source");
    assert_eq!(previous.workspace_id, TEST_WORKSPACE_ID);
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_none()
    );

    harness
        .world()
        .entity_mut(entity)
        .remove::<WindowVisibility>();
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity))
    );
    assert!(harness.world().get::<Floating>(entity).is_none());
    assert!(harness.world().get::<PreviousTiledStrip>(entity).is_none());
}

#[test]
#[allow(clippy::too_many_lines)]
fn restored_unavailable_pending_window_rehomes_while_source_still_exists() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .with_workspace_window(1, FULLSCREEN_WORKSPACE_ID, |_| {});
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let entity = find_window_entity(0, harness.world());
    let sibling = find_window_entity(1, harness.world());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity) && strip.contains(sibling)),
        "fixture must keep a sibling in the disappearing source"
    );

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_some()
    );

    harness.mock_state.os_withdraw_window(0);
    harness.world().write_message(Event::ReconcileWindows {
        scope: ReconcileScope::Application(TEST_PROCESS_ID),
    });
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some()
    );
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_some(),
        "transient lifecycle suspension must preserve Space reassignment"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity) && strip.contains(sibling)),
        "lifecycle suspension must preserve the source layout until membership can settle"
    );

    harness.mock_state.os_restore_withdrawn_window(0);
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness.world().write_message(Event::ReconcileWindows {
        scope: ReconcileScope::Application(TEST_PROCESS_ID),
    });
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(4);

    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_none(),
        "restoration into the pending source must not cancel destination reconciliation"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity))
    );
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
            .is_some_and(|strip| !strip.contains(entity) && strip.contains(sibling))
    );
}

#[test]
fn unavailable_before_space_destroy_waits_in_source_then_rehomes() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let entity = find_window_entity(0, harness.world());
    harness.mock_state.os_withdraw_window(0);
    harness.world().write_message(Event::ReconcileWindows {
        scope: ReconcileScope::Application(TEST_PROCESS_ID),
    });
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some()
    );
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_none(),
        "the Space has not been invalidated yet"
    );

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_some(),
        "destroy invalidation must include unavailable windows retained in the source strip"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity)),
        "the destroyed source must remain as a tombstone while its window is suspended"
    );

    harness.mock_state.os_restore_withdrawn_window(0);
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness.world().write_message(Event::ReconcileWindows {
        scope: ReconcileScope::Application(TEST_PROCESS_ID),
    });
    harness.pump_frames(2);

    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_none()
    );
    assert!(harness.world().get::<Floating>(entity).is_none());
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(entity)
            .is_none(),
        "live target reconciliation must terminate the pending state"
    );
    assert!(harness.world().get::<PreviousTiledStrip>(entity).is_none());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(entity)),
        "the restored window must tile into its live target Space"
    );
}

#[test]
fn startup_fullscreen_on_inactive_display_reenters_its_live_user_space() {
    const SECOND_DISPLAY_ID: u32 = TEST_DISPLAY_ID + 1;
    const SECOND_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            SECOND_DISPLAY_ID,
            IRect::new(
                TEST_DISPLAY_WIDTH,
                0,
                TEST_DISPLAY_WIDTH * 2,
                TEST_DISPLAY_HEIGHT,
            ),
            vec![FULLSCREEN_WORKSPACE_ID, SECOND_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .with_workspace_window(1, SECOND_WORKSPACE_ID, |_| {});
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = SECOND_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(SECOND_DISPLAY_ID, SECOND_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(SECOND_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(2);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let second_display_strip = strips
        .iter(harness.world())
        .find(|strip| strip.id() == SECOND_WORKSPACE_ID)
        .expect("second display user Space strip");
    assert!(second_display_strip.contains(fullscreen_window));
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| !strip.contains(fullscreen_window)),
        "global active Space must not be guessed as the destination"
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TEST_WORKSPACE_ID],
        "an inactive display transition must not replace the global active Space"
    );
}

#[test]
fn destroyed_user_space_rehomes_every_window_from_live_membership() {
    const TARGET_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET_WORKSPACE_ID],
        )
        .with_windows(2);
    harness.pump_frames(10);

    let first = find_window_entity(0, harness.world());
    let second = find_window_entity(1, harness.world());
    for window_id in [0, 1] {
        harness.mock_state.update_window(window_id, |window| {
            window.workspace_id = TARGET_WORKSPACE_ID;
        });
    }
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TARGET_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: TEST_WORKSPACE_ID,
    });
    harness.pump_frames(1);

    let mut strips = harness.world().query::<&LayoutStrip>();
    let target = strips
        .iter(harness.world())
        .find(|strip| strip.id() == TARGET_WORKSPACE_ID)
        .expect("target strip");
    assert!(target.contains(first));
    assert!(target.contains(second));
    assert_eq!(target.index_of(first).expect("first window index"), 0);
    assert_eq!(target.index_of(second).expect("second window index"), 1);
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != TEST_WORKSPACE_ID)
    );
    let mut active_strips = harness
        .world()
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
    assert_eq!(
        active_strips
            .iter(harness.world())
            .map(LayoutStrip::id)
            .collect::<Vec<_>>(),
        vec![TARGET_WORKSPACE_ID]
    );
}

#[test]
fn destroyed_space_cleans_up_every_strip_with_the_same_stable_id() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![FULLSCREEN_WORKSPACE_ID, TEST_WORKSPACE_ID],
        )
        .with_workspace_window(0, FULLSCREEN_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
    harness.pump_frames(10);

    let display_entity = {
        let world = harness.world();
        world
            .query::<(Entity, &crate::manager::Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == TEST_DISPLAY_ID).then_some(entity))
            .expect("test display entity")
    };
    for _ in 0..2 {
        harness.world().spawn((
            LayoutStrip::new(FULLSCREEN_WORKSPACE_ID),
            Position(Origin::ZERO),
            ChildOf(display_entity),
        ));
    }
    let duplicate_count = {
        let world = harness.world();
        let mut strips = world.query::<&LayoutStrip>();
        strips
            .iter(world)
            .filter(|strip| strip.id() == FULLSCREEN_WORKSPACE_ID)
            .count()
    };
    assert_eq!(
        duplicate_count, 3,
        "fixture must contain duplicate ECS projections of one native Space"
    );

    let fullscreen_window = find_window_entity(0, harness.world());
    harness.mock_state.update_window(0, |window| {
        window.workspace_id = TEST_WORKSPACE_ID;
        window.is_full_screen = false;
    });
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness
        .mock_state
        .destroy_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID);
    harness.world().write_message(Event::SpaceDestroyed {
        space_id: FULLSCREEN_WORKSPACE_ID,
    });
    harness.pump_frames(2);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| strip.id() != FULLSCREEN_WORKSPACE_ID),
        "all duplicate projections of a destroyed stable Space ID must be removed"
    );
    assert!(
        strips
            .iter(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .is_some_and(|strip| strip.contains(fullscreen_window))
    );
    assert_eq!(
        strips
            .iter(harness.world())
            .filter(|strip| strip.contains(fullscreen_window))
            .count(),
        1
    );
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
            Event::ActionRequested {
                action: Action::MoveWindowToSpace {
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
            Event::ActionRequested {
                action: Action::MoveWindowToSpace {
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
        .on_iteration(0, |_, state| {
            assert_eq!(
                state.native_space_intents(),
                vec![crate::manager::NativeSpaceIntent::Focus {
                    space_id: TARGET_SPACE_ID,
                    animate: true,
                }]
            );
        })
        .run(vec![Event::ActionRequested {
            action: Action::FocusSpace {
                space_id: TARGET_SPACE_ID,
            },
        }]);
}

#[test]
fn returning_to_a_space_focuses_its_previous_window() {
    const TARGET_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;

    TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET_SPACE_ID],
        )
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.focus_window(1);
        })
        .on_iteration(1, |world, state| {
            assert_focused!(world, 1);
            state.update_window(1, |window| {
                window.workspace_id = TARGET_SPACE_ID;
            });
            state.activate_workspace(TEST_DISPLAY_ID, TARGET_SPACE_ID, false);
        })
        .on_iteration(2, |_world, state| {
            state.take_focus_requests();
            state.activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 0);
            assert_eq!(state.take_focus_requests(), vec![0]);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::SpaceChanged,
            Event::SpaceChanged,
        ]);
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
fn startup_without_confirmed_focus_does_not_focus_the_first_window() {
    TestHarness::new()
        .with_windows(1)
        .without_focused_window()
        .on_iteration(0, |world, state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(focused.iter(world).count(), 0);
            assert!(
                state.take_focus_requests().is_empty(),
                "startup without an AX focus must not issue a focus request"
            );
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
            Event::WindowMinimized {
                window_id: 0,
                incarnation: None,
            },
            Event::WindowDeminimized {
                window_id: 0,
                incarnation: None,
            },
        ]);
}

#[test]
fn minimizing_an_unfocused_window_does_not_change_focus() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| {
            state.take_focus_requests();
        })
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            assert!(
                state.take_focus_requests().is_empty(),
                "minimizing an unfocused window must not request another focus"
            );
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::WindowMinimized {
                window_id: 2,
                incarnation: None,
            },
        ]);
}

#[test]
fn destroying_an_unfocused_window_does_not_change_focus() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| {
            state.take_focus_requests();
            state.os_vanish_window(2);
        })
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            assert!(
                state.take_focus_requests().is_empty(),
                "destroying an unfocused window must not request another focus"
            );
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::WindowDestroyed {
                window_id: 2,
                source: DestroySource::WindowServer,
                incarnation: None,
            },
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
            world.trigger(SpawnWindowTrigger::new(vec![window]));
        })
        .on_iteration(3, |world, _state| {
            // usable origin = (pad_left, menubar + pad_top) = (40, 20 + 15).
            assert_window_at!(world, 0, 40, TEST_MENUBAR_HEIGHT + 15);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::ActionRequested {
                action: Action::PrintState,
            },
            Event::ActionRequested {
                action: Action::PrintState,
            },
        ]);
}

#[test]
fn test_dont_focus() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 }, // 0
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        }, // 1
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
        }, // 2
        Event::ActionRequested {
            action: Action::PrintState,
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
            world.trigger(SpawnWindowTrigger::new(vec![window]));
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
    assert!(parse_action(&["window", "focus", "0"]).is_err());
    let action = parse_action(&["window", "focus", "2"]).unwrap();

    TestHarness::new()
        .with_windows(3)
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| assert_focused!(world, 1))
        .on_iteration(3, |world, _state| assert_focused!(world, 2))
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::ActionRequested {
                action: action.clone(),
            },
            Event::ActionRequested {
                action: Action::Window(Operation::ToggleFloating),
            },
            Event::ActionRequested { action },
        ]);
}

#[test]
fn focus_next_and_previous_wrap_in_tiled_order() {
    let next = parse_action(&["window", "focus", "next"]).unwrap();
    let previous = parse_action(&["window", "focus", "previous"]).unwrap();

    TestHarness::new()
        .with_windows(3)
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| assert_focused!(world, 2))
        .on_iteration(3, |world, _state| assert_focused!(world, 0))
        .on_iteration(4, |world, _state| assert_focused!(world, 2))
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            Event::ActionRequested {
                action: next.clone(),
            },
            Event::ActionRequested {
                action: next.clone(),
            },
            Event::ActionRequested { action: next },
            Event::ActionRequested { action: previous },
        ]);
}

#[test]
fn focus_next_and_previous_use_stable_floating_order() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    let config: Config = (MainOptions::default(), vec![params]).into();
    let next = parse_action(&["window", "focus", "next"]).unwrap();
    let previous = parse_action(&["window", "focus", "previous"]).unwrap();

    TestHarness::new()
        .with_config(config)
        .with_window(30, |_| {})
        .with_window(20, |_| {})
        .with_window(10, |_| {})
        .with_focused_window(20)
        .on_iteration(0, |world, _state| assert_focused!(world, 20))
        .on_iteration(1, |world, _state| assert_focused!(world, 30))
        .on_iteration(2, |world, _state| assert_focused!(world, 10))
        .on_iteration(3, |world, _state| assert_focused!(world, 30))
        .run(vec![
            Event::MenuOpened { window_id: 20 },
            Event::ActionRequested {
                action: next.clone(),
            },
            Event::ActionRequested { action: next },
            Event::ActionRequested { action: previous },
        ]);
}

#[test]
fn test_offscreen_windows_preserve_height() {
    let expected_height = TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT;

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::Swipe {
            delta: 0.2,
            fingers: 3,
        },
        Event::ActionRequested {
            action: Action::PrintState,
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::First)),
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
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Center),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Move(Direction::Last)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Move(Direction::West)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ]);

    assert_focused!(harness.world(), 4);

    let focus_west = Event::ActionRequested {
        action: Action::Window(Operation::Focus(Direction::West)),
    };
    for _ in 0..3 {
        harness
            .app
            .world_mut()
            .write_message::<Event>(focus_west.clone());
        harness.app.update();
    }

    let requested = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .navigation_entity(TEST_WORKSPACE_ID)
        .expect("rapid commands retain their requested navigation target");
    assert_eq!(entity_to_window_id(harness.world(), requested), 1);
    assert_focused!(harness.world(), 4);

    for _ in 0..5 {
        for event in harness.mock_state.drain_events() {
            harness.app.world_mut().write_message::<Event>(event);
        }
        harness.app.update();
    }
    assert_focused!(harness.world(), 1);
}

#[test]
fn keyboard_focus_request_makes_the_target_visible_without_a_second_confirmed_reflow() {
    let mut harness = TestHarness::new().with_windows(5);
    harness.pump_frames(15);
    assert_focused!(harness.world(), 0);

    let strip_position = |world: &mut World| {
        let mut strips =
            world.query_filtered::<&Position, (With<LayoutStrip>, With<ActiveWorkspaceMarker>)>();
        strips.single(world).expect("active strip position").0
    };
    let before = strip_position(harness.world());

    harness
        .world()
        .write_message::<Event>(Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::Last)),
        });
    harness.app.update();

    assert_focused!(harness.world(), 0);
    let requested = strip_position(harness.world());
    assert_ne!(
        requested, before,
        "requested focus is layout state and must make the target visible even when AX confirmation is delayed"
    );

    for event in harness.mock_state.drain_events() {
        harness.world().write_message::<Event>(event);
    }
    harness.pump_frames(5);

    assert_focused!(harness.world(), 4);
    assert_eq!(
        strip_position(harness.world()),
        requested,
        "confirming an already projected focus request must not move the strip a second time"
    );
}

#[test]
fn test_stale_focus_event_ignored() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        Event::window_focused(4),
        Event::ActionRequested {
            action: Action::PrintState,
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
fn stale_known_focus_event_does_not_trigger_automatic_recovery() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        Event::window_focused(4),
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(5)
        .on_iteration(2, |world, state| {
            assert_focused!(world, 1);
            state.take_focus_requests();
            let focused = world
                .query_filtered::<Entity, With<FocusedMarker>>()
                .single(world)
                .expect("focus anchor before simulated marker loss");
            world.entity_mut(focused).remove::<FocusedMarker>();
        })
        .on_iteration(5, |world, state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(focused.iter(world).count(), 0);
            assert!(state.take_focus_requests().is_empty());
        })
        .run(commands);
}

#[test]
fn unknown_focus_clears_confirmed_focus_but_preserves_navigation() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::window_focused(999),
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(1, |world, _state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(
                focused.iter(world).count(),
                0,
                "an ignored window must not leave a tracked window confirmed"
            );
        })
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 1);
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
        Event::ActionRequested {
            action: Action::PrintState,
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
        Event::ActionRequested {
            action: Action::PrintState,
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
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
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
            // Keep this a genuinely untracked AX window even after the
            // inventory heartbeat discovers its element.
            state.update_window(EXTERNAL_WINDOW_ID, |window| {
                window.role = "AXUnknown".to_string();
            });
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = Some(EXTERNAL_WINDOW_ID);
            });
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
fn focus_query_timeout_never_focuses_an_arbitrary_window() {
    let mut commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::FocusRevalidationRequested {
            pid: TEST_PROCESS_ID,
            source: FocusSource::AccessibilityWindow,
        },
    ];
    commands.extend((0..7).map(|_| Event::ActionRequested {
        action: Action::PrintState,
    }));

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |_world, state| {
            state.update_app(TEST_PROCESS_ID, |app| {
                app.focused_window_id = None;
            });
        })
        .on_iteration(6, |_world, state| {
            state.take_focus_requests();
        })
        .on_iteration(8, |world, state| {
            let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
            assert_eq!(focused.iter(world).count(), 0);
            assert!(
                state.take_focus_requests().is_empty(),
                "an unresolved observation must not issue a focus request"
            );
        })
        .run(commands);
}

#[test]
fn test_repeated_external_focus_reshuffles_already_focused_window() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::West)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::West)),
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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::West)),
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
fn focus_other_layer_tracks_the_focused_tier() {
    fn current_layer(world: &mut World) -> FloatingLayer {
        let mut query = world.query::<&FloatingLayer>();
        *query
            .query(world)
            .iter()
            .find(|layer| layer.workspace_id == TEST_WORKSPACE_ID)
            .expect("active workspace has FloatingLayer")
    }

    let commands = vec![
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::FocusTiled),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::FocusOtherLayer),
        },
        Event::ActionRequested {
            action: Action::Window(Operation::FocusOtherLayer),
        },
    ];

    let mut floating = WindowParams::new("^Window 2$", None);
    floating.floating = Some(true);
    let config: Config = (MainOptions::default(), vec![floating]).into();

    TestHarness::new()
        .with_config(config)
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            assert!(!current_layer(world).front);
        })
        .on_iteration(2, |world, _state| {
            assert!(current_layer(world).front);
        })
        .on_iteration(3, |world, _state| {
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
        })
        .with_focused_window(0);

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::FocusFloating),
        },
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
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

    let pump = |h: &mut TestHarness, c: Action| {
        h.app
            .world_mut()
            .write_message::<Event>(Event::ActionRequested { action: c });
        for _ in 0..10 {
            h.app.update();
            for e in h.mock_state.drain_events() {
                h.app.world_mut().write_message::<Event>(e);
            }
        }
    };

    // Boot the strip; column 0 (window id 0) sits at layout x 0.
    pump(&mut h, Action::PrintState);

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
        Event::ActionRequested {
            action: Action::Window(Operation::Focus(Direction::East)),
        },
        // Swipe windows 0 and 1 off screen.
        Event::Swipe {
            delta: 0.3,
            fingers: 3,
        },
        // Noop to let the scroll settle.
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
        },
        // Now swipe the stacked windows off screen again.
        Event::Swipe {
            delta: 0.1,
            fingers: 3,
        },
        // Noop to let the scroll settle.
        Event::MenuOpened { window_id: 0 },
        Event::ActionRequested {
            action: Action::Window(Operation::ToggleStack),
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
        Event::ActionRequested {
            action: Action::PrintState,
        },
        Event::ActionRequested {
            action: Action::PrintState,
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
    let incarnation = world
        .get::<Window>(entity)
        .expect("tracked window")
        .incarnation();

    // A move of ours is in flight, and the app reports a frame we didn't ask
    // for. Displaced on the axis the animation leaves alone, so the assertion
    // can't be confused by how far the lerp has run.
    world
        .entity_mut(entity)
        .insert(RepositionMarker(Origin::new(5000, before.y)));
    state.os_move_window(0, Origin::new(before.x, before.y + 888));
    world.write_message(Event::WindowMoved {
        window_id: 0,
        incarnation,
    });

    world
        .run_system_once(crate::ecs::window_geometry::observe_external_window_geometry)
        .expect("running external geometry observer");

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
            Event::ActionRequested {
                action: Action::FocusWindow { window_id: 1 },
            },
        ]);
}

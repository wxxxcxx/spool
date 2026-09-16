use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use crate::assert_focused;
use crate::commands::{Action, Direction, Operation};
use crate::config::{Config, MainOptions, WindowParams};
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    Bounds, Floating, FocusedMarker, ObservedWindowFrame, Position, PresentedWindowFrame,
    SpawnWindowTrigger, WindowDefaultsPending,
};
use crate::manager::{Application, Window};

use super::*;

fn set_column_width_intent(harness: &mut TestHarness, entity: Entity, width: i32) {
    use crate::ecs::layout::WidthIntent;
    let world = harness.world();
    let mut strips = world.query::<&mut LayoutStrip>();
    for mut strip in strips.iter_mut(world) {
        if let Some(id) = strip.column_id(entity) {
            strip
                .set_width_intent(id, WidthIntent::Absolute(f64::from(width)))
                .unwrap();
            return;
        }
    }
    panic!("test window must belong to a column");
}

fn reconciliation_fixture_config() -> Config {
    (
        MainOptions {
            preset_column_widths: vec![
                f64::from(TEST_WINDOW_WIDTH) / f64::from(TEST_DISPLAY_WIDTH),
            ],
            ..Default::default()
        },
        vec![],
    )
        .into()
}

#[test]
fn application_ax_failure_backs_off_event_storms_and_recovers() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(20);
    let pid = TEST_PROCESS_ID + 1;
    harness
        .mock_state
        .spawn_app(pid, "unresponsive.test", "Unresponsive");
    harness
        .mock_state
        .set_application_ax_error(pid, Some(accessibility_sys::kAXErrorCannotComplete));
    let app = harness.mock_state.create_application(pid);
    let application = harness.world().spawn(app).id();
    let healthy_before = harness
        .mock_state
        .application_ax_attempts(TEST_PROCESS_ID)
        .1;

    for _ in 0..100 {
        harness
            .world()
            .write_message(crate::events::Event::ReconcileWindows {
                scope: crate::events::ReconcileScope::All,
            });
        harness.pump_frames(1);
    }

    let (observers, inventories) = harness.mock_state.application_ax_attempts(pid);
    assert!(
        (1..=4).contains(&observers),
        "ten seconds of AX failure must use bounded retries, got {observers}"
    );
    assert_eq!(
        inventories, 0,
        "do not query AXWindows after communication failed"
    );
    assert!(harness.world().get::<Application>(application).is_some());
    assert!(
        harness
            .mock_state
            .application_ax_attempts(TEST_PROCESS_ID)
            .1
            > healthy_before
    );

    harness.mock_state.set_application_ax_error(pid, None);
    let _window =
        harness
            .mock_state
            .spawn_window(pid, TEST_WORKSPACE_ID, 77, IRect::new(0, 0, 400, 400));
    harness.pump_frames(80);
    let window = find_window_entity(77, harness.world());
    assert_eq!(
        harness.world().get::<ChildOf>(window).unwrap().parent(),
        application
    );
    let attempts = harness.mock_state.application_ax_attempts(pid);
    harness.pump_frames(30);
    let recovered = harness.mock_state.application_ax_attempts(pid);
    assert_eq!(
        recovered.0, attempts.0,
        "successful observers remain settled"
    );
    assert!(
        recovered.1 > attempts.1,
        "recovery restores regular inventory audits"
    );
}

#[test]
fn application_ax_inventory_failure_preserves_layout_and_retries() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(20);
    let entity = find_window_entity(0, harness.world());
    let incarnation = harness.world().get::<Window>(entity).unwrap().incarnation();
    let desired = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    let before = harness.mock_state.application_ax_attempts(TEST_PROCESS_ID);
    harness.mock_state.set_application_ax_error(
        TEST_PROCESS_ID,
        Some(accessibility_sys::kAXErrorCannotComplete),
    );
    for _ in 0..100 {
        harness
            .world()
            .write_message(crate::events::Event::ReconcileWindows {
                scope: crate::events::ReconcileScope::All,
            });
        harness.pump_frames(1);
    }
    let attempts = harness.mock_state.application_ax_attempts(TEST_PROCESS_ID);
    assert_eq!(attempts.0, before.0);
    assert!((1..=4).contains(&(attempts.1 - before.1)));
    assert_eq!(find_window_entity(0, harness.world()), entity);
    assert_eq!(
        harness.world().get::<Window>(entity).unwrap().incarnation(),
        incarnation
    );
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::DesiredWindowFrame>(entity)
            .unwrap()
            .0,
        desired
    );
    let world = harness.world();
    let mut strips = world.query::<&LayoutStrip>();
    assert!(strips.iter(world).any(|strip| strip.contains(entity)));

    harness
        .mock_state
        .set_application_ax_error(TEST_PROCESS_ID, None);
    harness.pump_frames(80);
    assert_eq!(find_window_entity(0, harness.world()), entity);
    assert!(
        harness
            .mock_state
            .application_ax_attempts(TEST_PROCESS_ID)
            .1
            > attempts.1
    );
}

#[test]
fn application_ax_cooldown_does_not_prevent_exit_or_block_a_reused_pid() {
    let mut harness = TestHarness::new();
    harness.pump_frames(20);
    let pid = TEST_PROCESS_ID + 1;
    harness
        .mock_state
        .spawn_app(pid, "unresponsive.test", "Unresponsive");
    harness
        .mock_state
        .set_application_ax_error(pid, Some(accessibility_sys::kAXErrorCannotComplete));
    let app = harness.mock_state.create_application(pid);
    let old = harness.world().spawn(app).id();
    for _ in 0..100 {
        harness
            .world()
            .write_message(crate::events::Event::ReconcileWindows {
                scope: crate::events::ReconcileScope::All,
            });
        harness.pump_frames(1);
    }
    harness
        .mock_state
        .update_app(pid, |app| app.running = false);
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::Application(pid),
        });
    harness.pump_frames(1);
    assert!(harness.world().get::<Application>(old).is_none());

    harness
        .mock_state
        .spawn_app(pid, "replacement.test", "Replacement");
    harness.mock_state.set_application_ax_error(pid, None);
    let before = harness.mock_state.application_ax_attempts(pid);
    let app = harness.mock_state.create_application(pid);
    let replacement = harness.world().spawn(app).id();
    assert_ne!(old, replacement);
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::Application(pid),
        });
    harness.pump_frames(1);
    let after = harness.mock_state.application_ax_attempts(pid);
    assert_eq!(after, (before.0 + 1, before.1 + 1));
}

#[test]
fn frame_audit_leaves_physical_writes_to_the_presentation_committer() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    let target = IRect::from_corners(target.min, target.max + IVec2::new(100, 0));
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::DesiredWindowFrame(target));
    let drift = IRect::from_corners(
        target.min + IVec2::new(80, 50),
        target.max + IVec2::new(180, 150),
    );
    harness.mock_state.os_set_window_frame_silently(0, drift);
    let state = harness.mock_state.clone();
    harness.app.add_systems(
        PostUpdate,
        (move || {
            assert_eq!(
                state.actual_window_frame(0),
                Some(drift),
                "audit must observe without writing before presentation"
            );
        })
        .before(crate::ecs::window_frame::animate_presented_window_frames),
    );
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::All,
        });
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(target));
}

#[test]
fn frame_correction_yields_to_new_intent_before_commit() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    let drift = IRect::from_corners(
        target.min + IVec2::splat(80),
        target.max + IVec2::splat(180),
    );
    let replacement = IRect::from_corners(
        target.min + IVec2::splat(20),
        target.max + IVec2::splat(220),
    );
    harness.mock_state.os_set_window_frame_silently(0, drift);
    let attempts = harness.mock_state.frame_write_attempts(0);
    harness.app.add_systems(
        PostUpdate,
        (move |mut commands: Commands| {
            commands.entity(entity).insert((
                crate::ecs::DesiredWindowFrame(replacement),
                PresentedWindowFrame(replacement),
            ));
        })
        .before(crate::ecs::window_frame::animate_presented_window_frames),
    );
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::All,
        });
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(replacement));
    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - attempts,
        1,
        "one commit must apply current intent without first writing the stale audit target"
    );
}

#[test]
fn frame_correction_yields_to_space_reassignment_before_commit() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    let drift = IRect::from_corners(
        target.min + IVec2::splat(80),
        target.max + IVec2::splat(180),
    );
    harness.mock_state.os_set_window_frame_silently(0, drift);
    let attempts = harness.mock_state.frame_write_attempts(0);
    harness.app.add_systems(
        PostUpdate,
        (move |mut commands: Commands| {
            crate::ecs::workspace::freeze_window_for_space_reassignment(entity, 0, &mut commands);
        })
        .before(crate::ecs::window_frame::animate_presented_window_frames),
    );
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::All,
        });
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(drift));
    assert_eq!(harness.mock_state.frame_write_attempts(0), attempts);
}

fn assert_stale_geometry_callbacks_are_ignored(
    harness: &mut TestHarness,
    replacement: Entity,
    old_incarnation: crate::platform::WindowIncarnation,
) {
    let position = harness
        .world()
        .get::<Position>(replacement)
        .expect("replacement position")
        .0;
    let bounds = harness
        .world()
        .get::<Bounds>(replacement)
        .expect("replacement bounds")
        .0;
    harness.mock_state.os_set_window_frame_silently(
        0,
        IRect::from_corners(
            position + IVec2::new(90, 60),
            position + bounds + IVec2::new(190, 160),
        ),
    );
    harness
        .world()
        .write_message(crate::events::Event::WindowMoved {
            window_id: 0,
            incarnation: old_incarnation,
        });
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation: old_incarnation,
        });
    harness
        .world()
        .run_system_once(crate::ecs::window_geometry::observe_external_window_geometry)
        .expect("run geometry observer");
    assert_eq!(
        harness
            .world()
            .get::<Position>(replacement)
            .expect("replacement position")
            .0,
        position,
        "a delayed move callback from the old AX element must not target the replacement"
    );
    assert_eq!(
        harness
            .world()
            .get::<Bounds>(replacement)
            .expect("replacement bounds")
            .0,
        bounds,
        "a delayed resize callback from the old AX element must not target the replacement"
    );
}

#[test]
fn window_state_sync_recovers_a_missed_same_application_focus_change() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);
    assert_focused!(harness.world(), 0);

    // Change the application's AX focus truth without delivering either of the
    // normal focus notifications.
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = Some(1);
    });
    harness.pump_frames(40);

    assert_focused!(harness.world(), 1);
    assert_eq!(
        harness
            .world()
            .resource::<FocusCoordinator>()
            .snapshot()
            .confirmed_window_id(),
        Some(1)
    );
}

#[test]
fn transient_ax_focus_failure_does_not_confirm_a_stale_observation() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);
    assert_focused!(harness.world(), 0);

    harness
        .mock_state
        .fail_focused_window_queries(TEST_PROCESS_ID, 1);
    harness
        .world()
        .write_message(crate::events::Event::window_focused(1));
    harness.pump_frames(1);

    assert_focused!(harness.world(), 0);
    assert_eq!(
        harness
            .world()
            .resource::<FocusCoordinator>()
            .snapshot()
            .confirmed_window_id(),
        Some(0),
        "an AX query error is unknown evidence and must not change confirmed focus"
    );
}

#[test]
fn stray_focus_retry_cannot_cross_a_reused_window_id_on_ax_failure() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        99,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    ));
    harness
        .world()
        .write_message(crate::events::Event::window_focused(99));
    harness.pump_frames(1);

    harness.mock_state.os_stale_window_without_notifications(99);
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        99,
        IRect::new(120, 90, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = Some(0);
    });
    harness
        .mock_state
        .fail_focused_window_queries(TEST_PROCESS_ID, 4);
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(2);

    let replacement = find_window_entity(99, harness.world());
    assert!(harness.world().get::<FocusedMarker>(replacement).is_none());
    assert_ne!(
        harness
            .world()
            .resource::<FocusCoordinator>()
            .snapshot()
            .confirmed_window_id(),
        Some(99),
        "a delayed retry must not treat an AX error as proof that a reused ID is focused"
    );
}

#[test]
fn window_state_sync_recovers_focus_after_the_focused_window_silently_closes() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);

    let closed = find_window_entity(0, harness.world());
    harness.mock_state.os_vanish_window_with_stale_ax_handle(0);
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = Some(1);
    });
    harness.pump_frames(40);

    assert!(harness.world().get_entity(closed).is_err());
    assert_focused!(harness.world(), 1);
}

#[test]
fn window_state_sync_clears_stale_focus_when_the_front_app_has_no_window() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    assert_focused!(harness.world(), 0);

    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = None;
    });
    harness.pump_frames(40);

    let mut focused = harness
        .world()
        .query_filtered::<Entity, With<FocusedMarker>>();
    assert!(focused.iter(harness.world()).next().is_none());
    assert_eq!(
        harness
            .world()
            .resource::<FocusCoordinator>()
            .snapshot()
            .confirmed_window_id(),
        None
    );
    let mut retries = harness
        .world()
        .query_filtered::<Entity, With<crate::ecs::RetryFrontSwitch>>();
    assert_eq!(
        retries.iter(harness.world()).count(),
        0,
        "one no-focus episode must not restart focus retries on every heartbeat"
    );
}

#[test]
fn a_recovered_focus_starts_a_new_no_focus_episode() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = None;
    });
    harness.pump_frames(12);
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = Some(0);
    });
    harness.pump_frames(2);
    assert_focused!(harness.world(), 0);

    // Lose focus again before a successful heartbeat has a chance to clear the
    // first absence ledger entry.
    harness.mock_state.update_app(TEST_PROCESS_ID, |app| {
        app.focused_window_id = None;
    });
    harness.pump_frames(12);

    assert_focused!(harness.world(), 0);
    harness.pump_frames(12);

    let mut focused = harness
        .world()
        .query_filtered::<Entity, With<FocusedMarker>>();
    assert!(
        focused.iter(harness.world()).next().is_none(),
        "a repeated no-focus episode clears confirmation only after its bounded AX retry expires"
    );
}

#[test]
fn window_state_sync_retires_an_application_after_a_missed_termination() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let window = find_window_entity(0, harness.world());
    let application = {
        let world = harness.world();
        let mut applications = world.query::<(Entity, &crate::manager::Application)>();
        applications
            .iter(world)
            .next()
            .map(|(entity, _)| entity)
            .expect("application")
    };
    harness.mock_state.set_app_running(TEST_PROCESS_ID, false);
    harness.pump_frames(40);

    assert!(harness.world().get_entity(window).is_err());
    assert!(harness.world().get_entity(application).is_err());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| !strip.contains(window))
    );
}

#[test]
fn window_state_sync_fails_open_on_a_transient_liveness_error() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let window = find_window_entity(0, harness.world());
    let application = harness
        .world()
        .get::<ChildOf>(window)
        .expect("window owner")
        .parent();
    harness
        .mock_state
        .fail_application_liveness(TEST_PROCESS_ID, 1);
    harness.pump_frames(12);

    assert!(harness.world().get_entity(window).is_ok());
    assert!(harness.world().get_entity(application).is_ok());

    harness.pump_frames(12);
    assert!(harness.world().get_entity(window).is_ok());
    assert!(harness.world().get_entity(application).is_ok());
}

#[test]
fn targeted_spawn_uses_the_exact_application_when_a_pid_is_reused() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);

    let application = harness.mock_state.create_application(TEST_PROCESS_ID);
    let replacement_app = harness.world().spawn(application).id();
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        99,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness.world().trigger(SpawnWindowTrigger::for_application(
        replacement_app,
        vec![window],
    ));
    harness.world().flush();

    let entity = find_window_entity(99, harness.world());
    assert_eq!(
        harness
            .world()
            .get::<ChildOf>(entity)
            .expect("window owner")
            .parent(),
        replacement_app
    );
}

#[test]
fn ownership_transfer_reapplies_rules_and_retries_the_new_observer() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let old_application = harness
        .world()
        .get::<ChildOf>(entity)
        .expect("old window owner")
        .parent();
    harness.world().entity_mut(entity).insert(Floating);
    harness.world().flush();
    harness.pump_frames(1);
    assert!(harness.world().get::<Floating>(entity).is_some());

    harness.mock_state.set_app_running(TEST_PROCESS_ID, false);
    let replacement = harness
        .mock_state
        .create_application_with_running(TEST_PROCESS_ID, true);
    let replacement_app = harness.world().spawn(replacement).id();
    let attempts_before = harness.mock_state.window_observer_attempts(0);
    harness.mock_state.fail_window_observer_attempts(0, 1);
    let window = harness.mock_state.create_window(0);
    harness.world().trigger(SpawnWindowTrigger::for_application(
        replacement_app,
        vec![window],
    ));
    harness.world().flush();
    harness.pump_frames(20);

    assert!(harness.world().get_entity(old_application).is_err());
    assert!(harness.world().get_entity(entity).is_ok());
    assert_eq!(
        harness
            .world()
            .get::<ChildOf>(entity)
            .expect("new window owner")
            .parent(),
        replacement_app
    );
    assert!(harness.world().get::<Floating>(entity).is_none());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.contains(entity))
    );
    assert!(
        harness.mock_state.window_observer_attempts(0) >= attempts_before + 2,
        "the failed observer transfer must be retried by reconciliation"
    );
}

#[test]
fn one_heartbeat_transfers_a_window_before_retiring_its_stale_owner() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let old_application = harness
        .world()
        .get::<ChildOf>(entity)
        .expect("old window owner")
        .parent();
    harness.mock_state.set_app_running(TEST_PROCESS_ID, false);
    let replacement = harness
        .mock_state
        .create_application_with_running(TEST_PROCESS_ID, true);
    let replacement_app = harness.world().spawn(replacement).id();

    // No direct SpawnWindowTrigger: one lifecycle heartbeat must discover the
    // exact window under the new app and retire the stale owner in one command queue.
    harness.pump_frames(12);

    assert!(harness.world().get_entity(old_application).is_err());
    assert!(harness.world().get_entity(entity).is_ok());
    assert_eq!(
        harness
            .world()
            .get::<ChildOf>(entity)
            .expect("transferred owner")
            .parent(),
        replacement_app
    );
}

#[test]
fn visibility_events_skip_a_stale_application_after_pid_reuse() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    harness.mock_state.set_app_running(TEST_PROCESS_ID, false);

    let replacement = harness
        .mock_state
        .create_application_with_running(TEST_PROCESS_ID, true);
    let replacement_app = harness.world().spawn(replacement).id();
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        99,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness.world().trigger(SpawnWindowTrigger::for_application(
        replacement_app,
        vec![window],
    ));
    harness.world().flush();
    let entity = find_window_entity(99, harness.world());

    harness
        .world()
        .write_message(crate::events::Event::ApplicationHidden {
            pid: TEST_PROCESS_ID,
        });
    harness.pump_frames(2);
    assert!(matches!(
        harness.world().get::<crate::ecs::WindowVisibility>(entity),
        Some(crate::ecs::WindowVisibility::Hidden)
    ));

    harness
        .world()
        .write_message(crate::events::Event::ApplicationVisible {
            pid: TEST_PROCESS_ID,
        });
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowVisibility>(entity)
            .is_none()
    );
}

#[test]
fn window_state_sync_missed_close_without_notification_clears_focus_and_slot() {
    let mut harness = TestHarness::new()
        .with_config(reconciliation_fixture_config())
        .with_windows(3);
    harness.pump_frames(10);

    harness.mock_state.focus_window(1);
    harness.pump_frames(5);
    let vanished = find_window_entity(1, harness.world());
    harness.mock_state.os_vanish_window_with_stale_ax_handle(1);

    // No WindowDestroyed or ReconcileWindows event is delivered.
    harness.pump_frames(40);

    assert!(
        harness.world().get_entity(vanished).is_err(),
        "a heartbeat audit must remove a vanished tracked window"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| !strip.contains(vanished)),
        "a vanished window must not retain a tiled slot"
    );
    assert!(
        harness
            .world()
            .resource::<FocusCoordinator>()
            .snapshot()
            .confirmed_window_id()
            .is_none(),
        "a vanished window must not remain confirmed focus"
    );
    let mut focused = harness
        .world()
        .query_filtered::<&Window, With<FocusedMarker>>();
    assert!(focused.iter(harness.world()).next().is_none());
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        TEST_WINDOW_WIDTH
    );
}

#[test]
fn window_state_sync_silent_frame_drift_converges_cache_and_ecs() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);

    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(10);

    let actual = IRect::from_corners(IVec2::new(77, 88), IVec2::new(577, 688));
    harness.mock_state.os_set_window_frame_silently(0, actual);
    assert_ne!(harness.mock_state.cached_window_frame(0), Some(actual));

    // No WindowMoved or WindowResized event is delivered.
    harness.pump_frames(40);

    let cached = harness
        .world()
        .get::<Window>(entity)
        .expect("tracked window")
        .frame();
    let position = harness.world().get::<Position>(entity).expect("position").0;
    let bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(actual));
    assert_eq!(harness.mock_state.cached_window_frame(0), Some(actual));
    assert_eq!(cached, actual);
    assert_eq!(IRect::from_corners(position, position + bounds), actual);
}

#[test]
fn disabled_automatic_sweeps_still_recover_silent_frame_drift() {
    let config: Config = (
        MainOptions {
            automatic_reconcile: Some(false),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(15);
    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(10);
    let actual = IRect::from_corners(IVec2::new(77, 88), IVec2::new(577, 688));
    harness.mock_state.os_set_window_frame_silently(0, actual);
    assert_ne!(harness.mock_state.cached_window_frame(0), Some(actual));
    harness.pump_frames(40);
    assert_eq!(harness.mock_state.cached_window_frame(0), Some(actual));
    assert_eq!(
        harness.world().get::<Window>(entity).unwrap().frame(),
        actual
    );
}

#[test]
fn floating_resize_notification_adopts_the_complete_observed_frame() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(2);
    let old_frame = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("observed frame")
        .0;
    let resized = IRect::from_corners(
        old_frame.min + IVec2::new(50, 40),
        old_frame.max - IVec2::new(30, 20),
    );
    let incarnation = harness
        .world()
        .get::<Window>(entity)
        .expect("window")
        .incarnation();

    harness.mock_state.os_set_window_frame_silently(0, resized);
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(1);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(resized));
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        resized.min,
        "a resize-only notification must not preserve a stale floating origin"
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        resized.size()
    );
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .expect("observed frame")
            .0,
        resized
    );
}

#[test]
fn floating_move_notification_adopts_the_complete_observed_frame() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(2);
    let old_frame = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("observed frame")
        .0;
    let moved = IRect::from_corners(
        old_frame.min + IVec2::new(60, 30),
        old_frame.max + IVec2::new(140, 90),
    );
    let incarnation = harness
        .world()
        .get::<Window>(entity)
        .expect("window")
        .incarnation();

    harness.mock_state.os_set_window_frame_silently(0, moved);
    harness
        .world()
        .write_message(crate::events::Event::WindowMoved {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(1);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(moved));
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        moved.min
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        moved.size(),
        "a move-only notification must not preserve a stale floating size"
    );
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .expect("observed frame")
            .0,
        moved
    );
}

#[test]
fn window_state_sync_unknown_drift_after_completion_preserves_intent_without_fighting() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let desired_position = harness.world().get::<Position>(entity).expect("position").0;
    let desired_bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let desired = IRect::from_corners(desired_position, desired_position + desired_bounds);
    let drifted = IRect::from_corners(
        desired.min + IVec2::new(137, 91),
        desired.max + IVec2::new(237, 191),
    );
    harness.mock_state.os_set_window_frame_silently(0, drifted);

    harness.pump_frames(20);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(drifted));
    assert_eq!(harness.mock_state.cached_window_frame(0), Some(drifted));
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        desired_position,
        "an observed OS drift must not overwrite tiled layout intent"
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        desired_bounds,
        "an observed OS constraint must not overwrite tiled layout intent"
    );
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::ObservedWindowFrame>(entity)
            .expect("confirmed frame")
            .0,
        drifted
    );
}

#[test]
fn window_frame_commit_publishes_confirmed_geometry_without_notification() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let height = harness.world().get::<Bounds>(entity).expect("bounds").0.y;
    let target = IRect::from_corners(IVec2::new(90, 110), IVec2::new(690, 110 + height));
    {
        let mut entity = harness.world().entity_mut(entity);
        entity
            .get_mut::<PresentedWindowFrame>()
            .expect("presented frame")
            .0 = target;
    }

    // Mock geometry writes deliberately emit no AX move/resize notification.
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::DesiredWindowFrame(target));

    harness.pump_frames(1);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(target));
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .expect("confirmed frame")
            .0,
        target,
        "the overlay projection must update from the synchronous AX readback"
    );
}

#[test]
fn failed_geometry_setter_preserves_a_successful_followup_readback() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let current = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("confirmed frame")
        .0;
    let target = IRect::from_corners(
        current.min + IVec2::new(30, 20),
        current.max + IVec2::new(90, 20),
    );
    harness.mock_state.fail_frame_write_readbacks(0, 1);
    {
        let mut entity = harness.world().entity_mut(entity);
        entity
            .get_mut::<PresentedWindowFrame>()
            .expect("presented frame")
            .0 = target;
    }

    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::DesiredWindowFrame(target));

    harness.pump_frames(1);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(target));
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .expect("fresh physical readback")
            .0,
        target,
        "a successful follow-up AX read is authoritative even when the setter reported an error"
    );
}

#[test]
fn failed_geometry_setter_and_followup_read_invalidate_the_overlay_projection() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let current = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("confirmed frame")
        .0;
    let target = IRect::from_corners(
        current.min + IVec2::new(30, 20),
        current.max + IVec2::new(90, 20),
    );
    harness.mock_state.fail_frame_write_readbacks(0, 1);
    harness.mock_state.fail_frame_updates(0, 1);
    harness
        .world()
        .entity_mut(entity)
        .get_mut::<PresentedWindowFrame>()
        .expect("presented frame")
        .0 = target;

    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::DesiredWindowFrame(target));

    harness.pump_frames(1);

    assert!(
        harness.world().get::<ObservedWindowFrame>(entity).is_none(),
        "without a confirmed follow-up read, the overlay must not retain stale geometry"
    );
    let (query, snapshot) = harness
        .world()
        .run_system_once(|state: crate::ecs::state::QueryStateParams| {
            (state.extract().unwrap(), state.extract_window_set())
        })
        .unwrap();
    let published = query
        .spaces
        .iter()
        .flat_map(|space| &space.windows)
        .find(|window| window.window_id == 0)
        .unwrap();
    assert!(
        published.frame.is_none(),
        "failed readback must not publish an unconfirmed frame"
    );
    assert!(!published.visible);
    assert!(snapshot.window(0).unwrap().frame.is_none());
    assert!(!snapshot.window(0).unwrap().visible);
    harness.pump_frames(15);
    let query = harness
        .world()
        .run_system_once(|state: crate::ecs::state::QueryStateParams| state.extract())
        .unwrap()
        .unwrap();
    assert!(
        query
            .spaces
            .iter()
            .flat_map(|space| &space.windows)
            .find(|window| window.window_id == 0)
            .unwrap()
            .frame
            .is_some(),
        "successful readback restores public geometry"
    );
}

#[test]
fn delayed_geometry_echo_does_not_replace_tiled_layout_intent() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let desired_position = harness.world().get::<Position>(entity).expect("position").0;
    let desired_bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let observed = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("confirmed frame")
        .0;
    let incarnation = harness
        .world()
        .get::<Window>(entity)
        .expect("tracked window")
        .incarnation();

    // Model an AX notification arriving after Spool already read back the
    // geometry write that caused it.
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(2);

    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        desired_position
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        desired_bounds
    );
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(observed));
}

#[test]
fn tiled_resize_burst_defers_neighbour_reflow_until_geometry_settles() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let resized = find_window_entity(0, harness.world());
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");
    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let incarnation = harness
        .world()
        .get::<Window>(resized)
        .expect("resized window")
        .incarnation();

    for width in [500, 600] {
        let observed = IRect::from_corners(
            initial.min,
            initial.min + IVec2::new(width, initial.height()),
        );
        harness.mock_state.os_set_window_frame_silently(0, observed);
        harness
            .world()
            .write_message(crate::events::Event::WindowResized {
                window_id: 0,
                incarnation,
            });
        harness.pump_frames(1);

        assert_eq!(
            harness.mock_state.actual_window_frame(1),
            Some(neighbour_before),
            "a resize burst must not move an adjacent window before the geometry settles"
        );
    }

    harness.pump_frames(2);

    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(1)
            .expect("settled neighbour frame")
            .min
            .x,
        initial.min.x + 600,
        "the quiet period must reflow once from the final observed width"
    );
}

#[test]
fn interleaved_move_and_resize_notifications_commit_one_final_geometry() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let resized = find_window_entity(0, harness.world());
    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");
    let incarnation = harness
        .world()
        .get::<Window>(resized)
        .expect("resized window")
        .incarnation();
    let final_frame = IRect::from_corners(
        initial.min - IVec2::new(80, 0),
        initial.max + IVec2::new(40, 0),
    );

    harness
        .mock_state
        .os_set_window_frame_silently(0, final_frame);
    harness
        .world()
        .write_message(crate::events::Event::WindowMoved {
            window_id: 0,
            incarnation,
        });
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(1);

    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(neighbour_before),
        "move and resize callbacks from one drag must not produce separate reflows"
    );

    harness.pump_frames(2);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(final_frame));
    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(1)
            .expect("settled neighbour")
            .min
            .x,
        neighbour_before.min.x + 40,
        "the neighbour must move once by the final right-edge delta"
    );
}

#[test]
fn left_edge_resize_keeps_the_shared_boundary_stable_until_settle() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let resized = find_window_entity(0, harness.world());
    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");
    let incarnation = harness
        .world()
        .get::<Window>(resized)
        .expect("resized window")
        .incarnation();
    let observed = IRect::from_corners(initial.min - IVec2::new(100, 0), initial.max);

    harness.mock_state.os_set_window_frame_silently(0, observed);
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(1);

    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(neighbour_before),
        "the neighbouring column must not chase a dragged left edge"
    );

    harness.pump_frames(2);

    assert_eq!(harness.mock_state.actual_window_frame(0), Some(observed));
    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(neighbour_before),
        "settling a left-edge resize must preserve the shared right boundary"
    );
}

#[test]
fn top_edge_resize_defers_stacked_neighbour_height_until_settle() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let lower = find_window_entity(1, harness.world());
    {
        let world = harness.world();
        let mut strips =
            world.query_filtered::<&mut LayoutStrip, With<crate::ecs::ActiveWorkspaceMarker>>();
        strips
            .single_mut(world)
            .expect("active strip")
            .stack(lower)
            .expect("stack lower window");
    }
    harness.pump_frames(3);

    let upper_before = harness
        .mock_state
        .actual_window_frame(0)
        .expect("upper frame");
    let lower_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("lower frame");
    let incarnation = harness
        .world()
        .get::<Window>(lower)
        .expect("lower window")
        .incarnation();
    let observed = IRect::from_corners(lower_before.min - IVec2::new(0, 50), lower_before.max);

    harness.mock_state.os_set_window_frame_silently(1, observed);
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 1,
            incarnation,
        });
    harness.pump_frames(1);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(upper_before),
        "the window above must not resize while the shared edge is still moving"
    );

    harness.pump_frames(2);

    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(0)
            .expect("settled upper frame")
            .height(),
        upper_before.height() - 50
    );
    assert_eq!(harness.mock_state.actual_window_frame(1), Some(observed));
}

#[test]
fn sustained_resize_burst_is_not_overwritten_by_the_reconciliation_heartbeat() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let resized = find_window_entity(0, harness.world());
    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");
    let incarnation = harness
        .world()
        .get::<Window>(resized)
        .expect("resized window")
        .incarnation();

    for width in (410..=520).step_by(10) {
        let observed = IRect::from_corners(
            initial.min,
            initial.min + IVec2::new(width, initial.height()),
        );
        harness.mock_state.os_set_window_frame_silently(0, observed);
        harness
            .world()
            .write_message(crate::events::Event::WindowResized {
                window_id: 0,
                incarnation,
            });
        harness.pump_frames(1);

        assert_eq!(harness.mock_state.actual_window_frame(0), Some(observed));
        assert_eq!(
            harness.mock_state.actual_window_frame(1),
            Some(neighbour_before),
            "neither notifications nor the heartbeat may reflow during a sustained gesture"
        );
    }

    harness.pump_frames(2);
    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(1)
            .expect("settled neighbour")
            .min
            .x,
        initial.min.x + 520
    );
}

#[test]
fn paused_mouse_drag_does_not_settle_until_the_button_is_released() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let resized = find_window_entity(0, harness.world());
    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");
    let incarnation = harness
        .world()
        .get::<Window>(resized)
        .expect("resized window")
        .incarnation();
    let holder = harness
        .world()
        .spawn(crate::ecs::MouseHeldMarker(resized))
        .id();
    let observed =
        IRect::from_corners(initial.min, initial.min + IVec2::new(520, initial.height()));

    harness.mock_state.os_set_window_frame_silently(0, observed);
    harness
        .world()
        .write_message(crate::events::Event::WindowResized {
            window_id: 0,
            incarnation,
        });
    harness.pump_frames(5);

    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(neighbour_before),
        "a pause while the pointer button remains held is not the end of the gesture"
    );

    harness.world().entity_mut(holder).despawn();
    harness.pump_frames(2);
    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(1)
            .expect("settled neighbour")
            .min
            .x,
        initial.min.x + 520
    );
}

#[test]
fn mouse_resize_gesture_does_not_cross_a_reused_window_id() {
    let config: Config = (
        MainOptions {
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(30);
    let original = find_window_entity(0, harness.world());
    for x in [300.0, 310.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
        harness.pump_frames(1);
    }
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(0, 20, 400, 600),
    ));
    harness.pump_frames(20);
    let replacement = find_window_entity(0, harness.world());
    assert_ne!(replacement, original);
    let frame = harness.mock_state.actual_window_frame(0).unwrap();
    let writes = harness.mock_state.frame_write_attempts(0);
    harness
        .world()
        .write_message(crate::events::Event::MouseMoved {
            point: objc2_core_foundation::CGPoint::new(320.0, 200.0),
            modifiers: crate::platform::Modifiers::CMD,
        });
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(frame));
    for (x, modifiers) in [
        (330.0, crate::platform::Modifiers::empty()),
        (340.0, crate::platform::Modifiers::CMD),
        (350.0, crate::platform::Modifiers::CMD),
    ] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers,
            });
        harness.pump_frames(1);
    }
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes + 1);
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        frame.width() + 50
    );
}

#[test]
fn mouse_resize_coalesces_one_frame_of_input_into_one_commit() {
    let config: Config = (
        MainOptions {
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(30);
    let initial = harness.mock_state.actual_window_frame(0).unwrap();
    let writes = harness.mock_state.frame_write_attempts(0);
    for x in [300.0, 310.0, 320.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
    }
    harness.pump_frames(1);
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        initial.width() + 100
    );
    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - writes,
        1,
        "one input burst must produce one physical frame commit"
    );
}

#[test]
fn mouse_resize_request_is_discarded_if_space_migration_starts_before_commit() {
    let config: Config = (
        MainOptions {
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let initial = harness.mock_state.actual_window_frame(0).unwrap();
    let writes = harness.mock_state.frame_write_attempts(0);
    harness.app.add_systems(
        PostUpdate,
        (move |mut commands: Commands| {
            crate::ecs::workspace::freeze_window_for_space_reassignment(entity, 0, &mut commands);
        })
        .before(crate::ecs::window_frame::animate_presented_window_frames),
    );
    for x in [300.0, 310.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
    }
    harness.pump_frames(1);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(initial));
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    assert!(
        harness
            .world()
            .get::<crate::ecs::window_frame::InteractiveWindowFrame>(entity)
            .is_none(),
        "a blocked gesture must not replay when membership later resumes"
    );
}

#[test]
fn mouse_resize_failed_readback_preserves_physical_truth_without_settling_intent() {
    let config: Config = (
        MainOptions {
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let initial = harness.mock_state.actual_window_frame(0).unwrap();
    harness.mock_state.fail_frame_write_readbacks(0, 1);
    for x in [300.0, 310.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
    }
    harness.pump_frames(1);
    let physical = harness.mock_state.actual_window_frame(0).unwrap();
    assert_ne!(
        physical, initial,
        "the mock must simulate a partial successful setter"
    );
    assert_eq!(
        harness
            .world()
            .get::<ObservedWindowFrame>(entity)
            .unwrap()
            .0,
        physical
    );
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::DesiredWindowFrame>(entity)
            .unwrap()
            .0,
        initial
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowFrameCommitSuspended>(entity)
            .is_some()
    );
    assert!(
        !harness
            .world()
            .resource::<crate::ecs::window_geometry::WindowGeometrySettling>()
            .contains(entity)
    );
}

#[test]
fn floating_mouse_resize_commits_confirmed_intent_without_replaying_it() {
    let config: Config = (
        MainOptions {
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(30);
    harness
        .world()
        .write_message(crate::events::Event::ActionRequested {
            action: Action::Window(Operation::ToggleFloating),
        });
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    assert!(harness.world().get::<Floating>(entity).is_some());
    let initial = harness.mock_state.actual_window_frame(0).unwrap();
    let writes = harness.mock_state.frame_write_attempts(0);
    for x in [300.0, 310.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
    }
    harness.pump_frames(1);
    let confirmed = harness.mock_state.actual_window_frame(0).unwrap();
    assert_eq!(confirmed.width(), initial.width() + 50);
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::DesiredWindowFrame>(entity)
            .unwrap()
            .0,
        confirmed
    );
    assert_eq!(
        harness
            .world()
            .get::<PresentedWindowFrame>(entity)
            .unwrap()
            .0,
        confirmed
    );
    harness.pump_frames(30);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(confirmed));
    assert_eq!(harness.mock_state.frame_write_attempts(0) - writes, 1);
}

#[test]
fn configured_mouse_resize_updates_the_dragged_window_live_but_defers_its_neighbour() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(10000.0),
            mouse_resize_modifier: Some(crate::platform::Modifiers::CMD),
            preset_column_widths: vec![0.5],
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let initial = harness
        .mock_state
        .actual_window_frame(0)
        .expect("resized frame");
    let neighbour_before = harness
        .mock_state
        .actual_window_frame(1)
        .expect("neighbour frame");

    for x in [300.0, 310.0, 320.0] {
        harness
            .world()
            .write_message(crate::events::Event::MouseMoved {
                point: objc2_core_foundation::CGPoint::new(x, 200.0),
                modifiers: crate::platform::Modifiers::CMD,
            });
        harness.pump_frames(1);
        assert_eq!(
            harness.mock_state.actual_window_frame(1),
            Some(neighbour_before),
            "modifier-driven mouse resize must not animate the adjacent column"
        );
    }

    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(0)
            .expect("live resized frame")
            .width(),
        initial.width() + 100,
        "the window under the pointer must still track the gesture immediately"
    );

    harness.pump_frames(2);
    assert_eq!(
        harness
            .mock_state
            .actual_window_frame(1)
            .expect("settled neighbour")
            .min
            .x,
        initial.min.x + initial.width() + 100
    );
}

#[test]
fn tiled_move_burst_defers_layout_adoption_until_geometry_settles() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);

    let moved = find_window_entity(0, harness.world());
    let initial_position = harness.world().get::<Position>(moved).expect("position").0;
    let initial_frame = harness
        .mock_state
        .actual_window_frame(0)
        .expect("moved frame");
    let incarnation = harness
        .world()
        .get::<Window>(moved)
        .expect("moved window")
        .incarnation();
    let mut final_frame = initial_frame;

    for offset in [IVec2::new(30, 20), IVec2::new(60, 40)] {
        final_frame = IRect::from_corners(initial_frame.min + offset, initial_frame.max + offset);
        harness
            .mock_state
            .os_set_window_frame_silently(0, final_frame);
        harness
            .world()
            .write_message(crate::events::Event::WindowMoved {
                window_id: 0,
                incarnation,
            });
        harness.pump_frames(1);

        assert_eq!(
            harness.world().get::<Position>(moved).expect("position").0,
            initial_position,
            "continuous title-bar movement must not recalculate tiled layout mid-drag"
        );
    }

    harness.pump_frames(2);
    assert_eq!(
        harness.world().get::<Position>(moved).expect("position").0,
        final_frame.min
    );
}

#[test]
fn window_state_sync_stale_ax_inventory_suspends_without_losing_layout_state() {
    let mut harness = TestHarness::new()
        .with_config(reconciliation_fixture_config())
        .with_windows(3);
    harness.pump_frames(10);

    let stale = find_window_entity(1, harness.world());
    harness.mock_state.os_close_window(1);
    // Model both close channels being lost while the AX application inventory
    // still reports the closed window.
    _ = harness.mock_state.drain_events();
    harness.pump_frames(20);

    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.contains(stale)),
        "a pending close must retain declarative layout state until destruction is confirmed"
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(stale)
            .is_some(),
        "the stale AX handle must be excluded from the projected layout"
    );
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        TEST_WINDOW_WIDTH
    );

    harness.mock_state.os_settle_window_list();
    harness.pump_frames(20);
    assert!(harness.world().get_entity(stale).is_err());

    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        99,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(10);

    let newcomer = find_window_entity(99, harness.world());
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.contains(newcomer)),
        "new windows must still enter the tiled strip after stale cleanup"
    );
}

#[test]
fn first_inventory_with_two_incarnations_defers_until_the_id_is_unambiguous() {
    let mut harness = TestHarness::new();
    harness.pump_frames(15);
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    ));
    harness.mock_state.os_stale_window_without_notifications(0);
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(180, 120, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    ));

    harness.pump_frames(20);

    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        0,
        "without incarnation evidence, neither AX candidate may be guessed"
    );

    harness.mock_state.os_settle_window_list();
    harness.pump_frames(20);

    let replacement = find_window_entity(0, harness.world());
    assert!(harness.world().get_entity(replacement).is_ok());
    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        1
    );
}

#[test]
fn one_spawn_batch_never_creates_two_incarnations_for_one_owner_and_id() {
    let mut harness = TestHarness::new();
    harness.pump_frames(15);
    let application = harness
        .world()
        .query_filtered::<Entity, With<Application>>()
        .single(harness.world())
        .expect("application");
    let first = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    let second = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(180, 120, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    assert_ne!(first.incarnation(), second.incarnation());

    harness.world().trigger(SpawnWindowTrigger::for_application(
        application,
        vec![first, second],
    ));
    harness.pump_frames(2);
    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        0,
        "an ambiguous deferred batch must not choose an incarnation"
    );

    let duplicate_a = harness.mock_state.create_window(0);
    let duplicate_b = harness.mock_state.create_window(0);
    assert_eq!(duplicate_a.incarnation(), duplicate_b.incarnation());
    harness.world().trigger(SpawnWindowTrigger::for_application(
        application,
        vec![duplicate_a, duplicate_b],
    ));
    harness.pump_frames(2);
    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        1,
        "exact duplicate candidates must collapse to one entity"
    );
}

#[test]
fn spawn_batch_preserves_candidate_order_across_window_ids() {
    let mut harness = TestHarness::new();
    harness.pump_frames(15);
    let application = harness
        .world()
        .query_filtered::<Entity, With<Application>>()
        .single(harness.world())
        .expect("application");
    let windows = [7, 3, 9]
        .into_iter()
        .map(|id| {
            harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                id,
                IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
            )
        })
        .collect();

    harness
        .world()
        .trigger(SpawnWindowTrigger::for_application(application, windows));
    harness.pump_frames(10);

    assert!(window_x(harness.world(), 7) < window_x(harness.world(), 3));
    assert!(window_x(harness.world(), 3) < window_x(harness.world(), 9));
}

#[test]
fn reused_window_id_replaces_the_old_entity_by_incarnation() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let old_entity = find_window_entity(0, harness.world());
    let old_incarnation = harness
        .world()
        .get::<Window>(old_entity)
        .expect("old window")
        .incarnation();
    let replacement_frame = IRect::from_corners(
        IVec2::new(210, 160),
        IVec2::new(210 + TEST_WINDOW_WIDTH, 160 + TEST_WINDOW_HEIGHT),
    );

    // WindowServer reused the integer ID without delivering the old close or
    // the new create notification.
    drop(
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 0, replacement_frame),
    );
    harness.pump_frames(20);

    assert!(harness.world().get_entity(old_entity).is_err());
    let replacement = find_window_entity(0, harness.world());
    let replacement_incarnation = harness
        .world()
        .get::<Window>(replacement)
        .expect("replacement window")
        .incarnation();
    assert_ne!(replacement, old_entity);
    assert_ne!(replacement_incarnation, old_incarnation);

    // A delayed teardown callback from the old AX element must not redirect
    // through the recycled ID and despawn the replacement.
    harness
        .world()
        .write_message(crate::events::Event::WindowDestroyed {
            window_id: 0,
            source: crate::events::DestroySource::Accessibility,
            incarnation: Some(old_incarnation),
        });
    harness.pump_frames(2);
    assert!(harness.world().get_entity(replacement).is_ok());

    harness
        .world()
        .write_message(crate::events::Event::WindowMinimized {
            window_id: 0,
            incarnation: Some(old_incarnation),
        });
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowVisibility>(replacement)
            .is_none(),
        "a delayed minimize callback from the old AX element must not target the replacement"
    );

    assert_stale_geometry_callbacks_are_ignored(&mut harness, replacement, old_incarnation);
}

#[test]
fn reused_window_id_wins_while_the_old_ax_identity_is_still_stale() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let old_entity = find_window_entity(0, harness.world());
    let old_incarnation = harness
        .world()
        .get::<Window>(old_entity)
        .expect("old window")
        .incarnation();
    harness.mock_state.os_stale_window_without_notifications(0);
    let delayed_stale = harness.mock_state.create_stale_window(0);
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(180, 120, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    ));

    harness.pump_frames(20);

    assert!(harness.world().get_entity(old_entity).is_ok());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(old_entity)
            .is_some(),
        "the tracked incarnation must be isolated while the ID is ambiguous"
    );
    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        1,
        "an ambiguous ID must never create a second ECS window"
    );

    harness.mock_state.os_settle_window_list();
    harness.pump_frames(20);
    assert!(harness.world().get_entity(old_entity).is_err());
    let replacement = find_window_entity(0, harness.world());
    assert_ne!(replacement, old_entity);
    assert_ne!(
        harness
            .world()
            .get::<Window>(replacement)
            .expect("replacement")
            .incarnation(),
        old_incarnation
    );

    let application = harness
        .world()
        .get::<ChildOf>(replacement)
        .expect("replacement owner")
        .parent();
    harness.world().trigger(SpawnWindowTrigger::for_application(
        application,
        vec![delayed_stale],
    ));
    harness.pump_frames(2);
    assert!(harness.world().get_entity(replacement).is_ok());
    let mut windows = harness.world().query::<&Window>();
    assert_eq!(
        windows
            .iter(harness.world())
            .filter(|window| window.id() == 0)
            .count(),
        1,
        "a delayed stale candidate must remain tombstoned"
    );
}

#[test]
fn exact_destroy_tombstones_every_spawn_ingress_for_the_application_lifetime() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let old_entity = find_window_entity(0, harness.world());
    let old_incarnation = harness
        .world()
        .get::<Window>(old_entity)
        .expect("old window")
        .incarnation();
    let application = harness
        .world()
        .get::<ChildOf>(old_entity)
        .expect("old owner")
        .parent();
    harness.mock_state.os_stale_window_without_notifications(0);
    let delayed_stale = harness.mock_state.create_stale_window(0);
    harness
        .world()
        .write_message(crate::events::Event::WindowDestroyed {
            window_id: 0,
            source: crate::events::DestroySource::Accessibility,
            incarnation: Some(old_incarnation),
        });
    harness.pump_frames(2);
    assert!(harness.world().get_entity(old_entity).is_err());

    harness.world().trigger(SpawnWindowTrigger::for_application(
        application,
        vec![delayed_stale],
    ));
    harness.pump_frames(2);
    let mut windows = harness.world().query::<&Window>();
    assert!(windows.iter(harness.world()).all(|window| window.id() != 0));

    harness.mock_state.os_settle_window_list();
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(100, 80, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness.world().trigger(SpawnWindowTrigger::for_application(
        application,
        vec![replacement],
    ));
    harness.pump_frames(5);
    let replacement = find_window_entity(0, harness.world());
    assert_ne!(replacement, old_entity);
    assert_ne!(
        harness
            .world()
            .get::<Window>(replacement)
            .expect("replacement")
            .incarnation(),
        old_incarnation
    );
}

#[test]
fn ignored_window_reusing_an_id_retires_the_old_tiled_entity() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let old_entity = find_window_entity(0, harness.world());
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(100, 100, 300, 200),
    ));
    harness
        .mock_state
        .update_window(0, |window| window.role = "AXUnknown".to_string());

    harness.pump_frames(20);

    assert!(harness.world().get_entity(old_entity).is_err());
    let mut windows = harness.world().query::<&Window>();
    assert!(windows.iter(harness.world()).all(|window| window.id() != 0));
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .all(|strip| !strip.contains(old_entity))
    );
}

#[test]
fn failed_ax_inventory_suspends_window_server_missing_windows() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);

    let entities = [
        find_window_entity(0, harness.world()),
        find_window_entity(1, harness.world()),
    ];
    harness.mock_state.os_vanish_window(0);
    harness.mock_state.os_vanish_window(1);
    harness
        .mock_state
        .fail_application_inventory_attempts(TEST_PROCESS_ID, 10);

    harness.pump_frames(20);

    for entity in entities {
        assert!(
            harness.world().get_entity(entity).is_ok(),
            "one failed AX inventory must not be treated as definitive destruction"
        );
    }
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| entities.iter().all(|entity| strip.contains(*entity))),
        "an incomplete destruction decision must retain the declarative layout"
    );
    for entity in entities {
        assert!(
            harness
                .world()
                .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
                .is_some(),
            "WindowServer-missing windows must be excluded from the projected layout"
        );
    }

    harness
        .mock_state
        .fail_application_inventory_attempts(TEST_PROCESS_ID, 0);
    harness.pump_frames(20);
    for entity in entities {
        assert!(harness.world().get_entity(entity).is_err());
    }
}

#[test]
fn partial_ax_inventory_discovers_known_windows_without_destroying_unknown_absence() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let vanished = find_window_entity(0, harness.world());
    harness
        .mock_state
        .set_application_inventory_complete(TEST_PROCESS_ID, false);
    harness.mock_state.os_vanish_window(0);
    drop(harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        10,
        IRect::new(20, 20, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    ));

    harness.pump_frames(20);

    assert!(
        harness.world().get_entity(vanished).is_ok(),
        "an incomplete AX snapshot must not prove an omitted window was destroyed"
    );
    let discovered = find_window_entity(10, harness.world());
    assert!(harness.world().get_entity(discovered).is_ok());

    harness
        .mock_state
        .set_application_inventory_complete(TEST_PROCESS_ID, true);
    harness.pump_frames(20);
    assert!(harness.world().get_entity(vanished).is_err());
    assert!(harness.world().get_entity(discovered).is_ok());
}

#[test]
fn incomplete_ax_inventory_omission_keeps_a_live_window_tiled() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    harness
        .mock_state
        .omit_window_from_application_inventory(TEST_PROCESS_ID, 0, true);
    harness.pump_frames(30);

    assert!(harness.world().get_entity(entity).is_ok());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_none(),
        "an unidentified AX element with a live WindowServer surface must fail open"
    );
    let mut strips = harness.world().query::<&LayoutStrip>();
    assert!(
        strips
            .iter(harness.world())
            .any(|strip| strip.contains(entity)),
        "a partial identity omission must not make a live window suddenly untile"
    );

    harness
        .mock_state
        .omit_window_from_application_inventory(TEST_PROCESS_ID, 0, false);
    harness.pump_frames(20);
    assert!(harness.world().get_entity(entity).is_ok());
}

#[test]
fn inactive_ordered_out_surface_preserves_its_layout_projection() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(20);
    let entity = find_window_entity(1, harness.world());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID + 1, false);
    harness.mock_state.update_window(1, |window| {
        window.visible = false;
        window.ordered_out = true;
    });
    harness.mock_state.os_withdraw_window(1);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(20);
    let unavailable = harness
        .world()
        .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
        .unwrap();
    assert!(
        !unavailable.excludes_from_layout_projection(),
        "order-out on an inactive Space is not proof of a closed presentation"
    );
    assert_eq!(layout_window_ids(harness.world()), vec![0, 1, 2]);
}

#[test]
fn space_change_ax_withdrawal_preserves_layout_order() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(15);

    harness.mock_state.focus_window(1);
    harness.pump_frames(5);
    harness
        .world()
        .write_message(crate::events::Event::ActionRequested {
            action: Action::Window(Operation::Move(Direction::West)),
        });
    harness.pump_frames(5);
    assert_eq!(layout_window_ids(harness.world()), vec![1, 0, 2]);

    for id in 0..3 {
        harness.mock_state.os_withdraw_window(id);
    }
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);

    assert_eq!(
        layout_window_ids(harness.world()),
        vec![1, 0, 2],
        "a Space observation must not detach live WindowServer surfaces from LayoutStrip"
    );
    for id in 0..3 {
        let entity = find_window_entity(id, harness.world());
        assert!(
            harness
                .world()
                .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
                .is_some(),
            "AX-withdrawn window {id} must pause macOS operations while retaining layout state"
        );
    }

    harness.mock_state.os_restore_withdrawn_window(0);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);

    assert_eq!(layout_window_ids(harness.world()), vec![1, 0, 2]);
    assert_window_suspended(harness.world(), 0, false);
    assert_window_suspended(harness.world(), 1, true);
    assert_window_suspended(harness.world(), 2, true);

    harness.mock_state.os_restore_withdrawn_window(1);
    harness.mock_state.os_restore_withdrawn_window(2);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(12);

    assert_eq!(layout_window_ids(harness.world()), vec![1, 0, 2]);
    for id in 0..3 {
        assert_window_suspended(harness.world(), id, false);
    }
}

#[test]
fn confirmed_close_removes_only_the_destroyed_layout_member() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(15);

    harness.mock_state.focus_window(1);
    harness.pump_frames(5);
    harness
        .world()
        .write_message(crate::events::Event::ActionRequested {
            action: Action::Window(Operation::Move(Direction::West)),
        });
    harness.pump_frames(5);
    assert_eq!(layout_window_ids(harness.world()), vec![1, 0, 2]);

    let closed = find_window_entity(0, harness.world());
    harness.mock_state.os_withdraw_window(0);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);
    assert_eq!(layout_window_ids(harness.world()), vec![1, 0, 2]);
    assert_window_suspended(harness.world(), 0, true);

    harness.mock_state.os_settle_withdrawn_surface(0);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);

    assert!(harness.world().get_entity(closed).is_err());
    assert_eq!(layout_window_ids(harness.world()), vec![1, 2]);
}

#[test]
fn transient_window_server_omission_reflows_available_stack_members_and_recovers() {
    use crate::ecs::LayoutPosition;
    for missing in 0..3 {
        let missing_index = usize::try_from(missing).unwrap();
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let entities = [0, 1, 2].map(|id| find_window_entity(id, harness.world()));
        let world = harness.world();
        let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
        strip.stack(entities[1]).unwrap();
        strip.stack(entities[2]).unwrap();
        harness.pump_frames(5);
        let original = layout_window_ids(harness.world());
        let unavailable_bounds = harness
            .world()
            .get::<Bounds>(entities[missing_index])
            .unwrap()
            .0;
        harness
            .mock_state
            .omit_window_from_window_server_inventory(missing, true);
        harness
            .world()
            .write_message(crate::events::Event::SpaceChanged);
        harness.pump_frames(2);
        assert_window_suspended(harness.world(), missing, true);
        assert_eq!(layout_window_ids(harness.world()), original);
        assert_eq!(
            harness
                .world()
                .get::<Bounds>(entities[missing_index])
                .unwrap()
                .0,
            unavailable_bounds
        );
        let mut bottom = 0;
        for (id, entity) in entities.into_iter().enumerate() {
            if id == missing_index {
                continue;
            }
            assert_eq!(
                harness.world().get::<LayoutPosition>(entity).unwrap().0.y,
                bottom
            );
            bottom += harness.world().get::<Bounds>(entity).unwrap().0.y;
        }
        assert_eq!(bottom, TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT);

        harness
            .mock_state
            .omit_window_from_window_server_inventory(missing, false);
        harness
            .world()
            .write_message(crate::events::Event::SpaceChanged);
        harness.pump_frames(2);
        assert_window_suspended(harness.world(), missing, false);
        assert_eq!(layout_window_ids(harness.world()), original);
        let mut bottom = 0;
        for entity in entities {
            assert_eq!(
                harness.world().get::<LayoutPosition>(entity).unwrap().0.y,
                bottom
            );
            bottom += harness.world().get::<Bounds>(entity).unwrap().0.y;
        }
        assert_eq!(bottom, TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT);
    }
}

#[test]
fn transient_window_server_omission_restores_projection_without_mutating_layout() {
    let mut harness = TestHarness::new()
        .with_config(reconciliation_fixture_config())
        .with_windows(3);
    harness.pump_frames(15);

    harness
        .mock_state
        .omit_window_from_window_server_inventory(1, true);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);

    assert_eq!(layout_window_ids(harness.world()), vec![0, 1, 2]);
    assert_window_suspended(harness.world(), 1, true);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        TEST_WINDOW_WIDTH,
        "a missing surface must be excluded from the rendered layout while confirmation is pending"
    );

    harness
        .mock_state
        .omit_window_from_window_server_inventory(1, false);
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);

    assert_eq!(layout_window_ids(harness.world()), vec![0, 1, 2]);
    assert_window_suspended(harness.world(), 1, false);
    assert_eq!(
        window_x(harness.world(), 2) - window_x(harness.world(), 0),
        TEST_WINDOW_WIDTH * 2,
        "the original projection must return when WindowServer confirms the surface again"
    );
}

#[test]
fn space_change_ax_withdrawal_preserves_stack_and_tab_shape() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(15);

    let tab_leader = find_window_entity(0, harness.world());
    let tab_follower = find_window_entity(1, harness.world());
    let stacked = find_window_entity(2, harness.world());
    {
        let mut strips = harness.world().query::<&mut LayoutStrip>();
        let mut strip = strips
            .iter_mut(harness.world())
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .expect("test workspace layout strip");
        strip
            .convert_to_tabs(tab_leader, tab_follower)
            .expect("build native-tab fixture");
        strip.stack(stacked).expect("build stack fixture");
    }
    harness.pump_frames(2);
    let expected = layout_description(harness.world());

    for id in 0..4 {
        harness.mock_state.os_withdraw_window(id);
    }
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);
    assert_eq!(layout_description(harness.world()), expected);

    for id in 0..4 {
        harness.mock_state.os_restore_withdrawn_window(id);
    }
    harness
        .world()
        .write_message(crate::events::Event::SpaceChanged);
    harness.pump_frames(2);
    assert_eq!(layout_description(harness.world()), expected);
}

#[test]
fn window_state_sync_inventory_failure_is_fail_open_then_recovers() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);

    let vanished = find_window_entity(1, harness.world());
    harness
        .mock_state
        .set_window_server_inventory_available(false);
    harness.mock_state.os_vanish_window(1);
    harness.pump_frames(30);

    assert!(
        harness.world().get_entity(vanished).is_ok(),
        "one unavailable inventory must not be interpreted as absence"
    );

    harness
        .mock_state
        .set_window_server_inventory_available(true);
    harness.pump_frames(20);
    assert!(
        harness.world().get_entity(vanished).is_err(),
        "the next complete audit must converge the deferred close"
    );
}

#[test]
fn window_state_sync_retries_partial_window_observer_registration() {
    let mut harness = TestHarness::new();
    harness.mock_state.fail_window_observer_attempts(7, 2);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        7,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));

    harness.pump_frames(30);

    assert_eq!(
        harness.mock_state.window_observer_attempts(7),
        3,
        "a partial AX registration must retry until every notification is installed"
    );
}

#[test]
fn grid_defaults_use_the_owning_display_origin_and_extent() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config).with_display(
        EXT_DISPLAY_ID,
        IRect::new(1200, 200, 2800, 1100),
        vec![EXT_WORKSPACE_ID],
    );
    harness.pump_frames(30);
    let expected = {
        let world = harness.world();
        let config = world.resource::<Config>().clone();
        world
            .query::<(&crate::manager::Display, Option<&crate::ecs::DockPosition>)>()
            .iter(world)
            .find(|(display, _)| display.id() == EXT_DISPLAY_ID)
            .map(|(display, dock)| display.actual_display_bounds(dock, &config))
            .unwrap()
    };
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        EXT_WORKSPACE_ID,
        9,
        IRect::new(1300, 300, 1700, 700),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(3);
    assert_eq!(harness.mock_state.actual_window_frame(9), Some(expected));
    assert_eq!(harness.mock_state.frame_write_attempts(9), 1);
}

#[test]
fn configured_width_uses_the_owning_display_during_startup() {
    let mut params = WindowParams::new(".*", None);
    params.width = Some(0.5);
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(1200, 200, 2800, 1100),
            vec![EXT_WORKSPACE_ID],
        )
        .with_workspace_window(9, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(30);
    let entity = find_window_entity(9, harness.world());
    assert_eq!(harness.world().get::<Bounds>(entity).unwrap().0.x, 800);
}

#[test]
fn grid_defaults_wait_for_unambiguous_membership_then_retry() {
    for ambiguous in [false, true] {
        let mut params = WindowParams::new(".*", None);
        params.floating = Some(true);
        params.grid = Some("1:1:0:0:1:1".to_string());
        let config: Config = (MainOptions::default(), vec![params]).into();
        let mut harness = TestHarness::new().with_config(config).with_display(
            EXT_DISPLAY_ID,
            IRect::new(1200, 200, 2800, 1100),
            vec![EXT_WORKSPACE_ID],
        );
        harness.pump_frames(30);
        let initial = IRect::new(1300, 300, 1700, 700);
        let window = harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 9, initial);
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.mock_state.script_workspace_membership_queries(
            TEST_WORKSPACE_ID,
            std::iter::repeat_n(if ambiguous { Ok(vec![9]) } else { Err(()) }, 100),
        );
        harness.pump_frames(2);
        let entity = find_window_entity(9, harness.world());
        assert_eq!(harness.mock_state.frame_write_attempts(9), 0);
        assert_eq!(harness.mock_state.actual_window_frame(9), Some(initial));
        assert!(
            harness
                .world()
                .get::<WindowDefaultsPending>(entity)
                .is_some()
        );
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID, []);
        harness.pump_frames(3);
        assert!(
            harness
                .world()
                .get::<WindowDefaultsPending>(entity)
                .is_none()
        );
        assert_eq!(harness.mock_state.frame_write_attempts(9), 1);
    }
}

#[test]
fn default_failures_are_bounded_across_heartbeats_and_recover_after_cooldown() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(30);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.reject_frame_writes(9, true);
    for _ in 0..40 {
        harness
            .world()
            .write_message(crate::events::Event::ReconcileWindows {
                scope: crate::events::ReconcileScope::All,
            });
        harness.pump_frames(1);
    }
    assert_eq!(
        harness.mock_state.frame_write_attempts(9),
        3,
        "event and topology heartbeats must not restart failed initialization every frame"
    );
    let entity = find_window_entity(9, harness.world());
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some()
    );
    harness.mock_state.reject_frame_writes(9, false);
    harness.pump_frames(30);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(9), 4);
}

#[test]
fn changed_default_context_restarts_a_cooling_transaction() {
    for change_config in [true, false] {
        let mut params = WindowParams::new(".*", None);
        params.floating = Some(true);
        params.grid = Some("1:1:0:0:1:1".to_string());
        let config: Config = (MainOptions::default(), vec![params.clone()]).into();
        let mut harness = TestHarness::new().with_config(config);
        harness.pump_frames(30);
        let window = harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            9,
            IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
        );
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.mock_state.reject_frame_writes(9, true);
        harness.pump_frames(4);
        assert_eq!(harness.mock_state.frame_write_attempts(9), 3);
        harness.mock_state.reject_frame_writes(9, false);
        let expected_width = if change_config {
            params.grid = Some("2:1:0:0:1:1".to_string());
            let config: Config = (MainOptions::default(), vec![params]).into();
            harness.world().insert_resource(config);
            TEST_DISPLAY_WIDTH / 2
        } else {
            harness.mock_state.add_display(
                TEST_DISPLAY_ID,
                IRect::new(500, 0, 1900, 900),
                vec![TEST_WORKSPACE_ID],
            );
            harness
                .world()
                .write_message(crate::events::Event::DisplayConfigured {
                    display_id: TEST_DISPLAY_ID,
                });
            1400
        };
        harness.pump_frames(2);
        let entity = find_window_entity(9, harness.world());
        assert!(
            harness
                .world()
                .get::<WindowDefaultsPending>(entity)
                .is_none()
        );
        assert_eq!(harness.mock_state.frame_write_attempts(9), 4);
        assert_eq!(
            harness.mock_state.actual_window_frame(9).unwrap().width(),
            expected_width
        );
    }
}

#[test]
fn default_read_failures_also_obey_the_retry_budget() {
    let mut params = WindowParams::new(".*", None);
    params.width = Some(0.5);
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(30);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.fail_frame_updates(9, 4);
    harness.pump_frames(40);
    let entity = find_window_entity(9, harness.world());
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(9), 0);
    harness.pump_frames(30);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert!(harness.mock_state.frame_write_attempts(9) > 0);
}

#[test]
fn changing_partial_default_writes_do_not_restart_the_budget() {
    let mut params = WindowParams::new(".*", None);
    // Tiled width rules are now column intent, not native default writes.
    // A floating grid still exercises the complete native defaults transaction.
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(30);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.fail_frame_write_readbacks(9, 100);
    for offset in 0..20 {
        harness
            .mock_state
            .os_set_window_frame_silently(9, IRect::new(offset, 20, offset + 400, 500));
        harness.pump_frames(1);
    }
    assert_eq!(
        harness.mock_state.frame_write_attempts(9),
        3,
        "changing physical origins must not create fresh initialization budgets"
    );
}

#[test]
fn resuming_one_default_transaction_preserves_another_windows_cooldown() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(30);
    for id in [9, 10] {
        let window = harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            id,
            IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
        );
        harness
            .world()
            .trigger(SpawnWindowTrigger::new(vec![window]));
        harness.mock_state.reject_frame_writes(id, true);
    }
    harness.pump_frames(4);
    assert_eq!(harness.mock_state.frame_write_attempts(9), 3);
    assert_eq!(harness.mock_state.frame_write_attempts(10), 3);
    harness.mock_state.os_withdraw_window(9);
    harness.pump_frames(15);
    let entity = find_window_entity(9, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some()
    );
    harness.mock_state.reject_frame_writes(9, false);
    harness.mock_state.os_restore_withdrawn_window(9);
    harness.pump_frames(15);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(9), 4);
    assert_eq!(harness.mock_state.frame_write_attempts(10), 3);
}

#[test]
fn window_defaults_retry_after_a_transient_geometry_failure() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(15);

    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.fail_frame_write_readbacks(9, 1);

    harness.pump_frames(1);
    let entity = find_window_entity(9, harness.world());
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some()
    );
    assert!(harness.world().get::<Floating>(entity).is_none());
    let failed_attempts = harness.mock_state.frame_write_attempts(9);
    assert!(failed_attempts >= 1);

    harness.pump_frames(1);
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert!(harness.world().get::<Floating>(entity).is_some());
    assert!(harness.mock_state.frame_write_attempts(9) > failed_attempts);
    let observed = harness
        .world()
        .get::<ObservedWindowFrame>(entity)
        .expect("confirmed default frame")
        .0;
    assert_eq!(harness.mock_state.actual_window_frame(9), Some(observed));
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        observed.min
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        observed.size()
    );
}

#[test]
fn isolated_pending_defaults_pause_ax_attempts_and_resume_after_restore() {
    let mut params = WindowParams::new(".*", None);
    params.floating = Some(true);
    params.grid = Some("1:1:0:0:1:1".to_string());
    let config: Config = (MainOptions::default(), vec![params]).into();
    let mut harness = TestHarness::new().with_config(config);
    harness.pump_frames(15);

    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.mock_state.fail_frame_write_readbacks(9, 20);
    harness.pump_frames(1);

    let entity = find_window_entity(9, harness.world());
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some()
    );
    harness.mock_state.os_withdraw_window(9);
    harness.pump_frames(20);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some()
    );
    let isolated_attempts = harness.mock_state.frame_write_attempts(9);

    harness.pump_frames(20);
    assert_eq!(
        harness.mock_state.frame_write_attempts(9),
        isolated_attempts,
        "isolated pending defaults must not call the dead AX element every update"
    );
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_some(),
        "isolation must preserve pending defaults for a later restore"
    );

    harness.mock_state.fail_frame_write_readbacks(9, 0);
    harness.mock_state.os_restore_withdrawn_window(9);
    harness.pump_frames(20);

    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_none()
    );
    assert!(
        harness
            .world()
            .get::<WindowDefaultsPending>(entity)
            .is_none()
    );
    assert!(harness.world().get::<Floating>(entity).is_some());
    assert!(harness.mock_state.frame_write_attempts(9) > isolated_attempts);
}

#[test]
fn window_state_sync_bounds_retries_for_a_constrained_tiled_window() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let desired_position = harness.world().get::<Position>(entity).expect("position").0;
    let desired_bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let drifted = IRect::from_corners(
        desired_position + IVec2::new(80, 50),
        desired_position + desired_bounds + IVec2::new(180, 150),
    );
    harness.mock_state.os_set_window_frame_silently(0, drifted);
    harness.mock_state.constrain_frame_writes(0, true);
    let old_target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    harness.world().entity_mut(entity).insert((
        crate::ecs::DesiredWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
        PresentedWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
    ));
    let baseline_attempts = harness.mock_state.frame_write_attempts(0);

    harness.pump_frames(45);

    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - baseline_attempts,
        1
    );
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(drifted));
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        desired_position
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        desired_bounds
    );

    // Unknown competing readback is not a proven minimum-size constraint.
    // Merely waiting or making the mock writable does not grant another round.
    harness.mock_state.constrain_frame_writes(0, false);
    harness.pump_frames(60);
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(drifted));
    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - baseline_attempts,
        1
    );
}

#[test]
fn unrelated_window_events_do_not_reset_frame_retry_budget() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let position = harness.world().get::<Position>(entity).expect("position").0;
    let bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let drifted = IRect::from_corners(
        position + IVec2::new(50, 30),
        position + bounds + IVec2::new(150, 130),
    );
    harness.mock_state.os_set_window_frame_silently(0, drifted);
    harness.mock_state.constrain_frame_writes(0, true);
    let old_target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    harness.world().entity_mut(entity).insert((
        crate::ecs::DesiredWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
        PresentedWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
    ));
    let baseline_attempts = harness.mock_state.frame_write_attempts(0);
    let other_entity = find_window_entity(1, harness.world());
    let other_incarnation = harness
        .world()
        .get::<Window>(other_entity)
        .expect("tracked window")
        .incarnation();

    for _ in 0..4 {
        harness
            .world()
            .write_message(crate::events::Event::WindowMoved {
                window_id: 1,
                incarnation: other_incarnation,
            });
        harness.pump_frames(1);
        harness.pump_frames(9);
    }

    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - baseline_attempts,
        1,
        "activity from another window must not turn bounded retries into an unbounded loop"
    );
}

#[test]
fn changing_constrained_readbacks_do_not_reset_frame_retry_budget() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let position = harness.world().get::<Position>(entity).expect("position").0;
    let bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let drifted = IRect::from_corners(
        position + IVec2::new(80, 50),
        position + bounds + IVec2::new(180, 150),
    );
    harness.mock_state.os_set_window_frame_silently(0, drifted);
    harness.mock_state.progress_frame_writes(0, true);
    let old_target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    harness.world().entity_mut(entity).insert((
        crate::ecs::DesiredWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
        PresentedWindowFrame(IRect::from_corners(
            old_target.min,
            old_target.max + IVec2::new(20, 0),
        )),
    ));
    let baseline_attempts = harness.mock_state.frame_write_attempts(0);

    harness.pump_frames(45);

    assert_eq!(
        harness.mock_state.frame_write_attempts(0) - baseline_attempts,
        1,
        "changing partial readbacks for one target must still share one bounded retry budget"
    );
    assert_ne!(
        harness.mock_state.actual_window_frame(0),
        Some(drifted),
        "the mock must return a different observed frame after every write"
    );
    assert_eq!(
        harness.world().get::<Position>(entity).expect("position").0,
        position
    );
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        bounds
    );
}

#[test]
fn failed_ax_writes_stop_the_animation_commit_loop_and_preserve_readback() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(32.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(2);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let original_bounds = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let target = original_bounds + IVec2::new(240, 0);

    // AX setters are not transactional: the frame can change even when the
    // final readback reports an error. Repeating every intermediate animation
    // frame then walks the window through a series of unintended geometries.
    harness.mock_state.reject_frame_writes(0, true);
    let baseline_attempts = harness.mock_state.frame_write_attempts(0);
    set_column_width_intent(&mut harness, entity, target.x);

    harness.pump_frames(50);

    let attempts = harness.mock_state.frame_write_attempts(0) - baseline_attempts;
    assert!(
        attempts == 3,
        "the initial animation and retries share a three-attempt budget; got {attempts} writes"
    );
    assert!(
        harness.world().get::<ObservedWindowFrame>(entity).is_some(),
        "a successful read after a failed AX setter must remain the physical source of truth"
    );
}

#[test]
fn suspended_animation_commit_recovers_through_bounded_reconciliation() {
    let config: Config = (
        MainOptions {
            animation_speed: Some(32.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let original = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let target = original + IVec2::new(240, 0);
    harness.mock_state.reject_frame_writes(0, true);
    set_column_width_intent(&mut harness, entity, target.x);
    harness.pump_frames(1);

    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowFrameCommitSuspended>(entity)
            .is_some(),
        "a failed animation commit must remain suspended between retry bursts"
    );

    harness.mock_state.reject_frame_writes(0, false);
    harness.pump_frames(70);

    let desired = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .expect("desired frame")
        .0;
    assert_eq!(harness.mock_state.actual_window_frame(0), Some(desired));
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowFrameCommitSuspended>(entity)
            .is_none(),
        "successful bounded reconciliation must resume normal projection"
    );
}

#[test]
fn external_frame_recovery_resumes_presentation_without_another_write() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(30);
    let entity = find_window_entity(0, harness.world());
    let target = harness
        .world()
        .get::<crate::ecs::DesiredWindowFrame>(entity)
        .unwrap()
        .0;
    let drift = IRect::from_corners(
        target.min + IVec2::splat(80),
        target.max + IVec2::splat(180),
    );
    harness.mock_state.os_set_window_frame_silently(0, drift);
    harness.mock_state.constrain_frame_writes(0, true);
    harness.pump_frames(40);
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowFrameCommitSuspended>(entity)
            .is_some()
    );
    let attempts = harness.mock_state.frame_write_attempts(0);
    harness.mock_state.os_set_window_frame_silently(0, target);
    harness
        .world()
        .write_message(crate::events::Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::All,
        });
    harness.pump_frames(1);
    assert_eq!(
        harness
            .world()
            .get::<PresentedWindowFrame>(entity)
            .unwrap()
            .0,
        target
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::WindowFrameCommitSuspended>(entity)
            .is_none()
    );
    assert_eq!(harness.mock_state.frame_write_attempts(0), attempts);
}

/// A width edit the display never realized follows the display once its round is
/// over: the authored column intent becomes what the window actually shows, so
/// state and display agree again (ADR 0011). Before that decision the edited
/// target was kept forever and only reported as blocked.
#[test]
fn a_constrained_width_edit_aligns_to_what_the_window_shows() {
    use crate::ecs::alignment::{AlignmentReason, RealizationAlignments};
    use crate::ecs::layout::WidthIntent;

    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let original = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let target = original + IVec2::new(240, 0);
    // The window accepts the write and stays where it is: the display cannot
    // reach the edited width, which is what a native minimum size looks like.
    harness.mock_state.constrain_frame_writes(0, true);
    set_column_width_intent(&mut harness, entity, target.x);
    // The projection publishes the edit, and the layout target stays the edited
    // value through the readback grace period: alignment waits it out.
    harness.pump_frames(2);
    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        target,
        "the edit is the layout target while it is still being tried"
    );

    harness.pump_frames(60);

    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        original,
        "the authored width follows the display once the round is over"
    );
    let width = {
        let mut strips = harness.world().query::<&LayoutStrip>();
        let strip = strips
            .iter(harness.world())
            .find(|strip| strip.column_id(entity).is_some())
            .expect("the window still has a column");
        let id = strip.column_id(entity).expect("column");
        strip
            .column_states()
            .find(|state| state.id == id)
            .expect("column state")
            .width
    };
    assert_eq!(
        width,
        WidthIntent::Absolute(f64::from(original.x)),
        "the column intent is the displayed width, not the unrealized edit"
    );
    let records = {
        let alignments = harness.world().resource::<RealizationAlignments>();
        alignments
            .records(entity)
            .map(|record| record.reason)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        records,
        vec![AlignmentReason::NotRealizedWithinGrace],
        "one alignment is recorded, and re-aligning the same value stops"
    );
    let attempts = harness.mock_state.frame_write_attempts(0);
    harness.pump_frames(40);
    assert_eq!(
        harness.mock_state.frame_write_attempts(0),
        attempts,
        "the aligned value needs no further writes: state and display already agree"
    );
}

/// A write the platform answers as invalid is a definitive refusal: the authored
/// width follows the display at once, without waiting out the readback grace
/// period, and the record says which error answered (ADR 0011).
#[test]
fn a_refused_width_write_aligns_to_what_the_window_shows_immediately() {
    use crate::ecs::alignment::{AlignmentReason, RealizationAlignments};
    use crate::ecs::layout::WidthIntent;

    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);

    let entity = find_window_entity(0, harness.world());
    let original = harness.world().get::<Bounds>(entity).expect("bounds").0;
    let target = original + IVec2::new(240, 0);
    let code = accessibility_sys::kAXErrorIllegalArgument;
    harness
        .mock_state
        .refuse_frame_writes_with_code(0, Some(code));
    set_column_width_intent(&mut harness, entity, target.x);
    harness.pump_frames(5);

    assert_eq!(
        harness.world().get::<Bounds>(entity).expect("bounds").0,
        original,
        "a refused write aligns the authored width to the display without a grace period"
    );
    let width = {
        let mut strips = harness.world().query::<&LayoutStrip>();
        let strip = strips
            .iter(harness.world())
            .find(|strip| strip.column_id(entity).is_some())
            .expect("the window still has a column");
        let id = strip.column_id(entity).expect("column");
        strip
            .column_states()
            .find(|state| state.id == id)
            .expect("column state")
            .width
    };
    assert_eq!(width, WidthIntent::Absolute(f64::from(original.x)));
    let records = {
        let alignments = harness.world().resource::<RealizationAlignments>();
        alignments
            .records(entity)
            .map(|record| record.reason)
            .collect::<Vec<_>>()
    };
    assert_eq!(records, vec![AlignmentReason::WriteRefused { code }]);
    assert!(
        harness
            .world()
            .get::<crate::ecs::alignment::FrameWriteRefused>(entity)
            .is_none(),
        "the refusal is consumed once it has been answered"
    );
}

#[test]
fn window_state_sync_retries_window_server_notification_subscription() {
    let mut harness = TestHarness::new();
    harness.mock_state.fail_notification_requests(1);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        8,
        IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));

    harness.pump_frames(60);

    assert_eq!(
        harness.mock_state.notification_request_attempts(),
        2,
        "a failed WindowServer close subscription must be retried"
    );
}

fn window_x(world: &mut World, id: i32) -> i32 {
    let entity = find_window_entity(id, world);
    world.get::<Position>(entity).expect("window position").0.x
}

fn layout_window_ids(world: &mut World) -> Vec<i32> {
    let entities = {
        let mut strips = world.query::<&LayoutStrip>();
        strips
            .iter(world)
            .find(|strip| strip.id() == TEST_WORKSPACE_ID)
            .expect("test workspace layout strip")
            .all_windows()
    };
    entities
        .into_iter()
        .map(|entity| world.get::<Window>(entity).expect("tracked window").id())
        .collect()
}

fn layout_description(world: &mut World) -> String {
    let mut strips = world.query::<&LayoutStrip>();
    strips
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .map(ToString::to_string)
        .expect("test workspace layout strip")
}

fn assert_window_suspended(world: &mut World, id: i32, expected: bool) {
    let entity = find_window_entity(id, world);
    assert_eq!(
        world
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some(),
        expected,
        "window {id} suspension state"
    );
}

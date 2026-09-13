use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use super::*;
use crate::commands::{Action, MoveFocus};
use crate::config::{Config, MainOptions};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{self, VisibleNativeSpaceMarker};
use crate::ecs::{Floating, SpawnCommandsExt, WindowVisibility};
use crate::events::Event;
use crate::platform::WinID;

const TARGET: WorkspaceId = TEST_WORKSPACE_ID + 1;

fn focus_harness() -> TestHarness {
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET],
        )
        .with_windows(1)
        .with_workspace_window(1, TARGET, |_| {});
    harness.pump_frames(20);
    harness.mock_state.take_focus_requests();
    harness
}

fn run_commands(harness: &mut TestHarness, actions: impl IntoIterator<Item = Action>) {
    for action in actions {
        harness
            .world()
            .write_message(Event::action_requested(action));
    }
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
}

fn focus(harness: &mut TestHarness, window_id: WinID) {
    run_commands(harness, [Action::FocusWindow { window_id }]);
}

fn enable_space_control(harness: &mut TestHarness) {
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            ..Default::default()
        },
        vec![],
    )
        .into();
    harness.world().insert_resource(config);
    harness.mock_state.enable_native_space_control();
}

/// A native read that keeps failing, in both answers a focus command may ask
/// for.
///
/// A Space the retained layout places the window in is read once to confirm
/// that claim, and once more by the full observation the command falls back to
/// when the claim is not confirmed. A read that must refuse therefore has to
/// answer the same way both times.
fn always_unavailable<T>() -> [std::result::Result<T, ()>; 2] {
    [Err(()), Err(())]
}

fn multi_display_harness() -> TestHarness {
    let mut harness = focus_harness()
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
        .with_workspace_window(2, EXT_WORKSPACE_ID, |_| {});
    harness.pump_frames(10);
    harness.mock_state.take_focus_requests();
    harness
}

fn source_strip(harness: &mut TestHarness) -> Entity {
    let world = harness.world();
    world
        .query::<(Entity, &LayoutStrip)>()
        .iter(world)
        .find(|(_, strip)| strip.id() == TEST_WORKSPACE_ID)
        .unwrap()
        .0
}

#[test]
fn native_focus_refreshes_visibility_instead_of_trusting_a_retained_marker() {
    for read_fails in [false, true] {
        let mut harness = focus_harness();
        let source = source_strip(&mut harness);
        assert!(
            harness
                .world()
                .get::<VisibleNativeSpaceMarker>(source)
                .is_some()
        );
        if read_fails {
            harness
                .mock_state
                .script_active_space_queries(TEST_DISPLAY_ID, always_unavailable());
        } else {
            harness
                .mock_state
                .activate_workspace(TEST_DISPLAY_ID, TARGET, false);
        }
        focus(&mut harness, 0);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "an old visible marker cannot authorize a new focus request"
        );
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
        focus(&mut harness, 0);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    }
}

#[derive(Clone, Copy, Debug)]
enum DetachedKind {
    Floating,
    Hidden,
    Minimized,
    Unplaced,
}

fn detach(harness: &mut TestHarness, kind: DetachedKind) {
    let entity = find_window_entity(0, harness.world());
    match kind {
        DetachedKind::Floating => {
            harness.world().entity_mut(entity).insert(Floating);
        }
        DetachedKind::Hidden => {
            harness
                .world()
                .entity_mut(entity)
                .insert(WindowVisibility::Hidden);
        }
        DetachedKind::Minimized => {
            harness
                .world()
                .entity_mut(entity)
                .insert(WindowVisibility::Minimized);
        }
        DetachedKind::Unplaced => {
            let source = source_strip(harness);
            harness
                .world()
                .get_mut::<LayoutStrip>(source)
                .unwrap()
                .remove(entity);
        }
    }
}

#[test]
fn native_focus_cannot_bypass_space_visibility_when_a_window_is_outside_layout() {
    for kind in [
        DetachedKind::Floating,
        DetachedKind::Hidden,
        DetachedKind::Minimized,
        DetachedKind::Unplaced,
    ] {
        let mut harness = focus_harness();
        detach(&mut harness, kind);
        harness
            .mock_state
            .update_window(0, |window| window.workspace_id = TARGET);
        focus(&mut harness, 0);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "{kind:?}"
        );
        assert!(
            harness.mock_state.native_space_intents().is_empty(),
            "ordinary named focus must not implicitly submit a Space switch"
        );
    }
}

#[test]
fn native_focus_rejects_missing_or_failed_membership() {
    // A membership read that keeps failing.
    let mut harness = focus_harness();
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, always_unavailable());
    focus(&mut harness, 0);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "layout placement is not proof of current native membership"
    );

    // The window is not in the Space that placement claims, and no other Space
    // holds it either.
    let mut harness = focus_harness();
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![]), Ok(vec![])]);
    harness
        .mock_state
        .script_workspace_membership_queries(TARGET, [Ok(vec![1]), Ok(vec![1])]);
    focus(&mut harness, 0);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "layout placement is not proof of current native membership"
    );

    // Recovery: with the reads answering again, the same command focuses.
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

/// A caller that names the Space it drew the window in resolves the one
/// ambiguity the unclaimed path refuses: a window macOS lists in two Spaces is
/// focusable when the named Space is confirmed to be visible and to hold it.
#[test]
fn native_focus_accepts_a_named_space_for_a_multiply_listed_window() {
    let mut harness = focus_harness();
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0])]);
    harness
        .mock_state
        .script_workspace_membership_queries(TARGET, [Ok(vec![0, 1])]);
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

/// Confirming the caller's Space must not read the other Spaces: that scan is
/// what this path exists to skip, so a Space that cannot be read cannot refuse
/// a focus whose own Space is confirmed.
#[test]
fn native_focus_confirmed_claim_does_not_scan_other_spaces() {
    let mut harness = focus_harness();
    harness
        .mock_state
        .script_workspace_membership_queries(TARGET, always_unavailable());
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn native_focus_accepts_current_visibility_before_layout_projection_catches_up() {
    for moved in [false, true] {
        let mut harness = focus_harness();
        let source = source_strip(&mut harness);
        harness
            .world()
            .entity_mut(source)
            .remove::<VisibleNativeSpaceMarker>();
        if moved {
            harness
                .mock_state
                .update_window(0, |window| window.workspace_id = TARGET);
            harness
                .mock_state
                .activate_workspace(TEST_DISPLAY_ID, TARGET, false);
        }
        focus(&mut harness, 0);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            vec![0],
            "current native ownership outranks a retained source strip"
        );
    }
}

#[test]
fn native_focus_observes_a_space_selection_earlier_in_the_same_command_batch() {
    let mut harness = focus_harness();
    enable_space_control(&mut harness);
    run_commands(
        &mut harness,
        [
            Action::FocusSpace { space_id: TARGET },
            Action::FocusWindow { window_id: 1 },
        ],
    );
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
}

#[test]
fn native_focus_visible_detached_windows_keep_explicit_focus_without_space_control() {
    for kind in [
        DetachedKind::Floating,
        DetachedKind::Hidden,
        DetachedKind::Minimized,
        DetachedKind::Unplaced,
    ] {
        let mut harness = focus_harness();
        detach(&mut harness, kind);
        focus(&mut harness, 0);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            vec![0],
            "{kind:?}"
        );
        assert!(harness.mock_state.native_space_intents().is_empty());
    }
}

#[test]
fn native_focus_visible_secondary_display_does_not_require_active_display_identity() {
    let mut harness = multi_display_harness();
    harness.mock_state.script_active_display_queries([Err(())]);
    focus(&mut harness, 2);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![2]);
    assert!(
        harness
            .world()
            .resource::<crate::ecs::topology::NativeTopology>()
            .active_display()
            .is_none()
    );
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn native_focus_distinguishes_unrelated_visibility_failure_from_incomplete_topology() {
    let mut harness = multi_display_harness();
    harness
        .mock_state
        .script_active_space_queries(EXT_DISPLAY_ID, [Err(())]);
    focus(&mut harness, 0);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "only the target display's visibility is required"
    );
    harness
        .mock_state
        .script_active_space_queries(EXT_DISPLAY_ID, always_unavailable());
    focus(&mut harness, 2);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness
        .mock_state
        .script_present_display_topology_queries(EXT_DISPLAY_ID, [Err(())]);
    focus(&mut harness, 0);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "incomplete topology cannot prove globally unique membership"
    );
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn native_focus_rejects_ambiguous_display_ownership_and_recovers_after_removal() {
    let mut harness = focus_harness();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(
            TEST_DISPLAY_WIDTH,
            0,
            TEST_DISPLAY_WIDTH + EXT_DISPLAY_WIDTH,
            EXT_DISPLAY_HEIGHT,
        ),
        vec![TEST_WORKSPACE_ID],
    );
    focus(&mut harness, 0);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "do not pick the first display for a multiply owned Space"
    );
    harness.mock_state.remove_display(EXT_DISPLAY_ID);
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    focus(&mut harness, 0);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "retained layout cannot authorize focus after its physical display is gone"
    );
}

#[test]
fn native_focus_accepts_a_visible_fullscreen_space_and_duplicate_native_entries() {
    let mut harness = focus_harness();
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0, 0])]);
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TARGET, true);
    harness
        .mock_state
        .update_window(1, |window| window.is_full_screen = true);
    focus(&mut harness, 1);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![1],
        "fullscreen movement restrictions do not prohibit focusing a visible window"
    );
}

#[test]
fn native_focus_rejection_waits_for_observation_recovery_without_canceling_follow() {
    let mut harness = focus_harness();
    enable_space_control(&mut harness);
    run_commands(
        &mut harness,
        [Action::MoveWindowToSpace {
            window_id: 0,
            space_id: TARGET,
            move_focus: MoveFocus::Follow,
        }],
    );
    harness
        .world()
        .run_system_once(native_space::reconcile_native_space_transactions)
        .unwrap();
    assert_eq!(harness.mock_state.native_space_intents().len(), 2);
    harness.mock_state.take_focus_requests();
    harness
        .mock_state
        .script_active_space_queries(TEST_DISPLAY_ID, always_unavailable());
    focus(&mut harness, 1);
    harness
        .world()
        .run_system_once(native_space::reconcile_native_space_transactions)
        .unwrap();
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
    harness
        .world()
        .run_system_once(native_space::reconcile_native_space_transactions)
        .unwrap();
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "a rejected focus request cannot supersede the valid pending follow"
    );
}

#[test]
fn native_focus_script_keeps_explicit_space_activation_policy_and_identity_binding() {
    let mut harness = focus_harness();
    let plan = harness
        .world()
        .run_system_once(|state: crate::ecs::state::QueryStateParams| {
            state.extract_window_set().focus(1).plan()
        })
        .unwrap();
    for op in plan.ops {
        harness.world().write_message(Event::LayoutSpaceRequested {
            op,
            snapshot: plan.snapshot.clone(),
        });
    }
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![1],
        "bound script focus explicitly allows native Space activation"
    );
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn activation_attempt_is_not_repeated_by_coordinator_or_automatic_restore() {
    let mut harness = focus_harness();
    focus(&mut harness, 0);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    let target = find_window_entity(0, harness.world());
    for _ in 0..3 {
        harness
            .world()
            .run_system_once(crate::ecs::focus::reconcile_activation)
            .unwrap();
        harness
            .world()
            .run_system_once(move |mut commands: Commands| {
                commands.restore_focus_entity(target, true);
            })
            .unwrap();
    }
    assert!(harness.mock_state.take_focus_requests().is_empty());
    focus(&mut harness, 0);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "new explicit input gets one attempt"
    );
}

#[test]
fn activation_blocked_by_mission_control_only_executes_latest_intent() {
    let mut harness = focus_harness();
    harness
        .world()
        .resource_mut::<crate::ecs::MissionControlActive>()
        .0 = true;
    focus(&mut harness, 0);
    focus(&mut harness, 0);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness
        .world()
        .resource_mut::<crate::ecs::MissionControlActive>()
        .0 = false;
    harness
        .world()
        .run_system_once(crate::ecs::focus::reconcile_activation)
        .unwrap();
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    harness
        .world()
        .run_system_once(crate::ecs::focus::reconcile_activation)
        .unwrap();
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn activation_background_preference_is_state_only_and_space_local() {
    let mut harness = focus_harness();
    let before = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .snapshot();
    run_commands(
        &mut harness,
        [Action::SetSpaceFocusPreference {
            space_id: TARGET,
            window_id: 1,
        }],
    );
    let target = find_window_entity(1, harness.world());
    let focus = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>();
    assert_eq!(focus.preference_entity(TARGET), Some(target));
    assert_eq!(focus.navigation_entity(TARGET), Some(target));
    assert_eq!(focus.snapshot(), before);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn activation_failed_submission_yields_only_after_stable_native_readback() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(20);
    harness.mock_state.take_focus_requests();
    harness.mock_state.fail_focus_requests(true);
    focus(&mut harness, 1);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
    // Native focus stays on 0 even though Spool requested 1.
    harness
        .mock_state
        .update_app(TEST_PROCESS_ID, |app| app.focused_window_id = Some(0));
    let diagnostics = |harness: &mut TestHarness| {
        harness
            .world()
            .resource::<crate::ecs::focus::FocusCoordinator>()
            .activation_diagnostics()
    };
    assert_eq!(
        diagnostics(&mut harness)["activation"]["submission_failed"],
        true
    );
    harness
        .world()
        .resource_mut::<Time>()
        .advance_by(std::time::Duration::from_millis(250));
    harness
        .world()
        .run_system_once(crate::ecs::focus::activation::verify_activation)
        .unwrap();
    assert_eq!(
        diagnostics(&mut harness)["activation"]["outcome"],
        "unconfirmed"
    );
    harness
        .world()
        .resource_mut::<Time>()
        .advance_by(std::time::Duration::from_millis(750));
    harness
        .world()
        .run_system_once(crate::ecs::focus::activation::verify_activation)
        .unwrap();
    assert_eq!(
        diagnostics(&mut harness)["activation"]["outcome"],
        "yielded"
    );
    harness.mock_state.fail_focus_requests(false);
    harness
        .world()
        .run_system_once(crate::ecs::focus::reconcile_activation)
        .unwrap();
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn activation_unknown_exhausts_readback_budget_without_new_writes() {
    let mut harness = focus_harness();
    harness.mock_state.fail_focus_requests(true);
    focus(&mut harness, 0);
    harness
        .mock_state
        .update_app(TEST_PROCESS_ID, |app| app.focused_window_id = None);
    harness.mock_state.take_focus_requests();
    for _ in 0..5 {
        harness
            .world()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(5));
        harness
            .world()
            .run_system_once(crate::ecs::focus::activation::verify_activation)
            .unwrap();
        harness
            .world()
            .run_system_once(crate::ecs::focus::reconcile_activation)
            .unwrap();
    }
    let snapshot = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .activation_diagnostics();
    assert_eq!(snapshot["activation"]["outcome"], "unconfirmed");
    assert_eq!(snapshot["activation"]["readbacks"], 3);
    assert_eq!(snapshot["activation"]["readback_budget_exhausted"], true);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

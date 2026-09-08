use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use super::*;
use crate::commands::{Action, MoveFocus};
use crate::config::{Config, MainOptions};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{self, VisibleNativeSpaceMarker};
use crate::ecs::{Floating, WindowVisibility};
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
                .script_active_space_queries(TEST_DISPLAY_ID, [Err(())]);
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
fn native_focus_rejects_missing_ambiguous_or_failed_membership() {
    for (source, target) in [
        (Err(()), Ok(vec![1])),
        (Ok(vec![]), Ok(vec![1])),
        (Ok(vec![0]), Ok(vec![0, 1])),
    ] {
        let mut harness = focus_harness();
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID, [source]);
        harness
            .mock_state
            .script_workspace_membership_queries(TARGET, [target]);
        focus(&mut harness, 0);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "layout placement is not proof of current native membership"
        );
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID, []);
        harness
            .mock_state
            .script_workspace_membership_queries(TARGET, []);
        focus(&mut harness, 0);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    }
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
        .script_active_space_queries(EXT_DISPLAY_ID, [Err(())]);
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
        .script_active_space_queries(TEST_DISPLAY_ID, [Err(())]);
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

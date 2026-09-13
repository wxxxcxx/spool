use bevy::ecs::system::RunSystemOnce as _;
use bevy::prelude::*;

use super::*;
use crate::commands::{Action, MoveFocus};
use crate::config::{Config, MainOptions};
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::native_space::{self, NativeMoveOwner};
use crate::ecs::workspace::WindowSpaceReassignmentPending;
use crate::ecs::{PreviousTiledStrip, SpawnCommandsExt as _, WindowVisibility};
use crate::events::Event;

const TARGET: WorkspaceId = TEST_WORKSPACE_ID + 1;

fn column_harness() -> (TestHarness, Entity, [Entity; 2]) {
    let config: Config = (
        MainOptions {
            experimental_space_control: Some(true),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, TARGET],
        );
    harness.mock_state.enable_native_space_control();
    harness.pump_frames(30);
    let members = [
        find_window_entity(0, harness.world()),
        find_window_entity(1, harness.world()),
    ];
    let source = {
        let world = harness.world();
        let (entity, mut strip) = world
            .query::<(Entity, &mut LayoutStrip)>()
            .iter_mut(world)
            .find(|(_, strip)| strip.id() == TEST_WORKSPACE_ID)
            .unwrap();
        strip.append_column(Column::Stack(
            members.into_iter().map(StackItem::Single).collect(),
        ));
        entity
    };
    harness.pump_frames(3);
    (harness, source, members)
}

fn add_source_windows(harness: &mut TestHarness, ids: &[crate::platform::WinID]) -> Vec<Entity> {
    let windows = ids
        .iter()
        .map(|&id| {
            harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                id,
                IRect::new(0, 20, 400, 700),
            )
        })
        .collect();
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(windows));
    harness.pump_frames(5);
    ids.iter()
        .map(|&id| find_window_entity(id, harness.world()))
        .collect()
}

fn submit(harness: &mut TestHarness, follow: MoveFocus) {
    harness.world().write_message(Event::ActionRequested {
        action: Action::MoveColumnToSpace {
            window_id: 0,
            space_id: TARGET,
            move_focus: follow,
        },
    });
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
}

fn dispatch_action(harness: &mut TestHarness, action: Action) {
    harness
        .world()
        .write_message(Event::action_requested(action));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
}

fn source_layout(harness: &mut TestHarness, source: Entity) -> String {
    format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap())
}

/// Asserts that a Space move neither reached the platform nor started a
/// reassignment, and that the source layout is untouched.
fn assert_space_move_refused(harness: &mut TestHarness, source: Entity, members: [Entity; 2]) {
    assert!(
        harness.mock_state.native_space_intents().is_empty(),
        "a refused Space move must not reach the platform"
    );
    for member in members {
        assert!(harness.world().get::<NativeMoveOwner>(member).is_none());
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(member)
                .is_none()
        );
    }
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .all_windows()
            .is_empty(),
        "a refused Space move must leave the source column in place"
    );
}

/// The move precondition refuses a target Space that no native source listed.
/// Without it the command would submit a native intent for a Space that exists
/// on no observed display.
#[test]
fn native_space_move_rejects_a_target_outside_the_observed_topology() {
    let (mut harness, source, members) = column_harness();
    let before = source_layout(&mut harness, source);

    dispatch_action(
        &mut harness,
        Action::MoveWindowToSpace {
            window_id: 0,
            // The fixture's only display lists TEST_WORKSPACE_ID and TARGET.
            space_id: TARGET + 1,
            move_focus: MoveFocus::Stay,
        },
    );

    assert_eq!(source_layout(&mut harness, source), before);
    assert_space_move_refused(&mut harness, source, members);
}

/// A display whose own Space list could not be read cannot confirm the target,
/// and that failure is not evidence that the target Space does not exist. The
/// move is refused rather than attempted either way.
#[test]
fn native_space_move_rejects_when_no_display_can_confirm_the_target() {
    let (mut harness, source, members) = column_harness();
    harness.mock_state.script_present_display_topology_queries(
        TEST_DISPLAY_ID,
        std::iter::repeat_n(Err(()), 1000),
    );
    let before = source_layout(&mut harness, source);

    dispatch_action(
        &mut harness,
        Action::MoveWindowToSpace {
            window_id: 0,
            space_id: TARGET,
            move_focus: MoveFocus::Stay,
        },
    );

    assert_eq!(source_layout(&mut harness, source), before);
    assert_space_move_refused(&mut harness, source, members);
}

fn retained_width(harness: &mut TestHarness, entity: Entity) -> crate::ecs::layout::WidthIntent {
    let world = harness.world();
    world
        .query::<&LayoutStrip>()
        .iter(world)
        .find_map(|strip| {
            strip
                .column_state(strip.index_of(entity).ok()?)
                .map(|state| state.width)
        })
        .unwrap()
}

#[test]
fn native_move_layout_admission_allows_unrelated_columns() {
    use crate::commands::Operation;
    let (mut harness, source, members) = column_harness();
    let extra = add_source_windows(&mut harness, &[2, 3]);
    for &entity in &extra {
        harness
            .world()
            .get_mut::<LayoutStrip>(source)
            .unwrap()
            .append_column(Column::Single(entity));
    }
    submit(&mut harness, MoveFocus::Stay);
    dispatch_action(&mut harness, Action::FocusWindow { window_id: 3 });
    dispatch_action(&mut harness, Action::Window(Operation::ToggleStack));
    let column = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .column_containing(extra[0])
        .unwrap();
    assert_eq!(column.window_iter().collect::<Vec<_>>(), extra);
    dispatch_action(&mut harness, Action::Window(Operation::SetWidth(0.75)));
    for entity in members {
        assert!(
            harness
                .world()
                .get::<crate::ecs::ResizeMarker>(entity)
                .is_none()
        );
    }
    for entity in extra {
        assert_eq!(
            retained_width(&mut harness, entity),
            crate::ecs::layout::WidthIntent::ViewportRatio(0.75)
        );
    }
}

#[test]
fn native_move_layout_admission_checks_every_crossed_column() {
    use spool_shared_types::commands::Placement;
    let (mut harness, source, members) = column_harness();
    let extra = add_source_windows(&mut harness, &[2, 3, 4]);
    {
        let mut strip = harness.world().get_mut::<LayoutStrip>(source).unwrap();
        strip.append_column(Column::Single(extra[0]));
        strip.append_column(Column::Stack(
            members.into_iter().map(StackItem::Single).collect(),
        ));
        for &entity in &extra[1..] {
            strip.append_column(Column::Single(entity));
        }
    }
    submit(&mut harness, MoveFocus::Stay);
    let before = source_layout(&mut harness, source);
    dispatch_action(
        &mut harness,
        Action::ReorderColumn {
            window_id: 4,
            anchor_window_id: 2,
            placement: Placement::Before,
        },
    );
    assert_eq!(source_layout(&mut harness, source), before);
    dispatch_action(&mut harness, Action::FocusWindow { window_id: 4 });
    dispatch_action(
        &mut harness,
        Action::Window(crate::commands::Operation::Move(
            crate::commands::Direction::First,
        )),
    );
    assert_eq!(source_layout(&mut harness, source), before);
    dispatch_action(
        &mut harness,
        Action::ReorderColumn {
            window_id: 4,
            anchor_window_id: 3,
            placement: Placement::Before,
        },
    );
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .all_windows(),
        vec![extra[0], members[0], members[1], extra[2], extra[1]]
    );
}

#[test]
fn native_move_layout_admission_honors_a_barrier_without_transaction_ownership() {
    let (mut harness, source, members) = column_harness();
    harness
        .world()
        .run_system_once(move |mut commands: Commands| {
            crate::ecs::workspace::freeze_window_for_space_reassignment(
                members[0],
                0,
                &mut commands,
            );
        })
        .unwrap();
    assert!(harness.world().get::<NativeMoveOwner>(members[0]).is_none());
    dispatch_action(&mut harness, Action::FocusWindow { window_id: 1 });
    let before = source_layout(&mut harness, source);
    dispatch_action(
        &mut harness,
        Action::Window(crate::commands::Operation::ToggleStack),
    );
    assert_eq!(source_layout(&mut harness, source), before);
}

#[test]
fn native_move_layout_admission_resumes_after_timeout_or_confirmation() {
    for confirmed in [false, true] {
        let (mut harness, source, members) = column_harness();
        submit(&mut harness, MoveFocus::Stay);
        if confirmed {
            reconcile(&mut harness);
        } else {
            for id in 0..2 {
                harness
                    .mock_state
                    .update_window(id, |window| window.workspace_id = TEST_WORKSPACE_ID);
            }
            harness.pump_frames(45);
        }
        assert!(
            harness
                .world()
                .run_system_once(move |windows: crate::ecs::params::Windows| {
                    members
                        .into_iter()
                        .all(|entity| windows.layout_is_writable(entity))
                })
                .unwrap()
        );
        if !confirmed {
            let before = source_layout(&mut harness, source);
            dispatch_action(
                &mut harness,
                Action::Window(crate::commands::Operation::ToggleStack),
            );
            assert_ne!(source_layout(&mut harness, source), before);
        }
    }
}

#[test]
fn native_move_layout_admission_preserves_focus_and_floating_classification() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    for id in 0..2 {
        harness
            .mock_state
            .update_window(id, |window| window.workspace_id = TEST_WORKSPACE_ID);
    }
    harness.mock_state.take_focus_requests();
    dispatch_action(&mut harness, Action::FocusWindow { window_id: 1 });
    assert_eq!(harness.mock_state.take_focus_requests(), vec![1]);
    dispatch_action(
        &mut harness,
        Action::Window(crate::commands::Operation::ToggleFloating),
    );
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(members[1])
            .is_some()
    );
}

#[cfg(feature = "lua")]
#[test]
fn native_move_layout_admission_keeps_independent_script_operations() {
    use spool_shared_types::windowset::LayoutOp;
    let (mut harness, source, members) = column_harness();
    let extra = add_source_windows(&mut harness, &[2])[0];
    submit(&mut harness, MoveFocus::Stay);
    let before = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .structure_revision();
    let mut plan = script_set(&mut harness).plan();
    plan.ops = vec![
        LayoutOp::Unstack(1),
        LayoutOp::SetWidth {
            window: 2,
            ratio: 0.75,
        },
    ];
    dispatch_action(&mut harness, Action::Layout(plan));
    assert_eq!(
        harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .structure_revision(),
        before
    );
    for entity in members {
        assert!(
            harness
                .world()
                .get::<crate::ecs::ResizeMarker>(entity)
                .is_none()
        );
    }
    assert_eq!(
        retained_width(&mut harness, extra),
        crate::ecs::layout::WidthIntent::ViewportRatio(0.75)
    );
}

#[test]
fn native_move_layout_admission_rejects_direct_local_mutations() {
    use crate::commands::{Direction, Operation};
    for operation in [
        Operation::Move(Direction::South),
        Operation::ToggleStack,
        Operation::SetWidth(0.75),
        Operation::Maximize,
        Operation::Equalize,
        Operation::Balance,
    ] {
        let (mut harness, source, members) = column_harness();
        submit(&mut harness, MoveFocus::Stay);
        let before = source_layout(&mut harness, source);
        dispatch_action(&mut harness, Action::Window(operation));
        assert_eq!(source_layout(&mut harness, source), before);
        for entity in members {
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .is_none()
            );
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::RepositionMarker>(entity)
                    .is_none()
            );
        }
    }
}

#[test]
fn native_move_layout_admission_rejects_indirect_column_mutations() {
    use crate::commands::Operation;
    for operation in [
        Operation::ToggleStack,
        Operation::SetWidth(0.75),
        Operation::Maximize,
        Operation::Equalize,
        Operation::Balance,
    ] {
        let (mut harness, source, members) = column_harness();
        dispatch_action(
            &mut harness,
            Action::MoveWindowToSpace {
                window_id: 0,
                space_id: TARGET,
                move_focus: MoveFocus::Stay,
            },
        );
        dispatch_action(&mut harness, Action::FocusWindow { window_id: 1 });
        let before = source_layout(&mut harness, source);
        dispatch_action(&mut harness, Action::Window(operation));
        assert_eq!(source_layout(&mut harness, source), before);
        for entity in members {
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .is_none()
            );
        }
    }
}

#[cfg(feature = "lua")]
#[test]
fn native_move_layout_admission_rejects_script_mutation_cohorts() {
    use spool_shared_types::windowset::LayoutOp;
    for op in [
        LayoutOp::Swap(1, 2),
        LayoutOp::Stack {
            window: 2,
            onto: 1,
            tabs: false,
        },
        LayoutOp::Unstack(1),
        LayoutOp::SetWidth {
            window: 1,
            ratio: 0.75,
        },
        LayoutOp::SetFrame {
            window: 0,
            frame: spool_shared_types::state::Frame {
                x: 100,
                y: 100,
                width: 700,
                height: 500,
            },
        },
    ] {
        let (mut harness, source, members) = column_harness();
        let extra = add_source_windows(&mut harness, &[2])[0];
        dispatch_action(
            &mut harness,
            Action::MoveWindowToSpace {
                window_id: 0,
                space_id: TARGET,
                move_focus: MoveFocus::Stay,
            },
        );
        let before = source_layout(&mut harness, source);
        let mut plan = script_set(&mut harness).plan();
        plan.ops = vec![op];
        dispatch_action(&mut harness, Action::Layout(plan));
        assert_eq!(source_layout(&mut harness, source), before);
        for entity in members.into_iter().chain([extra]) {
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::ResizeMarker>(entity)
                    .is_none()
            );
            assert!(
                harness
                    .world()
                    .get::<crate::ecs::RepositionMarker>(entity)
                    .is_none()
            );
        }
    }
}

fn reconcile(harness: &mut TestHarness) {
    harness
        .world()
        .run_system_once(native_space::reconcile_native_space_transactions)
        .unwrap();
}

fn refresh_native_observation(harness: &mut TestHarness) {
    harness
        .world()
        .run_system_once(crate::ecs::topology::gather_initial_topology)
        .unwrap();
}

fn mark_target_visible(harness: &mut TestHarness) {
    let world = harness.world();
    let target = world
        .query::<(Entity, &LayoutStrip)>()
        .iter(world)
        .find(|(_, strip)| strip.id() == TARGET)
        .unwrap()
        .0;
    world
        .entity_mut(target)
        .insert(native_space::VisibleNativeSpaceMarker);
}

#[test]
fn native_move_hidden_members_remember_destination_without_rejoining_layout() {
    for minimized in [false, true] {
        for hidden in [vec![0], vec![1], vec![0, 1]] {
            let (mut harness, source, members) = column_harness();
            submit(&mut harness, MoveFocus::Stay);
            for &index in &hidden {
                harness
                    .world()
                    .entity_mut(members[index])
                    .insert(if minimized {
                        WindowVisibility::Minimized
                    } else {
                        WindowVisibility::Hidden
                    });
            }
            reconcile(&mut harness);
            assert_eq!(harness.world().get::<LayoutStrip>(source).unwrap().len(), 0);
            for (index, member) in members.into_iter().enumerate() {
                let world = harness.world();
                let owners = world
                    .query::<&LayoutStrip>()
                    .iter(world)
                    .filter(|strip| strip.contains(member))
                    .map(LayoutStrip::id)
                    .collect::<Vec<_>>();
                if hidden.contains(&index) {
                    assert!(
                        owners.is_empty(),
                        "hidden members must not occupy live columns"
                    );
                    assert_eq!(
                        world
                            .get::<PreviousTiledStrip>(member)
                            .unwrap()
                            .workspace_id,
                        TARGET
                    );
                } else {
                    assert_eq!(owners, vec![TARGET]);
                }
                assert!(world.get::<NativeMoveOwner>(member).is_none());
                assert!(
                    world
                        .get::<WindowSpaceReassignmentPending>(member)
                        .is_none()
                );
            }
            for index in hidden {
                harness
                    .world()
                    .entity_mut(members[index])
                    .remove::<WindowVisibility>();
                let world = harness.world();
                assert_eq!(
                    world
                        .query::<&LayoutStrip>()
                        .iter(world)
                        .filter(|strip| strip.contains(members[index]))
                        .map(LayoutStrip::id)
                        .collect::<Vec<_>>(),
                    vec![TARGET],
                    "showing a moved window restores it exactly once at the destination"
                );
            }
        }
    }
}

#[test]
fn native_move_visibility_change_cancels_follow_before_or_after_space_focus() {
    for minimized in [false, true] {
        for focus_submitted in [false, true] {
            let (mut harness, _, members) = column_harness();
            submit(&mut harness, MoveFocus::Follow);
            if focus_submitted {
                reconcile(&mut harness);
            }
            harness.world().entity_mut(members[0]).insert(if minimized {
                WindowVisibility::Minimized
            } else {
                WindowVisibility::Hidden
            });
            refresh_native_observation(&mut harness);
            harness.mock_state.take_focus_requests();
            reconcile(&mut harness);
            assert!(harness.mock_state.take_focus_requests().is_empty());
            assert_eq!(
                harness.mock_state.native_space_intents().len(),
                if focus_submitted { 2 } else { 1 },
                "a hidden member must not submit a new Space focus"
            );
            harness
                .world()
                .entity_mut(members[0])
                .remove::<WindowVisibility>();
            refresh_native_observation(&mut harness);
            harness.mock_state.take_focus_requests();
            reconcile(&mut harness);
            assert!(
                harness.mock_state.take_focus_requests().is_empty(),
                "showing the window must not revive its canceled follow"
            );
        }
    }
}

#[test]
fn native_move_hidden_same_space_keeps_remembered_insertion_without_freezing() {
    for visibility in [WindowVisibility::Hidden, WindowVisibility::Minimized] {
        let (mut harness, source, _) = column_harness();
        let member = add_source_windows(&mut harness, &[2])[0];
        harness.world().entity_mut(member).insert(visibility);
        let before = *harness.world().get::<PreviousTiledStrip>(member).unwrap();
        assert_eq!(before.index, 1);
        let layout_before = format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap());
        harness.world().write_message(Event::ActionRequested {
            action: Action::MoveWindowToSpace {
                window_id: 2,
                space_id: TEST_WORKSPACE_ID,
                move_focus: MoveFocus::Stay,
            },
        });
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(member)
                .is_none(),
            "a reconciled hidden same-Space request is a no-op"
        );
        reconcile(&mut harness);
        let after = harness.world().get::<PreviousTiledStrip>(member).unwrap();
        assert_eq!(
            (after.workspace_id, after.index),
            (before.workspace_id, before.index)
        );
        assert_eq!(
            format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap()),
            layout_before
        );
        assert!(harness.mock_state.native_space_intents().is_empty());
    }
}

#[test]
fn native_move_rejects_a_tracked_but_unavailable_associated_member() {
    let (mut harness, source, members) = column_harness();
    let extra = add_source_windows(&mut harness, &[2])[0];
    harness.mock_state.set_associated_windows(0, vec![0, 2]);
    harness.mock_state.os_withdraw_window(2);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(extra)
            .is_some()
    );
    let before = format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap());
    submit(&mut harness, MoveFocus::Stay);
    assert!(
        harness.mock_state.native_space_intents().is_empty(),
        "an unavailable tracked association must not be silently omitted from the transaction"
    );
    for member in [members[0], members[1], extra] {
        assert!(harness.world().get::<NativeMoveOwner>(member).is_none());
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(member)
                .is_none()
        );
    }
    assert_eq!(
        format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap()),
        before
    );
    harness.mock_state.os_restore_withdrawn_window(2);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    submit(&mut harness, MoveFocus::Stay);
    reconcile(&mut harness);
    for member in [members[0], members[1], extra] {
        let world = harness.world();
        assert_eq!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|strip| strip.contains(member))
                .map(LayoutStrip::id)
                .collect::<Vec<_>>(),
            vec![TARGET]
        );
    }
}

#[test]
fn native_move_visibility_recovery_during_confirmation_does_not_revive_follow() {
    for minimized in [false, true] {
        let (mut harness, _, members) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        harness.world().entity_mut(members[0]).insert(if minimized {
            WindowVisibility::Minimized
        } else {
            WindowVisibility::Hidden
        });
        harness
            .mock_state
            .script_workspace_membership_queries(TARGET, [Err(())]);
        reconcile(&mut harness);
        for member in members {
            assert!(harness.world().get::<NativeMoveOwner>(member).is_some());
            assert!(
                harness
                    .world()
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_some()
            );
        }
        harness
            .world()
            .entity_mut(members[0])
            .remove::<WindowVisibility>();
        assert!(
            harness
                .world()
                .get::<PreviousTiledStrip>(members[0])
                .is_some()
        );
        reconcile(&mut harness);
        let world = harness.world();
        let target = world
            .query::<&LayoutStrip>()
            .iter(world)
            .find(|strip| strip.id() == TARGET)
            .unwrap();
        assert!(matches!(target.columns().next(), Some(Column::Stack(items))
            if items == &vec![StackItem::Single(members[0]), StackItem::Single(members[1])]));
        assert!(world.get::<PreviousTiledStrip>(members[0]).is_none());
        assert_eq!(harness.mock_state.native_space_intents().len(), 1);
    }
}

#[test]
fn native_move_ax_withdrawal_defers_commit_and_recovers_hidden_destination() {
    for hidden in [false, true] {
        let (mut harness, source, members) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        harness.mock_state.os_withdraw_window(0);
        harness.world().write_message(Event::SpaceChanged);
        harness.world().run_schedule(Update);
        harness.world().resource_mut::<Messages<Event>>().clear();
        assert!(
            harness
                .world()
                .get::<crate::ecs::reconcile::WindowUnavailable>(members[0])
                .is_some()
        );
        if hidden {
            harness
                .world()
                .entity_mut(members[0])
                .insert(WindowVisibility::Hidden);
        }
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        assert!(harness.mock_state.take_focus_requests().is_empty());
        assert_eq!(harness.mock_state.native_space_intents().len(), 1);
        for member in members {
            assert!(
                harness
                    .world()
                    .get::<LayoutStrip>(source)
                    .unwrap()
                    .contains(member)
            );
            assert!(harness.world().get::<NativeMoveOwner>(member).is_some());
            assert!(
                harness
                    .world()
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_some()
            );
        }
        harness.mock_state.os_restore_withdrawn_window(0);
        harness.world().write_message(Event::SpaceChanged);
        harness.world().run_schedule(Update);
        harness.world().resource_mut::<Messages<Event>>().clear();
        assert!(
            harness
                .world()
                .get::<crate::ecs::reconcile::WindowUnavailable>(members[0])
                .is_none()
        );
        reconcile(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            if hidden { vec![] } else { vec![0] }
        );
        for (index, member) in members.into_iter().enumerate() {
            let world = harness.world();
            let owners = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|strip| strip.contains(member))
                .map(LayoutStrip::id)
                .collect::<Vec<_>>();
            if hidden && index == 0 {
                assert!(owners.is_empty());
                assert_eq!(
                    world
                        .get::<PreviousTiledStrip>(member)
                        .unwrap()
                        .workspace_id,
                    TARGET
                );
            } else {
                assert_eq!(owners, vec![TARGET]);
            }
            assert!(world.get::<NativeMoveOwner>(member).is_none());
            assert!(
                world
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_none()
            );
        }
    }
}

#[test]
fn native_move_already_hidden_window_preserves_recovery_index_without_following() {
    for visibility in [WindowVisibility::Hidden, WindowVisibility::Minimized] {
        let (mut harness, source, _) = column_harness();
        let member = add_source_windows(&mut harness, &[2])[0];
        harness.world().entity_mut(member).insert(visibility);
        let source_before = format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap());
        assert_eq!(
            harness
                .world()
                .get::<PreviousTiledStrip>(member)
                .unwrap()
                .index,
            1
        );
        for request in 0..2 {
            harness.world().write_message(Event::ActionRequested {
                action: Action::MoveWindowToSpace {
                    window_id: 2,
                    space_id: TARGET,
                    move_focus: MoveFocus::Follow,
                },
            });
            harness
                .world()
                .run_system_once(crate::commands::dispatch_actions)
                .unwrap();
            harness.world().resource_mut::<Messages<Event>>().clear();
            if request == 0 {
                assert_eq!(
                    harness
                        .world()
                        .get::<WindowSpaceReassignmentPending>(member)
                        .unwrap()
                        .source_index(),
                    1
                );
            } else {
                assert!(
                    harness
                        .world()
                        .get::<WindowSpaceReassignmentPending>(member)
                        .is_none()
                );
            }
            reconcile(&mut harness);
            let world = harness.world();
            let previous = world.get::<PreviousTiledStrip>(member).unwrap();
            assert_eq!((previous.workspace_id, previous.index), (TARGET, 1));
            assert!(
                world
                    .query::<&LayoutStrip>()
                    .iter(world)
                    .all(|strip| !strip.contains(member))
            );
            assert_eq!(harness.mock_state.native_space_intents().len(), 1);
        }
        assert_eq!(
            format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap()),
            source_before
        );
        harness
            .world()
            .entity_mut(member)
            .remove::<WindowVisibility>();
        let world = harness.world();
        assert_eq!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|strip| strip.contains(member))
                .map(LayoutStrip::id)
                .collect::<Vec<_>>(),
            vec![TARGET]
        );
        assert!(world.get::<PreviousTiledStrip>(member).is_none());
    }
}

#[test]
fn native_move_ax_timeout_retains_hidden_reassignment_until_membership_audit() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    harness.mock_state.os_withdraw_window(0);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(members[0])
            .is_some()
    );
    harness
        .world()
        .entity_mut(members[0])
        .insert(WindowVisibility::Minimized);
    harness.pump_frames(45);
    assert!(harness.world().get::<NativeMoveOwner>(members[0]).is_none());
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(members[0])
            .is_some(),
        "timeout releases the transaction, not the unavailable window's geometry barrier"
    );
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
    harness.mock_state.os_restore_withdrawn_window(0);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    let world = harness.world();
    assert!(
        world
            .get::<WindowSpaceReassignmentPending>(members[0])
            .is_none()
    );
    assert_eq!(
        world
            .get::<PreviousTiledStrip>(members[0])
            .unwrap()
            .workspace_id,
        TARGET
    );
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .all(|strip| !strip.contains(members[0]))
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        1,
        "recovery neither rolls back movement nor resumes expired follow"
    );
}

#[test]
fn focusing_a_window_in_another_space_goes_there_first() {
    let (mut harness, _, _) = column_harness();
    // Park the column in the other Space without following it there, so the
    // only thing that can focus window 0 is the request under test.
    submit(&mut harness, MoveFocus::Stay);
    reconcile(&mut harness);
    harness.mock_state.take_focus_requests();
    let intents = harness.mock_state.native_space_intents().len();

    dispatch_action(
        &mut harness,
        Action::FocusWindowInSpace {
            window_id: 0,
            space_id: TARGET,
        },
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        intents + 1,
        "the Space switch is submitted straight away"
    );
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "the window is not focused before its Space is up"
    );

    mark_target_visible(&mut harness);
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "and then it is focused"
    );
}

#[test]
fn a_space_window_click_supersedes_both_stages_of_an_older_follow() {
    for focus_submitted in [false, true] {
        let (mut harness, _, _) = column_harness();
        add_source_windows(&mut harness, &[2]);
        submit(&mut harness, MoveFocus::Follow);
        if focus_submitted {
            reconcile(&mut harness);
        }
        dispatch_action(
            &mut harness,
            Action::FocusWindowInSpace {
                window_id: 2,
                space_id: TEST_WORKSPACE_ID,
            },
        );
        harness.mock_state.take_focus_requests();
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![2]);
        mark_target_visible(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "the older follow must not revive when its Space becomes visible"
        );
    }
}

#[test]
fn a_failed_repeat_space_window_click_preserves_the_accepted_follow() {
    let (mut harness, _, _) = column_harness();
    submit(&mut harness, MoveFocus::Stay);
    reconcile(&mut harness);
    let action = Action::FocusWindowInSpace {
        window_id: 0,
        space_id: TARGET,
    };
    dispatch_action(&mut harness, action.clone());
    harness.mock_state.disable_native_space_control();
    dispatch_action(&mut harness, action);
    harness.mock_state.enable_native_space_control();
    harness.mock_state.take_focus_requests();
    mark_target_visible(&mut harness);
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn focusing_a_withdrawn_window_in_another_space_still_goes_there() {
    let (mut harness, _, _) = column_harness();
    // Park the column in the other Space without following it there, so the
    // only thing that can switch is the request under test.
    submit(&mut harness, MoveFocus::Stay);
    reconcile(&mut harness);
    harness.mock_state.take_focus_requests();
    let intents = harness.mock_state.native_space_intents().len();

    // macOS withdraws the AX surface of every window in a Space it is not
    // showing. That is the normal state of the windows a Bar icon for another
    // Space points at, so the request must not depend on seeing the window: the
    // Space switch is all the user asked for, and the focus can wait for it.
    harness.mock_state.os_withdraw_window(0);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(2);
    let withdrawn = find_window_entity(0, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(withdrawn)
            .is_some(),
        "the fixture must model the withdrawn window a hidden Space holds"
    );

    dispatch_action(
        &mut harness,
        Action::FocusWindowInSpace {
            window_id: 0,
            space_id: TARGET,
        },
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        intents + 1,
        "the Space switch is submitted for a window whose surface is withdrawn"
    );
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "the window is not focused before its Space is up"
    );

    // Its Space comes up and macOS exposes the window again.
    harness.mock_state.os_restore_withdrawn_window(0);
    mark_target_visible(&mut harness);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(5);
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "and then the window it was aimed at is focused"
    );
}

#[test]
fn focusing_a_window_that_is_no_longer_in_that_space_is_refused() {
    let (mut harness, _, _) = column_harness();
    let intents = harness.mock_state.native_space_intents().len();
    // Window 0 is in the Space macOS is showing, not in TARGET: the request is
    // stale, and switching the user to an empty Space on the strength of it
    // would be worse than doing nothing.
    dispatch_action(
        &mut harness,
        Action::FocusWindowInSpace {
            window_id: 0,
            space_id: TARGET,
        },
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        intents,
        "no switch for a window that is not there"
    );
    mark_target_visible(&mut harness);
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn native_move_follow_requires_observed_visibility_not_a_retained_marker() {
    for visibility in [Err(()), Ok(TEST_WORKSPACE_ID)] {
        let (mut harness, _, _) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.native_space_intents().len(), 2);
        mark_target_visible(&mut harness);
        harness
            .mock_state
            .script_active_space_queries(TEST_DISPLAY_ID, [visibility]);
        refresh_native_observation(&mut harness);
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "stale visibility cannot authorize focus"
        );

        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
        reconcile(&mut harness);
        assert!(harness.mock_state.take_focus_requests().is_empty());
    }
}

#[test]
fn native_move_disabling_control_cancels_follow_without_losing_confirmation() {
    for focus_submitted in [false, true] {
        let (mut harness, _, members) = column_harness();
        let enabled = harness.world().resource::<Config>().clone();
        submit(&mut harness, MoveFocus::Follow);
        if focus_submitted {
            reconcile(&mut harness);
        }
        let intents = harness.mock_state.native_space_intents();
        harness.world().insert_resource(Config::default());
        mark_target_visible(&mut harness);
        refresh_native_observation(&mut harness);
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        assert_eq!(
            harness.mock_state.native_space_intents(),
            intents,
            "disabled control must not submit Space focus"
        );
        assert!(harness.mock_state.take_focus_requests().is_empty());
        let world = harness.world();
        for member in members {
            assert!(world.get::<NativeMoveOwner>(member).is_none());
            assert!(
                world
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_none()
            );
            assert!(
                world
                    .query::<&LayoutStrip>()
                    .iter(world)
                    .any(|strip| strip.id() == TARGET && strip.contains(member))
            );
        }
        harness.world().insert_resource(enabled);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.native_space_intents(), intents);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "reenabling cannot revive a canceled follow"
        );
    }
}

#[test]
fn native_move_column_confirmation_preserves_changes_to_floating_mode() {
    for floated in [vec![0], vec![1], vec![0, 1]] {
        let (mut harness, _, members) = column_harness();
        submit(&mut harness, MoveFocus::Stay);
        for &index in &floated {
            harness
                .world()
                .entity_mut(members[index])
                .insert(crate::ecs::Floating);
        }
        reconcile(&mut harness);
        let world = harness.world();
        for (index, member) in members.into_iter().enumerate() {
            let owner = world
                .query::<&LayoutStrip>()
                .iter(world)
                .find(|strip| strip.contains(member))
                .map(LayoutStrip::id);
            assert_eq!(
                owner,
                (!floated.contains(&index)).then_some(TARGET),
                "confirmation must not retile floated members"
            );
            assert!(world.get::<NativeMoveOwner>(member).is_none());
            assert!(
                world
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_none()
            );
        }
    }
}

#[test]
fn native_move_disabling_control_while_membership_is_pending_cancels_later_follow() {
    let (mut harness, _, _) = column_harness();
    let enabled = harness.world().resource::<Config>().clone();
    submit(&mut harness, MoveFocus::Follow);
    harness
        .mock_state
        .script_workspace_membership_queries(TARGET, [Err(())]);
    harness.world().insert_resource(Config::default());
    reconcile(&mut harness);
    harness.world().insert_resource(enabled);
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
    harness.mock_state.take_focus_requests();
    mark_target_visible(&mut harness);
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn native_move_same_space_requests_preserve_layout_without_moving_or_freezing() {
    for column in [false, true] {
        for follow in [MoveFocus::Stay, MoveFocus::Follow] {
            let (mut harness, source, members) = column_harness();
            let third = add_source_windows(&mut harness, &[2])[0];
            let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
            strip.append_column(Column::Stack(
                members.into_iter().map(StackItem::Single).collect(),
            ));
            strip.append(third);
            let before = format!("{strip:?}");
            harness.world().entity_mut(source).insert(strip);
            let action = if column {
                Action::MoveColumnToSpace {
                    window_id: 0,
                    space_id: TEST_WORKSPACE_ID,
                    move_focus: follow,
                }
            } else {
                Action::MoveWindowToSpace {
                    window_id: 0,
                    space_id: TEST_WORKSPACE_ID,
                    move_focus: follow,
                }
            };
            harness
                .world()
                .write_message(Event::action_requested(action));
            harness
                .world()
                .run_system_once(crate::commands::dispatch_actions)
                .unwrap();
            harness.world().resource_mut::<Messages<Event>>().clear();
            assert!(
                harness
                    .mock_state
                    .native_space_intents()
                    .iter()
                    .all(|intent| !matches!(
                        intent,
                        crate::manager::NativeSpaceIntent::MoveWindows { .. }
                    )),
                "same-Space requests must not issue native movement"
            );
            for member in members {
                assert!(harness.world().get::<NativeMoveOwner>(member).is_none());
                assert!(
                    harness
                        .world()
                        .get::<WindowSpaceReassignmentPending>(member)
                        .is_none()
                );
            }
            harness.mock_state.take_focus_requests();
            reconcile(&mut harness);
            assert_eq!(
                format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap()),
                before
            );
            let focused = harness.mock_state.take_focus_requests();
            assert_eq!(
                focused,
                if follow == MoveFocus::Follow {
                    vec![0]
                } else {
                    vec![]
                }
            );
        }
    }
}

#[test]
fn native_move_new_stay_request_cancels_an_older_pending_follow() {
    let (mut harness, _, _) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    let intents = harness.mock_state.native_space_intents();
    assert_eq!(intents.len(), 2);
    harness
        .world()
        .write_message(Event::action_requested(Action::MoveWindowToSpace {
            window_id: 0,
            space_id: TARGET,
            move_focus: MoveFocus::Stay,
        }));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    mark_target_visible(&mut harness);
    refresh_native_observation(&mut harness);
    harness.mock_state.take_focus_requests();
    reconcile(&mut harness);
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "a newer stay request cancels the old follow"
    );
    assert_eq!(harness.mock_state.native_space_intents(), intents);
}

#[test]
fn native_move_new_window_focus_supersedes_pending_space_and_window_follow() {
    for focus_submitted in [false, true] {
        let (mut harness, _, members) = column_harness();
        add_source_windows(&mut harness, &[2]);
        submit(&mut harness, MoveFocus::Follow);
        let requested = if focus_submitted {
            reconcile(&mut harness);
            mark_target_visible(&mut harness);
            refresh_native_observation(&mut harness);
            1
        } else {
            2
        };
        harness.mock_state.take_focus_requests();
        harness
            .world()
            .write_message(Event::action_requested(Action::FocusWindow {
                window_id: requested,
            }));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness.world().resource_mut::<Messages<Event>>().clear();
        assert_eq!(harness.mock_state.take_focus_requests(), vec![requested]);
        reconcile(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "a later explicit window selection supersedes both stages of an old follow"
        );
        assert_eq!(
            harness.mock_state.native_space_intents().len(),
            if focus_submitted { 2 } else { 1 }
        );
        for member in members {
            let world = harness.world();
            assert!(
                world
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_none()
            );
            assert!(
                world
                    .query::<&LayoutStrip>()
                    .iter(world)
                    .any(|strip| strip.id() == TARGET && strip.contains(member)),
                "canceling follow must still commit the submitted move"
            );
        }
    }
}

#[test]
fn native_move_new_space_focus_supersedes_pending_space_and_window_follow() {
    for focus_submitted in [false, true] {
        for target in [TEST_WORKSPACE_ID, TARGET] {
            let (mut harness, _, _) = column_harness();
            submit(&mut harness, MoveFocus::Follow);
            if focus_submitted {
                reconcile(&mut harness);
            }
            harness
                .world()
                .write_message(Event::action_requested(Action::FocusSpace {
                    space_id: target,
                }));
            harness
                .world()
                .run_system_once(crate::commands::dispatch_actions)
                .unwrap();
            harness.world().resource_mut::<Messages<Event>>().clear();
            let accepted = harness.mock_state.native_space_intents();
            harness.mock_state.take_focus_requests();
            refresh_native_observation(&mut harness);
            reconcile(&mut harness);
            refresh_native_observation(&mut harness);
            reconcile(&mut harness);
            assert_eq!(
                harness.mock_state.native_space_intents(),
                accepted,
                "a newer Space selection must not be followed by the old Space request"
            );
            assert!(
                harness.mock_state.take_focus_requests().is_empty(),
                "selecting the same destination does not revive an old window follow"
            );
        }
    }
}

#[cfg(feature = "lua")]
#[test]
fn script_move_follow_and_focus_respect_operation_order() {
    for follow_last in [false, true] {
        let (mut harness, _, _) = column_harness();
        add_source_windows(&mut harness, &[2]);
        let initial = script_set(&mut harness);
        let plan = if follow_last {
            initial.focus(2).shift_following(0, TARGET, true)
        } else {
            initial.shift_following(0, TARGET, true).focus(2)
        }
        .plan();
        harness.mock_state.take_focus_requests();
        harness
            .world()
            .write_message(Event::action_requested(Action::Layout(plan)));
        harness.pump_frames(30);
        assert_eq!(harness.mock_state.window_workspace(0), Some(TARGET));
        assert_eq!(
            script_set(&mut harness).focused(),
            Some(if follow_last { 0 } else { 2 })
        );
        let intents = harness.mock_state.native_space_intents();
        assert_eq!(
            intents.len(),
            if follow_last { 2 } else { 1 },
            "later focus must supersede the earlier follow without canceling movement"
        );
    }
}

#[test]
fn native_move_batched_focus_commands_preserve_acceptance_order() {
    for follow_last in [false, true] {
        for focus_kind in 0..3 {
            let (mut harness, _, _) = column_harness();
            add_source_windows(&mut harness, &[2]);
            let direct = match focus_kind {
                0 => Action::FocusSpace {
                    space_id: TEST_WORKSPACE_ID,
                },
                1 => Action::FocusWindow { window_id: 2 },
                _ => Action::Window(crate::commands::Operation::Focus(
                    crate::commands::Direction::Last,
                )),
            };
            let moving = Action::MoveWindowToSpace {
                window_id: 0,
                space_id: TARGET,
                move_focus: MoveFocus::Follow,
            };
            let actions = if follow_last {
                [direct, moving]
            } else {
                [moving, direct]
            };
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
            harness.mock_state.take_focus_requests();
            reconcile(&mut harness);
            refresh_native_observation(&mut harness);
            reconcile(&mut harness);
            assert_eq!(
                harness.mock_state.take_focus_requests(),
                if follow_last { vec![0] } else { vec![] }
            );
            assert_eq!(
                harness.mock_state.native_space_intents().len(),
                1 + usize::from(focus_kind == 0) + usize::from(follow_last)
            );
        }
    }
}

#[test]
fn native_move_automatic_focus_preserves_pending_follow() {
    for focus_submitted in [false, true] {
        let (mut harness, _, _) = column_harness();
        let other = add_source_windows(&mut harness, &[2])[0];
        submit(&mut harness, MoveFocus::Follow);
        if focus_submitted {
            reconcile(&mut harness);
        }
        harness.mock_state.take_focus_requests();
        harness
            .world()
            .run_system_once(move |mut commands: Commands| {
                commands.restore_focus_entity(other, true);
            })
            .unwrap();
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            if focus_submitted { vec![] } else { vec![2] },
            "automatic restoration cannot activate an invisible source Space"
        );
        reconcile(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    }
}

#[test]
fn native_move_invalid_or_stale_focus_preserves_pending_follow() {
    for stale_script in [false, true] {
        let (mut harness, _, _) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        reconcile(&mut harness);
        if stale_script {
            let plan = script_set(&mut harness).focus(1).plan();
            harness
                .world()
                .insert_resource(crate::ecs::layout_snapshot::LayoutSession::default());
            submit_script_space(&mut harness, plan);
        } else {
            harness
                .world()
                .write_message(Event::action_requested(Action::FocusWindow {
                    window_id: -1,
                }));
            harness
                .world()
                .run_system_once(crate::commands::dispatch_actions)
                .unwrap();
            harness.world().resource_mut::<Messages<Event>>().clear();
        }
        harness.mock_state.take_focus_requests();
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    }
}

#[test]
fn native_move_rejected_space_focus_preserves_pending_follow() {
    for focus_submitted in [false, true] {
        let (mut harness, _, _) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        if focus_submitted {
            reconcile(&mut harness);
        }
        let before = harness.mock_state.native_space_intents();
        harness.mock_state.disable_native_space_control();
        harness
            .world()
            .write_message(Event::action_requested(Action::FocusSpace {
                space_id: TEST_WORKSPACE_ID,
            }));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness.world().resource_mut::<Messages<Event>>().clear();
        assert_eq!(harness.mock_state.native_space_intents(), before);
        harness.mock_state.enable_native_space_control();
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
    }
}

#[test]
fn native_move_rejected_same_space_follow_preserves_an_unrelated_follow() {
    let (mut harness, _, _) = column_harness();
    add_source_windows(&mut harness, &[2]);
    harness
        .world()
        .write_message(Event::action_requested(Action::MoveWindowToSpace {
            window_id: 2,
            space_id: TARGET,
            move_focus: MoveFocus::Stay,
        }));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    reconcile(&mut harness);
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    let before = harness.mock_state.native_space_intents();
    harness.mock_state.disable_native_space_control();
    harness
        .world()
        .write_message(Event::action_requested(Action::MoveWindowToSpace {
            window_id: 2,
            space_id: TARGET,
            move_focus: MoveFocus::Follow,
        }));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    assert_eq!(harness.mock_state.native_space_intents(), before);
    harness.mock_state.enable_native_space_control();
    harness.mock_state.take_focus_requests();
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn native_move_new_follow_supersedes_an_older_unrelated_follow() {
    for focus_submitted in [false, true] {
        let (mut harness, _, _) = column_harness();
        add_source_windows(&mut harness, &[2]);
        submit(&mut harness, MoveFocus::Follow);
        if focus_submitted {
            reconcile(&mut harness);
        }
        harness
            .world()
            .write_message(Event::action_requested(Action::MoveWindowToSpace {
                window_id: 2,
                space_id: TARGET,
                move_focus: MoveFocus::Follow,
            }));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness.world().resource_mut::<Messages<Event>>().clear();
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        refresh_native_observation(&mut harness);
        reconcile(&mut harness);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            vec![2],
            "only the newer accepted follow should claim focus"
        );
        assert_eq!(
            harness.mock_state.native_space_intents().len(),
            if focus_submitted { 4 } else { 3 }
        );
        for id in 0..3 {
            assert_eq!(harness.mock_state.window_workspace(id), Some(TARGET));
        }
    }
}

#[cfg(feature = "lua")]
#[test]
fn script_native_focus_revalidates_identity_at_native_dispatch() {
    let (mut harness, _, _) = column_harness();
    let other = add_source_windows(&mut harness, &[2])[0];
    let plan = script_set(&mut harness).focus(2).plan();
    let deferred = Event::LayoutSpaceRequested {
        op: plan.ops[0],
        snapshot: plan.snapshot,
    };
    harness.world().despawn(other);
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        2,
        IRect::new(0, 20, 400, 700),
    );
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(5);
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    harness.mock_state.take_focus_requests();
    harness.world().write_message(deferred);
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    assert!(
        harness.mock_state.take_focus_requests().is_empty(),
        "a reused ID cannot receive deferred focus"
    );
    refresh_native_observation(&mut harness);
    reconcile(&mut harness);
    assert_eq!(
        harness.mock_state.take_focus_requests(),
        vec![0],
        "stale focus cannot cancel the valid follow"
    );
}

#[test]
fn native_move_column_filtering_keeps_surviving_native_tabs_grouped() {
    let (mut harness, source, members) = column_harness();
    let third = add_source_windows(&mut harness, &[2])[0];
    let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
    strip.append_column(Column::Stack(vec![
        StackItem::Tabs(vec![members[0], third]),
        StackItem::Single(members[1]),
    ]));
    harness.world().entity_mut(source).insert(strip);
    submit(&mut harness, MoveFocus::Stay);
    harness
        .world()
        .entity_mut(members[1])
        .insert(crate::ecs::Floating);
    reconcile(&mut harness);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TARGET)
        .unwrap();
    assert!(!target.contains(members[1]));
    let Some(Column::Tabs(tabs)) = target.column_containing(members[0]) else {
        panic!("surviving native tabs must not be flattened")
    };
    assert_eq!(tabs, vec![members[0], third]);
}

#[test]
fn native_move_confirmation_preserves_an_already_present_layout_after_an_unknown_read() {
    let (mut harness, source, members) = column_harness();
    let before = format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap());
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Err(())]);
    harness
        .world()
        .write_message(Event::action_requested(Action::MoveWindowToSpace {
            window_id: 0,
            space_id: TEST_WORKSPACE_ID,
            move_focus: MoveFocus::Stay,
        }));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    harness.world().resource_mut::<Messages<Event>>().clear();
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        1,
        "unknown membership cannot prove a no-op at admission"
    );
    reconcile(&mut harness);
    assert_eq!(
        format!("{:?}", harness.world().get::<LayoutStrip>(source).unwrap()),
        before
    );
    assert!(harness.world().get::<NativeMoveOwner>(members[0]).is_none());
    assert!(
        harness
            .world()
            .get::<WindowSpaceReassignmentPending>(members[0])
            .is_none()
    );
}

#[test]
fn native_move_new_move_cancels_only_affected_pending_follow() {
    for moving in [0, 1] {
        let (mut harness, _, members) = column_harness();
        submit(&mut harness, MoveFocus::Follow);
        reconcile(&mut harness);
        harness
            .world()
            .write_message(Event::action_requested(Action::MoveWindowToSpace {
                window_id: moving,
                space_id: TEST_WORKSPACE_ID,
                move_focus: MoveFocus::Stay,
            }));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness.world().resource_mut::<Messages<Event>>().clear();
        harness
            .mock_state
            .update_window(moving, |window| window.workspace_id = TARGET);
        mark_target_visible(&mut harness);
        refresh_native_observation(&mut harness);
        harness.mock_state.take_focus_requests();
        reconcile(&mut harness);
        assert_eq!(
            harness.mock_state.take_focus_requests(),
            if moving == 0 { vec![] } else { vec![0] }
        );
        harness
            .mock_state
            .update_window(moving, |window| window.workspace_id = TEST_WORKSPACE_ID);
        reconcile(&mut harness);
        assert!(harness.mock_state.take_focus_requests().is_empty());
        for member in members {
            assert!(harness.world().get::<NativeMoveOwner>(member).is_none());
        }
    }
}

#[test]
fn native_move_follow_accepts_fresh_visibility_before_marker_projection() {
    let (mut harness, _, _) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    refresh_native_observation(&mut harness);
    let world = harness.world();
    assert!(
        world
            .query::<(&LayoutStrip, Has<native_space::VisibleNativeSpaceMarker>)>()
            .iter(world)
            .any(|(strip, visible)| strip.id() == TARGET && !visible)
    );
    harness.mock_state.take_focus_requests();
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.take_focus_requests(), vec![0]);
}

#[test]
fn native_move_column_keeps_associated_windows_outside_the_captured_column() {
    let (mut harness, source, members) = column_harness();
    let third = add_source_windows(&mut harness, &[2])[0];
    assert!(
        !harness
            .world()
            .get::<LayoutStrip>(source)
            .unwrap()
            .column_containing(members[0])
            .unwrap()
            .window_iter()
            .any(|entity| entity == third)
    );
    harness.mock_state.set_associated_windows(0, vec![2]);
    submit(&mut harness, MoveFocus::Stay);
    reconcile(&mut harness);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TARGET)
        .unwrap();
    assert!(
        target.contains(third),
        "confirmed associated windows must not disappear from all strips"
    );
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("original column must stay grouped")
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    assert!(
        matches!(target.column_containing(third), Some(Column::Single(entity)) if entity == third)
    );
    assert!(world.get::<NativeMoveOwner>(third).is_none());
    assert!(world.get::<WindowSpaceReassignmentPending>(third).is_none());
}

#[test]
fn native_move_observed_destination_still_reconciles_a_retained_source_layout() {
    let (mut harness, source, members) = column_harness();
    for id in [0, 1] {
        harness
            .mock_state
            .update_window(id, |window| window.workspace_id = TARGET);
    }
    submit(&mut harness, MoveFocus::Stay);
    assert!(
        harness.mock_state.native_space_intents().is_empty(),
        "already confirmed movement must not be submitted twice"
    );
    reconcile(&mut harness);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TARGET)
        .unwrap();
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("the retained source still needs projection into the observed target")
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    for member in members {
        assert!(!world.get::<LayoutStrip>(source).unwrap().contains(member));
        assert!(world.get::<NativeMoveOwner>(member).is_none());
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(member)
                .is_none()
        );
    }
}

#[test]
fn native_move_column_preserves_associated_tab_shape_and_late_floating_mode() {
    for floated in [vec![], vec![2], vec![3], vec![2, 3]] {
        let (mut harness, source, members) = column_harness();
        let extras = add_source_windows(&mut harness, &[2, 3]);
        let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
        strip.append_column(Column::Stack(
            members.into_iter().map(StackItem::Single).collect(),
        ));
        strip.append_column(Column::Tabs(vec![extras[1], extras[0]]));
        harness.world().entity_mut(source).insert(strip);
        harness.mock_state.set_associated_windows(0, vec![2, 3]);
        submit(&mut harness, MoveFocus::Stay);
        for &id in &floated {
            let entity = find_window_entity(id, harness.world());
            harness
                .world()
                .entity_mut(entity)
                .insert(crate::ecs::Floating);
        }
        reconcile(&mut harness);
        let world = harness.world();
        let target = world
            .query::<&LayoutStrip>()
            .iter(world)
            .find(|strip| strip.id() == TARGET)
            .unwrap();
        let surviving = [(3, extras[1]), (2, extras[0])]
            .into_iter()
            .filter_map(|(id, entity)| (!floated.contains(&id)).then_some(entity))
            .collect::<Vec<_>>();
        if let Some(&first) = surviving.first() {
            let column = target.column_containing(first).unwrap();
            assert_eq!(column.window_iter().collect::<Vec<_>>(), surviving);
            assert_eq!(matches!(column, Column::Tabs(_)), surviving.len() == 2);
        }
        let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
            panic!("primary stack preserved")
        };
        assert_eq!(
            items,
            members
                .into_iter()
                .map(StackItem::Single)
                .collect::<Vec<_>>()
        );
        for (id, member) in [
            (0, members[0]),
            (1, members[1]),
            (2, extras[0]),
            (3, extras[1]),
        ] {
            assert_eq!(
                world
                    .query::<&LayoutStrip>()
                    .iter(world)
                    .filter(|strip| strip.contains(member))
                    .count(),
                usize::from(!floated.contains(&id))
            );
            assert!(world.get::<NativeMoveOwner>(member).is_none());
            assert!(
                world
                    .get::<WindowSpaceReassignmentPending>(member)
                    .is_none()
            );
        }
    }
}

fn script_set(harness: &mut TestHarness) -> spool_shared_types::windowset::WindowSet {
    harness
        .world()
        .run_system_once(|state: crate::ecs::state::QueryStateParams| state.extract_window_set())
        .unwrap()
}

fn submit_script_space(harness: &mut TestHarness, plan: spool_shared_types::windowset::LayoutPlan) {
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
}

#[test]
fn script_following_shift_focuses_after_native_confirmation() {
    let (mut harness, _, _) = column_harness();
    harness.mock_state.focus_window(1);
    harness.pump_frames(5);
    let original = script_set(&mut harness);
    assert_eq!(original.focused(), Some(1));
    let predicted = original.shift_following(0, TARGET, true);
    submit_script_space(&mut harness, predicted.plan());

    let pending = script_set(&mut harness);
    assert_eq!(pending.current().unwrap().space_id, TEST_WORKSPACE_ID);
    assert_eq!(pending.focused(), Some(1));
    assert_eq!(
        harness.mock_state.native_space_intents(),
        vec![crate::manager::NativeSpaceIntent::MoveWindows {
            window_ids: vec![0],
            space_id: TARGET
        },]
    );

    harness.pump_frames(30);
    let observed = script_set(&mut harness);
    for set in [&predicted, &observed] {
        assert_eq!(set.current().unwrap().space_id, TARGET);
        assert_eq!(set.workspace_of(0).unwrap().space_id, TARGET);
        assert_eq!(set.focused(), Some(0));
        assert!(set.window(0).unwrap().focused);
        assert!(!set.window(1).unwrap().focused);
        assert!(!set.window(1).unwrap().visible);
    }
    assert_eq!(
        harness.mock_state.native_space_intents(),
        vec![
            crate::manager::NativeSpaceIntent::MoveWindows {
                window_ids: vec![0],
                space_id: TARGET
            },
            crate::manager::NativeSpaceIntent::Focus {
                space_id: TARGET,
                animate: true
            },
        ]
    );
    assert_eq!(
        original.workspace_of(0).unwrap().space_id,
        TEST_WORKSPACE_ID
    );
}

#[cfg(feature = "lua")]
#[test]
fn script_native_move_revalidates_identity_at_native_dispatch() {
    let (mut harness, _, members) = column_harness();
    let plan = script_set(&mut harness).shift(0, TARGET).plan();
    let deferred = Event::LayoutSpaceRequested {
        op: plan.ops[0],
        snapshot: plan.snapshot,
    };
    assert!(harness.mock_state.native_space_intents().is_empty());
    harness.world().despawn(members[0]);
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(0, 20, 400, 700),
    );
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(5);
    harness.world().write_message(deferred);
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    assert!(harness.mock_state.native_space_intents().is_empty());
    assert_eq!(
        harness.mock_state.window_workspace(0),
        Some(TEST_WORKSPACE_ID)
    );
}

#[test]
fn script_native_space_commands_reject_the_previous_session_including_view() {
    let (mut harness, _, _) = column_harness();
    let plan = script_set(&mut harness)
        .shift(0, TARGET)
        .view(TARGET)
        .plan();
    harness
        .world()
        .insert_resource(crate::ecs::layout_snapshot::LayoutSession::default());
    submit_script_space(&mut harness, plan);
    assert!(harness.mock_state.native_space_intents().is_empty());
}

#[test]
fn script_native_move_rejects_replaced_or_unbound_associated_windows() {
    for associated in [1, 99] {
        let (mut harness, _, members) = column_harness();
        let plan = script_set(&mut harness).shift(0, TARGET).plan();
        if associated == 1 {
            harness.world().despawn(members[1]);
            let replacement = harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                1,
                IRect::new(0, 20, 400, 700),
            );
            harness
                .world()
                .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
            harness.pump_frames(5);
        }
        harness
            .mock_state
            .set_associated_windows(0, vec![associated]);
        submit_script_space(&mut harness, plan);
        assert!(
            harness.mock_state.native_space_intents().is_empty(),
            "associated={associated}"
        );
    }
}

#[test]
fn script_native_move_accepts_unchanged_associated_identities() {
    let (mut harness, _, _) = column_harness();
    let plan = script_set(&mut harness).shift(0, TARGET).plan();
    harness.mock_state.set_associated_windows(0, vec![1, 1]);
    submit_script_space(&mut harness, plan);
    assert_eq!(
        harness.mock_state.native_space_intents(),
        vec![crate::manager::NativeSpaceIntent::MoveWindows {
            window_ids: vec![0, 1],
            space_id: TARGET,
        }]
    );
}

#[test]
fn native_move_keeps_floating_windows_outside_layout_strips() {
    for resizable in [true, false] {
        let config = Config::try_from(
            r#"{"options":{"experimental_space_control":true},
                "windows":{"float":{"title":"Floating","floating":true}}}"#,
        )
        .unwrap();
        let mut harness = TestHarness::new()
            .with_config(config)
            .with_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![TEST_WORKSPACE_ID, TARGET],
            )
            .with_window(0, |window| {
                window.title = "Floating".into();
                window.resizable = resizable;
            })
            .with_workspace_window(1, TARGET, |_| {});
        harness.mock_state.enable_native_space_control();
        harness.pump_frames(30);
        let entity = find_window_entity(0, harness.world());
        assert!(
            harness
                .world()
                .get::<crate::ecs::Floating>(entity)
                .is_some()
        );
        let frame = harness.mock_state.actual_window_frame(0);
        let writes = harness.mock_state.frame_write_attempts(0);
        let predicted = harness
            .world()
            .run_system_once(|state: crate::ecs::state::QueryStateParams| {
                state.extract_window_set()
            })
            .unwrap()
            .shift(0, TARGET);

        harness.world().write_message(Event::ActionRequested {
            action: Action::MoveWindowToSpace {
                window_id: 0,
                space_id: TARGET,
                move_focus: MoveFocus::Stay,
            },
        });
        harness.pump_frames(20);

        assert_eq!(harness.mock_state.window_workspace(0), Some(TARGET));
        let world = harness.world();
        assert!(world.get::<crate::ecs::Floating>(entity).is_some());
        assert!(world.get::<NativeMoveOwner>(entity).is_none());
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
        assert!(
            world
                .query::<&LayoutStrip>()
                .iter(world)
                .all(|strip| !strip.contains(entity)),
            "native movement cannot give a floating window a tiled slot"
        );
        assert_eq!(harness.mock_state.actual_window_frame(0), frame);
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
        let actual = harness
            .world()
            .run_system_once(|state: crate::ecs::state::QueryStateParams| {
                state.extract_window_set()
            })
            .unwrap();
        for snapshot in [predicted, actual] {
            assert_eq!(snapshot.workspace_of(0).unwrap().space_id, TARGET);
            assert!(snapshot.window(0).unwrap().floating);
            assert!(
                snapshot
                    .workspace(TARGET)
                    .unwrap()
                    .floating
                    .iter()
                    .any(|window| window.id == 0)
            );
        }
    }
}

#[test]
fn native_move_waits_for_unique_membership_before_committing_a_column() {
    let (mut harness, source, members) = column_harness();
    submit(&mut harness, MoveFocus::Stay);
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, [Ok(vec![0])]);
    reconcile(&mut harness);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<LayoutStrip>(source)
                .unwrap()
                .contains(entity)
        );
        assert!(harness.world().get::<NativeMoveOwner>(entity).is_some());
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_some()
        );
    }
    harness
        .mock_state
        .script_workspace_membership_queries(TEST_WORKSPACE_ID, []);
    reconcile(&mut harness);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TARGET)
        .unwrap();
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("stack preserved");
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    for entity in members {
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
}

#[test]
fn native_move_does_not_append_a_column_with_a_replaced_member() {
    let (mut harness, source, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    harness.world().despawn(members[1]);
    let replacement =
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TARGET, 1, IRect::new(0, 20, 400, 700));
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    reconcile(&mut harness);
    let world = harness.world();
    assert!(
        world
            .query::<&LayoutStrip>()
            .iter(world)
            .all(|strip| !strip.contains(members[1])),
        "captured column data must never resurrect a retired entity"
    );
    assert!(
        world
            .get::<LayoutStrip>(source)
            .unwrap()
            .contains(members[0])
    );
    assert!(world.get::<NativeMoveOwner>(members[0]).is_none());
    assert!(
        world
            .get::<WindowSpaceReassignmentPending>(members[0])
            .is_some()
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        1,
        "do not follow a broken transaction"
    );
}

#[test]
fn partial_column_move_times_out_into_membership_recovery_without_os_rollback() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    harness
        .mock_state
        .update_window(1, |window| window.workspace_id = TEST_WORKSPACE_ID);
    harness.pump_frames(15);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_some()
        );
    }
    harness.pump_frames(30);
    let world = harness.world();
    let mut strips = world.query::<&LayoutStrip>();
    assert!(
        strips
            .iter(world)
            .any(|strip| strip.id() == TARGET && strip.contains(members[0]))
    );
    assert!(
        strips
            .iter(world)
            .any(|strip| strip.id() == TEST_WORKSPACE_ID && strip.contains(members[1]))
    );
    for entity in members {
        assert!(world.get::<NativeMoveOwner>(entity).is_none());
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_recovery_preserves_source_stack_after_timeout() {
    let (mut harness, source, members) = column_harness();
    let Some(Column::Stack(before)) = harness
        .world()
        .get::<LayoutStrip>(source)
        .unwrap()
        .column_containing(members[0])
    else {
        panic!("source stack")
    };
    submit(&mut harness, MoveFocus::Stay);
    for id in 0..2 {
        harness.mock_state.update_window(id, |window| {
            window.workspace_id = TEST_WORKSPACE_ID;
        });
    }
    harness.pump_frames(45);
    for entity in members {
        assert!(
            harness
                .world()
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
        assert!(harness.world().get::<NativeMoveOwner>(entity).is_none());
    }
    let strip = harness.world().get::<LayoutStrip>(source).unwrap();
    let Some(Column::Stack(after)) = strip.column_containing(members[0]) else {
        panic!("the unchanged source column must remain a stack: {strip:?}");
    };
    assert_eq!(after, before);
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_recovery_preserves_column_at_an_unrequested_destination() {
    const ACTUAL: WorkspaceId = TARGET + 1;
    let (mut harness, _, members) = column_harness();
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, ACTUAL, false);
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(3);
    submit(&mut harness, MoveFocus::Follow);
    for id in 0..2 {
        harness
            .mock_state
            .update_window(id, |window| window.workspace_id = ACTUAL);
    }
    harness.pump_frames(45);
    let world = harness.world();
    let target = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == ACTUAL)
        .unwrap();
    let Some(Column::Stack(items)) = target.column_containing(members[0]) else {
        panic!("members sharing an actual destination must retain their column");
    };
    assert_eq!(
        items,
        members
            .into_iter()
            .map(StackItem::Single)
            .collect::<Vec<_>>()
    );
    for entity in members {
        assert!(
            world
                .get::<WindowSpaceReassignmentPending>(entity)
                .is_none()
        );
    }
    assert_eq!(harness.mock_state.native_space_intents().len(), 1);
}

#[test]
fn native_move_follow_does_not_focus_a_reused_window_id() {
    let (mut harness, _, members) = column_harness();
    submit(&mut harness, MoveFocus::Follow);
    reconcile(&mut harness);
    assert_eq!(harness.mock_state.native_space_intents().len(), 2);
    harness.world().despawn(members[0]);
    let replacement =
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TARGET, 0, IRect::new(0, 20, 400, 700));
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    let target = {
        let world = harness.world();
        world
            .query::<(Entity, &LayoutStrip)>()
            .iter(world)
            .find(|(_, strip)| strip.id() == TARGET)
            .unwrap()
            .0
    };
    harness
        .world()
        .entity_mut(target)
        .insert(native_space::VisibleNativeSpaceMarker);
    harness.mock_state.take_focus_requests();
    reconcile(&mut harness);
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

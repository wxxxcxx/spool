use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use spool_shared_types::state::Frame;
use spool_shared_types::windowset::{LayoutOp, LayoutPlan};

use crate::commands::Action;
use crate::ecs::Floating;
use crate::ecs::layout::LayoutStrip;
use crate::events::Event;
use crate::platform::WinID;

use super::*;

fn columns(harness: &mut TestHarness) -> Vec<Vec<WinID>> {
    let world = harness.world();
    let strip = world.query::<&LayoutStrip>().single(world).unwrap();
    strip
        .columns()
        .map(|column| {
            column
                .window_iter()
                .map(|entity| world.get::<Window>(entity).unwrap().id())
                .collect()
        })
        .collect()
}

fn replay(harness: &mut TestHarness, ops: Vec<LayoutOp>) {
    let plan = plan(harness, ops);
    replay_plan(harness, plan);
}

fn plan(harness: &mut TestHarness, ops: Vec<LayoutOp>) -> LayoutPlan {
    let mut plan = snapshot(harness).plan();
    plan.ops = ops;
    plan
}

fn replay_plan(harness: &mut TestHarness, plan: LayoutPlan) {
    harness
        .world()
        .write_message(Event::action_requested(Action::Layout(plan)));
    harness.pump_frames(5);
}

fn snapshot(harness: &mut TestHarness) -> spool_shared_types::windowset::WindowSet {
    harness
        .world()
        .run_system_once(|state: crate::ecs::state::QueryStateParams| state.extract_window_set())
        .unwrap()
}

fn replace_window(harness: &mut TestHarness, id: WinID) {
    let old = find_window_entity(id, harness.world());
    harness.world().despawn(old);
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        id,
        IRect::new(0, 20, 400, 700),
    );
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(5);
    assert_ne!(find_window_entity(id, harness.world()), old);
}

#[test]
fn script_snapshot_does_not_float_a_reused_window_id() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    let lua = mlua::Lua::new();
    lua.globals().set("ws", snapshot(&mut harness)).unwrap();
    let result: mlua::Value = lua.load("return ws:float(1)").eval().unwrap();
    let ops = spool_shared_types::windowset_lua::returned_plan(&result).unwrap();
    replace_window(&mut harness, 1);
    let before = columns(&mut harness);
    replay_plan(&mut harness, ops);
    let entity = find_window_entity(1, harness.world());
    assert!(harness.world().get::<Floating>(entity).is_none());
    assert_eq!(columns(&mut harness), before);
}

#[test]
fn script_unstack_rejects_an_overflowing_proposed_strip() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    replay(&mut harness, vec![stack(1, 0)]);
    let members = [
        find_window_entity(0, harness.world()),
        find_window_entity(1, harness.world()),
    ];
    {
        let world = harness.world();
        let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
        let id = strip.column_id(members[0]).unwrap();
        strip
            .set_width_intent(
                id,
                crate::ecs::layout::WidthIntent::Absolute(f64::from(i32::MAX / 2)),
            )
            .unwrap();
    }
    let plan = snapshot(&mut harness).unstack(1).plan();
    let before = actual_items(&mut harness);
    harness
        .world()
        .write_message(Event::action_requested(Action::Layout(plan)));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    assert_eq!(actual_items(&mut harness), before);
    assert!(
        harness
            .world()
            .get::<crate::ecs::ReshuffleAroundMarker>(members[1])
            .is_none()
    );
    {
        let world = harness.world();
        let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
        let id = strip.column_id(members[0]).unwrap();
        strip
            .set_width_intent(id, crate::ecs::layout::WidthIntent::Absolute(400.0))
            .unwrap();
    }
    harness.world().resource_mut::<Messages<Event>>().clear();
    let plan = snapshot(&mut harness).unstack(1).plan();
    replay_plan(&mut harness, plan);
    assert_eq!(columns(&mut harness), vec![vec![0], vec![1], vec![2]]);
}

#[test]
fn script_snapshot_does_not_swap_with_a_reused_secondary_id() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    let lua = mlua::Lua::new();
    lua.globals().set("ws", snapshot(&mut harness)).unwrap();
    let result: mlua::Value = lua.load("return ws:swap(0, 1)").eval().unwrap();
    let ops = spool_shared_types::windowset_lua::returned_plan(&result).unwrap();
    replace_window(&mut harness, 1);
    let before = columns(&mut harness);
    replay_plan(&mut harness, ops);
    assert_eq!(columns(&mut harness), before);
}

#[test]
fn script_snapshot_rejects_a_new_incarnation_on_the_same_entity() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    let plan = snapshot(&mut harness).float(1).plan();
    let entity = find_window_entity(1, harness.world());
    let replacement = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        1,
        IRect::new(0, 20, 400, 700),
    );
    assert_ne!(
        replacement.incarnation(),
        plan.snapshot.windows[&1].incarnation
    );
    harness.world().entity_mut(entity).insert(replacement);
    replay_plan(&mut harness, plan);
    assert_eq!(find_window_entity(1, harness.world()), entity);
    assert!(harness.world().get::<Floating>(entity).is_none());
}

#[test]
fn script_snapshot_rejects_a_new_entity_with_the_same_incarnation() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    let plan = snapshot(&mut harness).float(1).plan();
    let old = find_window_entity(1, harness.world());
    let replacement = harness.mock_state.create_window(1);
    assert_eq!(
        replacement.incarnation(),
        plan.snapshot.windows[&1].incarnation
    );
    harness.world().despawn(old);
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![replacement]));
    harness.pump_frames(5);
    replay_plan(&mut harness, plan);
    let entity = find_window_entity(1, harness.world());
    assert_ne!(entity, old);
    assert!(harness.world().get::<Floating>(entity).is_none());
}

#[test]
fn script_snapshot_rejects_operations_from_a_previous_daemon_session() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    let plan = snapshot(&mut harness).float(1).plan();
    harness
        .world()
        .insert_resource(crate::ecs::layout_snapshot::LayoutSession::default());
    let fresh = snapshot(&mut harness).plan();
    assert_eq!(fresh.snapshot.windows, plan.snapshot.windows);
    assert_ne!(fresh.snapshot.session, plan.snapshot.session);
    replay_plan(&mut harness, plan);
    let entity = find_window_entity(1, harness.world());
    assert!(harness.world().get::<Floating>(entity).is_none());
}

#[test]
fn script_snapshot_skips_stale_ops_without_discarding_valid_following_ops() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    let plan = snapshot(&mut harness).float(1).float(2).plan();
    replace_window(&mut harness, 1);
    replay_plan(&mut harness, plan);
    let replaced = find_window_entity(1, harness.world());
    let unaffected = find_window_entity(2, harness.world());
    assert!(harness.world().get::<Floating>(replaced).is_none());
    assert!(harness.world().get::<Floating>(unaffected).is_some());
}

#[test]
fn script_snapshot_does_not_bind_a_window_that_appears_after_capture() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    let plan = snapshot(&mut harness).float(9).plan();
    assert!(!plan.snapshot.windows.contains_key(&9));
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        9,
        IRect::new(0, 20, 400, 700),
    );
    harness
        .world()
        .trigger(crate::ecs::SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(5);
    replay_plan(&mut harness, plan);
    let entity = find_window_entity(9, harness.world());
    assert!(harness.world().get::<Floating>(entity).is_none());
}

#[test]
fn script_snapshot_checks_every_named_endpoint_before_layout_or_frame_changes() {
    for op in [
        LayoutOp::Focus(1),
        LayoutOp::Swap(1, 0),
        LayoutOp::SetWidth {
            window: 1,
            ratio: 0.75,
        },
        LayoutOp::SetFrame {
            window: 1,
            frame: Frame {
                x: 10,
                y: 20,
                width: 300,
                height: 400,
            },
        },
        stack(1, 0),
        stack(0, 1),
        LayoutOp::Unstack(1),
    ] {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        replay(&mut harness, vec![stack(1, 0)]);
        let plan = plan(&mut harness, vec![op]);
        replace_window(&mut harness, 1);
        replay(&mut harness, vec![stack(1, 0)]);
        let before = actual_items(&mut harness);
        let frames = frame_inputs(&mut harness);
        harness.mock_state.take_focus_requests();
        replay_plan(&mut harness, plan);
        assert_eq!(actual_items(&mut harness), before, "{op:?}");
        assert_eq!(frame_inputs(&mut harness), frames, "{op:?}");
        assert!(
            harness.mock_state.take_focus_requests().is_empty(),
            "{op:?}"
        );
    }
}

#[test]
fn script_snapshot_rejects_replaced_implicit_tab_and_column_members() {
    for op in [
        LayoutOp::Swap(1, 0),
        stack(1, 0),
        LayoutOp::Unstack(1),
        LayoutOp::SetWidth {
            window: 1,
            ratio: 0.75,
        },
    ] {
        let mut harness = nested_tabs_harness();
        let plan = plan(&mut harness, vec![op]);
        replace_window(&mut harness, 2);
        let group = [
            find_window_entity(1, harness.world()),
            find_window_entity(2, harness.world()),
        ];
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .append_tab_group(&group);
        replay(&mut harness, vec![stack(1, 0)]);
        let before = actual_items(&mut harness);
        let frames = frame_inputs(&mut harness);
        replay_plan(&mut harness, plan);
        assert_eq!(actual_items(&mut harness), before, "{op:?}");
        assert_eq!(frame_inputs(&mut harness), frames, "{op:?}");
    }
}

#[test]
fn script_snapshot_serialization_keeps_the_original_replay_binding() {
    use spool_shared_types::wire::{Request, Response};
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    let response = Response::WindowSet(Box::new(snapshot(&mut harness)));
    let bytes = serde_json::to_vec(&response).unwrap();
    let Response::WindowSet(set) = serde_json::from_slice(&bytes).unwrap() else {
        panic!("window set response")
    };
    let request = Request::WindowSetApply(set.float(1).plan());
    let bytes = serde_json::to_vec(&request).unwrap();
    let Request::WindowSetApply(plan) = serde_json::from_slice(&bytes).unwrap() else {
        panic!("layout request")
    };
    replace_window(&mut harness, 1);
    replay_plan(&mut harness, plan);
    let entity = find_window_entity(1, harness.world());
    assert!(harness.world().get::<Floating>(entity).is_none());
}

fn predicted_columns(set: &spool_shared_types::windowset::WindowSet) -> Vec<Vec<WinID>> {
    set.workspace(TEST_WORKSPACE_ID)
        .unwrap()
        .columns
        .iter()
        .map(|column| column.windows().map(|window| window.id).collect())
        .collect()
}

fn nested_tabs_harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(6);
    harness.pump_frames(5);
    for group in [[1, 2], [4, 5]] {
        let members = group.map(|id| find_window_entity(id, harness.world()));
        let world = harness.world();
        world
            .query::<&mut LayoutStrip>()
            .single_mut(world)
            .unwrap()
            .append_tab_group(&members);
    }
    replay(&mut harness, vec![stack(1, 0), stack(4, 3)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 1, 2], vec![3, 4, 5]]);
    harness
}

fn predicted_items(set: &spool_shared_types::windowset::WindowSet) -> Vec<Vec<Vec<WinID>>> {
    set.workspace(TEST_WORKSPACE_ID)
        .unwrap()
        .columns
        .iter()
        .map(|column| {
            column
                .items
                .iter()
                .map(|item| item.windows().iter().map(|window| window.id).collect())
                .collect()
        })
        .collect()
}

fn actual_items(harness: &mut TestHarness) -> Vec<Vec<Vec<WinID>>> {
    use crate::ecs::layout::Column;

    let world = harness.world();
    world
        .query::<&LayoutStrip>()
        .single(world)
        .unwrap()
        .columns()
        .map(|column| {
            let ids = |entities: Vec<Entity>| {
                entities
                    .into_iter()
                    .map(|entity| world.get::<Window>(entity).unwrap().id())
                    .collect()
            };
            match column {
                Column::Stack(items) => items
                    .iter()
                    .map(|item| ids(item.window_iter().collect()))
                    .collect(),
                _ => vec![ids(column.window_iter().collect())],
            }
        })
        .collect()
}

#[test]
fn script_nested_tabs_chained_predictions_match_the_complete_layout_structure() {
    let mut harness = nested_tabs_harness();
    let before = snapshot(&mut harness);
    assert_eq!(predicted_items(&before), actual_items(&mut harness));
    let predicted = before.swap(2, 3).stack(4, 0).unstack(1).swap(5, 2);
    replay_plan(&mut harness, predicted.plan());
    assert_eq!(predicted_items(&predicted), actual_items(&mut harness));
    assert_eq!(
        predicted_items(&before),
        vec![vec![vec![0], vec![1, 2]], vec![vec![3], vec![4, 5]]]
    );
}

#[test]
fn script_nested_tabs_repeated_transforms_match_the_complete_layout_structure() {
    let mut harness = nested_tabs_harness();
    for window in 0..6 {
        for other in 0..6 {
            for op in [
                stack(window, other),
                LayoutOp::Swap(window, other),
                LayoutOp::Unstack(window),
            ] {
                let before = snapshot(&mut harness);
                assert_eq!(predicted_items(&before), actual_items(&mut harness));
                let predicted = match op {
                    LayoutOp::Stack { window, onto, .. } => before.stack(window, onto),
                    LayoutOp::Swap(window, other) => before.swap(window, other),
                    LayoutOp::Unstack(window) => before.unstack(window),
                    _ => unreachable!(),
                };
                replay_plan(&mut harness, predicted.plan());
                assert_eq!(
                    predicted_items(&predicted),
                    actual_items(&mut harness),
                    "{op:?}"
                );
                assert_eq!(predicted.windows().count(), 6);
            }
        }
    }
}

#[test]
fn script_nested_tabs_lua_columns_stay_flat_while_transforms_keep_groups() {
    let mut harness = nested_tabs_harness();
    let lua = mlua::Lua::new();
    lua.globals().set("ws", snapshot(&mut harness)).unwrap();
    let result: mlua::Value = lua
        .load(
            r"
        local columns = ws:columns()
        assert(#columns == 2 and #columns[1] == 3)
        assert(columns[1][1] == 0 and columns[1][2] == 1 and columns[1][3] == 2)
        local next = ws:swap(2, 3):unstack(1)
        local changed = next:columns()
        assert(#changed == 3 and #changed[3] == 2)
        assert(changed[3][1] == 1 and changed[3][2] == 2)
        return next
    ",
        )
        .eval()
        .unwrap();
    let ops = spool_shared_types::windowset_lua::returned_plan(&result).unwrap();
    replay_plan(&mut harness, ops);
    let mlua::Value::UserData(result) = result else {
        panic!("window set")
    };
    let predicted = result
        .borrow::<spool_shared_types::windowset::WindowSet>()
        .unwrap();
    assert_eq!(predicted_items(&predicted), actual_items(&mut harness));
}

#[test]
fn script_nested_tabs_swap_prediction_preserves_whole_entries() {
    let mut harness = nested_tabs_harness();
    let predicted = snapshot(&mut harness).swap(2, 3);
    replay_plan(&mut harness, predicted.plan());
    assert_eq!(columns(&mut harness), vec![vec![0, 3], vec![1, 2, 4, 5]]);
    assert_eq!(predicted_columns(&predicted), columns(&mut harness));
}

#[test]
fn script_nested_tabs_stack_prediction_preserves_whole_entries() {
    let mut harness = nested_tabs_harness();
    let predicted = snapshot(&mut harness).stack(2, 3);
    replay_plan(&mut harness, predicted.plan());
    assert_eq!(columns(&mut harness), vec![vec![0], vec![3, 4, 5, 1, 2]]);
    assert_eq!(predicted_columns(&predicted), columns(&mut harness));
}

#[test]
fn script_nested_tabs_unstack_prediction_preserves_whole_entries() {
    let mut harness = nested_tabs_harness();
    let predicted = snapshot(&mut harness).unstack(2);
    replay_plan(&mut harness, predicted.plan());
    assert_eq!(
        columns(&mut harness),
        vec![vec![0], vec![1, 2], vec![3, 4, 5]]
    );
    assert_eq!(predicted_columns(&predicted), columns(&mut harness));
}

#[test]
fn script_nested_tabs_swap_within_one_group_is_a_prediction_noop() {
    let mut harness = nested_tabs_harness();
    let before = snapshot(&mut harness);
    let predicted = before.swap(1, 2);
    assert_eq!(predicted, before);
    replay_plan(&mut harness, predicted.plan());
    assert_eq!(predicted_columns(&predicted), columns(&mut harness));
}

fn stack(window: WinID, onto: WinID) -> LayoutOp {
    LayoutOp::Stack {
        window,
        onto,
        tabs: false,
    }
}

fn frame_inputs(harness: &mut TestHarness) -> (IVec2, IVec2, crate::ecs::DesiredWindowFrame) {
    let entity = find_window_entity(0, harness.world());
    let world = harness.world();
    (
        world.get::<crate::ecs::Position>(entity).unwrap().0,
        world.get::<crate::ecs::Bounds>(entity).unwrap().0,
        *world.get::<crate::ecs::DesiredWindowFrame>(entity).unwrap(),
    )
}

#[test]
fn script_swap_exchanges_named_entries_within_a_stack() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    replay(&mut harness, vec![stack(1, 0), LayoutOp::Swap(0, 1)]);
    assert_eq!(columns(&mut harness), vec![vec![1, 0], vec![2]]);
    crate::assert_focused!(harness.world(), 0);
}

#[test]
fn script_swap_leaves_unrelated_stacked_siblings_in_place() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![stack(1, 0), stack(3, 2), LayoutOp::Swap(1, 2)],
    );
    assert_eq!(columns(&mut harness), vec![vec![0, 2], vec![1, 3]]);
}

#[test]
fn script_swap_keeps_the_destination_column_widths() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![
            stack(1, 0),
            stack(3, 2),
            LayoutOp::SetWidth {
                window: 0,
                ratio: 0.25,
            },
            LayoutOp::SetWidth {
                window: 2,
                ratio: 0.75,
            },
        ],
    );
    replay(&mut harness, vec![LayoutOp::Swap(1, 2)]);
    for (id, width) in [(0, 256), (2, 256), (1, 768), (3, 768)] {
        assert_eq!(
            harness.mock_state.actual_window_frame(id).unwrap().width(),
            width,
            "window {id}"
        );
    }
}

#[test]
fn script_swap_moves_native_tabs_as_one_stack_item() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    let first = find_window_entity(2, harness.world());
    let second = find_window_entity(3, harness.world());
    let world = harness.world();
    world
        .query::<&mut LayoutStrip>()
        .single_mut(world)
        .unwrap()
        .append_tab_group(&[first, second]);
    replay(&mut harness, vec![stack(1, 0), LayoutOp::Swap(1, 3)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 2, 3], vec![1]]);
    replay(&mut harness, vec![LayoutOp::Swap(2, 3)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 2, 3], vec![1]]);
    let world = harness.world();
    assert_eq!(
        world
            .query::<&LayoutStrip>()
            .single(world)
            .unwrap()
            .tab_group(first),
        Some(vec![first, second])
    );
}

#[test]
fn script_width_uses_the_owner_display_instead_of_the_focused_display() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(1024, 0, 2944, 1200),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(1, EXT_WORKSPACE_ID, |_| {})
        .with_focused_window(0);
    harness.pump_frames(10);
    replay(
        &mut harness,
        vec![LayoutOp::SetWidth {
            window: 1,
            ratio: 0.5,
        }],
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().width(),
        960
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        TEST_DISPLAY_WIDTH / 4
    );
    crate::assert_focused!(harness.world(), 0);
}

#[test]
fn script_mode_predictions_keep_the_unfocused_display_membership() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(1024, 0, 2944, 1200),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(1, EXT_WORKSPACE_ID, |_| {})
        .with_focused_window(0);
    harness.pump_frames(10);
    for floating in [true, true, false, false] {
        let before = snapshot(&mut harness);
        let predicted = if floating {
            before.float(1)
        } else {
            before.sink(1)
        };
        replay_plan(&mut harness, predicted.plan());
        let after = snapshot(&mut harness);
        for set in [&predicted, &after] {
            assert_eq!(set.workspace_of(1).unwrap().space_id, EXT_WORKSPACE_ID);
            assert_eq!(set.display_of(1).unwrap().id, EXT_DISPLAY_ID);
            assert_eq!(set.window(1).unwrap().floating, floating);
            assert_eq!(set.windows().count(), 2);
        }
        crate::assert_focused!(harness.world(), 0);
    }
}

#[test]
fn script_width_does_not_overflow_the_strip_offsets() {
    for scripted in [false, true] {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let before = frame_inputs(&mut harness);
        let entity = find_window_entity(0, harness.world());
        harness
            .world()
            .entity_mut(entity)
            .insert(crate::ecs::FullWidthMarker {
                width_ratio: 0.5,
                floating_frame: None,
            });
        let ratio = f64::from(i32::MAX) / 1024.0;
        if scripted {
            replay(&mut harness, vec![LayoutOp::SetWidth { window: 0, ratio }]);
        } else {
            harness
                .world()
                .write_message(Event::action_requested(Action::Window(
                    crate::commands::Operation::SetWidth(ratio),
                )));
            harness.pump_frames(5);
        }
        assert_eq!(frame_inputs(&mut harness), before);
        assert!(
            harness
                .world()
                .get::<crate::ecs::FullWidthMarker>(entity)
                .is_some()
        );
    }
}

#[test]
fn script_width_allows_columns_wider_than_the_display() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![LayoutOp::SetWidth {
            window: 1,
            ratio: 2.0,
        }],
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().width(),
        2048
    );
}

#[test]
fn script_width_preserves_an_earlier_queued_height_request() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    let entity = find_window_entity(0, harness.world());
    harness
        .world()
        .entity_mut(entity)
        .insert(crate::ecs::ResizeMarker(IVec2::new(400, 250)));
    let plan = plan(
        &mut harness,
        vec![LayoutOp::SetWidth {
            window: 0,
            ratio: 0.5,
        }],
    );
    harness
        .world()
        .write_message(Event::action_requested(Action::Layout(plan)));
    harness
        .world()
        .run_system_once(crate::commands::dispatch_actions)
        .unwrap();
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::ResizeMarker>(entity)
            .unwrap()
            .0,
        IVec2::new(400, 250)
    );
}

#[test]
fn script_width_resizes_the_column_not_just_its_reported_ratio() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![
            stack(1, 0),
            LayoutOp::SetWidth {
                window: 1,
                ratio: 0.75,
            },
        ],
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        768
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().width(),
        768
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(2).unwrap().width(),
        TEST_DISPLAY_WIDTH / 4
    );
}

#[test]
fn script_width_rejects_unrepresentable_values_without_mutating_state() {
    for ratio in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::MIN_POSITIVE,
        f64::MAX,
    ] {
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        let before = {
            let world = harness.world();
            let strip = world.query::<&LayoutStrip>().single(world).unwrap();
            strip
                .column_state(strip.index_of(entity).unwrap())
                .unwrap()
                .width
        };
        let frame = harness.mock_state.actual_window_frame(0).unwrap();
        let writes = harness.mock_state.frame_write_attempts(0);
        replay(&mut harness, vec![LayoutOp::SetWidth { window: 0, ratio }]);
        {
            let world = harness.world();
            let strip = world.query::<&LayoutStrip>().single(world).unwrap();
            assert_eq!(
                strip
                    .column_state(strip.index_of(entity).unwrap())
                    .unwrap()
                    .width,
                before,
                "{ratio}"
            );
        }
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(frame));
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    }
}

#[test]
fn script_stack_uses_the_named_target_on_either_side() {
    for (window, onto, expected) in [
        (2, 0, vec![vec![0, 2], vec![1], vec![3]]),
        (0, 2, vec![vec![1], vec![2, 0], vec![3]]),
    ] {
        let mut harness = TestHarness::new().with_windows(4);
        harness.pump_frames(5);
        assert_eq!(
            columns(&mut harness),
            vec![vec![0], vec![1], vec![2], vec![3]]
        );
        replay(&mut harness, vec![stack(window, onto)]);
        assert_eq!(columns(&mut harness), expected, "stack({window}, {onto})");
    }
}

#[test]
fn script_stack_moves_only_the_named_stack_item() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    replay(&mut harness, vec![stack(2, 1)]);
    assert_eq!(columns(&mut harness), vec![vec![0], vec![1, 2], vec![3]]);
    replay(&mut harness, vec![stack(2, 0)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 2], vec![1], vec![3]]);
}

#[test]
fn script_stack_preserves_a_native_tab_group() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    let first = find_window_entity(2, harness.world());
    let second = find_window_entity(3, harness.world());
    let world = harness.world();
    world
        .query::<&mut LayoutStrip>()
        .single_mut(world)
        .unwrap()
        .append_tab_group(&[first, second]);

    replay(&mut harness, vec![stack(3, 0)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 2, 3], vec![1]]);
    let world = harness.world();
    assert_eq!(
        world
            .query::<&LayoutStrip>()
            .single(world)
            .unwrap()
            .tab_group(first),
        Some(vec![first, second])
    );

    replay(&mut harness, vec![stack(3, 1)]);
    assert_eq!(columns(&mut harness), vec![vec![0], vec![1, 2, 3]]);
    replay(&mut harness, vec![LayoutOp::Unstack(3)]);
    assert_eq!(columns(&mut harness), vec![vec![0], vec![1], vec![2, 3]]);
}

#[test]
fn script_stack_self_missing_and_same_column_targets_do_not_disturb_other_ops() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    replay(&mut harness, vec![stack(2, 2), stack(2, 99), stack(99, 0)]);
    assert_eq!(
        columns(&mut harness),
        vec![vec![0], vec![1], vec![2], vec![3]]
    );
    replay(&mut harness, vec![stack(2, 0), stack(2, 0), stack(3, 1)]);
    assert_eq!(columns(&mut harness), vec![vec![0, 2], vec![1, 3]]);
}

#[test]
fn script_frame_overflow_does_not_mutate_floating_geometry() {
    for frame in [
        Frame {
            x: i32::MAX,
            y: 100,
            width: 1,
            height: 100,
        },
        Frame {
            x: 100,
            y: i32::MAX,
            width: 100,
            height: 1,
        },
        Frame {
            x: i32::MAX,
            y: 100,
            width: 0,
            height: 100,
        },
    ] {
        let mut rule = crate::config::WindowParams::new(".*", None);
        rule.floating = Some(true);
        let mut harness = TestHarness::new()
            .with_config((crate::config::MainOptions::default(), vec![rule]).into())
            .with_windows(1);
        harness.pump_frames(5);
        let before = harness.mock_state.actual_window_frame(0).unwrap();
        let writes = harness.mock_state.frame_write_attempts(0);
        let inputs = frame_inputs(&mut harness);
        replay(&mut harness, vec![LayoutOp::SetFrame { window: 0, frame }]);
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(before));
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
        assert_eq!(frame_inputs(&mut harness), inputs);
    }
}

#[test]
fn script_stack_does_not_rearrange_floating_windows() {
    for (window, onto) in [(2, 1), (1, 0)] {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        replay(
            &mut harness,
            vec![LayoutOp::SetFloating {
                window: 1,
                floating: true,
            }],
        );
        let before = columns(&mut harness);
        replay(&mut harness, vec![stack(window, onto)]);
        assert_eq!(columns(&mut harness), before);
    }
}

#[test]
fn script_stack_respects_pending_floating_requests_across_batches() {
    for separate_batches in [false, true] {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(5);
        let float = LayoutOp::SetFloating {
            window: 0,
            floating: true,
        };
        if separate_batches {
            let float_plan = plan(&mut harness, vec![float]);
            let stack_plan = plan(&mut harness, vec![stack(2, 0)]);
            harness
                .world()
                .write_message(Event::action_requested(Action::Layout(float_plan)));
            harness
                .world()
                .write_message(Event::action_requested(Action::Layout(stack_plan)));
            harness.pump_frames(5);
        } else {
            replay(&mut harness, vec![float, stack(2, 0)]);
        }
        assert_eq!(
            columns(&mut harness),
            vec![vec![1], vec![2]],
            "separate batches: {separate_batches}"
        );
    }
}

#[test]
fn script_stack_can_follow_a_pending_sink_request() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![LayoutOp::SetFloating {
            window: 0,
            floating: true,
        }],
    );
    replay(
        &mut harness,
        vec![
            LayoutOp::SetFloating {
                window: 0,
                floating: false,
            },
            stack(0, 1),
        ],
    );
    assert_eq!(columns(&mut harness), vec![vec![1, 0], vec![2]]);
}

#[test]
fn script_stack_cannot_bypass_a_refused_sink() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    replay(
        &mut harness,
        vec![LayoutOp::SetFloating {
            window: 0,
            floating: true,
        }],
    );
    harness
        .mock_state
        .update_window(0, |window| window.resizable = false);
    replay(
        &mut harness,
        vec![
            LayoutOp::SetFloating {
                window: 0,
                floating: false,
            },
            stack(0, 1),
        ],
    );
    assert_eq!(columns(&mut harness), vec![vec![1], vec![2]]);
    let entity = find_window_entity(0, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::Floating>(entity)
            .is_some()
    );
}

#[test]
fn script_invalid_frame_does_not_discard_an_earlier_valid_request() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(5);
    let valid = Frame {
        x: 100,
        y: 100,
        width: 300,
        height: 200,
    };
    replay(
        &mut harness,
        vec![
            LayoutOp::SetFloating {
                window: 0,
                floating: true,
            },
            LayoutOp::SetFrame {
                window: 0,
                frame: valid,
            },
            LayoutOp::SetFrame {
                window: 0,
                frame: Frame {
                    x: i32::MAX,
                    ..valid
                },
            },
        ],
    );
    let entity = find_window_entity(0, harness.world());
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::DesiredWindowFrame>(entity)
            .unwrap()
            .0,
        IRect::new(100, 100, 400, 300)
    );
}

#[test]
fn script_stack_and_unstack_predictions_match_replayed_column_order() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(5);
    for window in 0..4 {
        for onto in 0..4 {
            for op in [stack(window, onto), LayoutOp::Unstack(onto)] {
                let before = harness
                    .world()
                    .run_system_once(|state: crate::ecs::state::QueryStateParams| {
                        state.extract_window_set()
                    })
                    .unwrap();
                let predicted = match op {
                    LayoutOp::Stack { window, onto, .. } => before.stack(window, onto),
                    LayoutOp::Unstack(window) => before.unstack(window),
                    _ => unreachable!(),
                };
                let expected: Vec<Vec<WinID>> = predicted
                    .workspace(TEST_WORKSPACE_ID)
                    .unwrap()
                    .columns
                    .iter()
                    .map(|column| column.windows().map(|window| window.id).collect())
                    .collect();
                replay_plan(&mut harness, predicted.plan());
                assert_eq!(columns(&mut harness), expected, "{op:?}");
            }
        }
    }
}

#[test]
fn script_frame_accepts_negative_origins_and_normalizes_empty_dimensions() {
    let mut rule = crate::config::WindowParams::new(".*", None);
    rule.floating = Some(true);
    let mut harness = TestHarness::new()
        .with_config((crate::config::MainOptions::default(), vec![rule]).into())
        .with_windows(1);
    harness.pump_frames(5);
    let entity = find_window_entity(0, harness.world());
    for (width, height) in [(300, 200), (0, -1)] {
        replay(
            &mut harness,
            vec![LayoutOp::SetFrame {
                window: 0,
                frame: Frame {
                    x: -400,
                    y: -300,
                    width,
                    height,
                },
            }],
        );
        assert_eq!(
            harness
                .world()
                .get::<crate::ecs::DesiredWindowFrame>(entity)
                .unwrap()
                .0,
            IRect::new(-400, -300, -400 + width.max(1), -300 + height.max(1))
        );
    }
}

#[test]
fn stale_arrangement_cannot_overwrite_a_newer_structural_edit() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(10);
    let stale = plan(&mut harness, vec![stack(1, 0)]);
    replay(&mut harness, vec![LayoutOp::Swap(0, 2)]);
    let current = columns(&mut harness);
    replay_plan(&mut harness, stale);
    assert_eq!(columns(&mut harness), current);
}

#[test]
fn unavailable_retained_identity_can_be_rearranged_by_a_fresh_script() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(10);
    harness.mock_state.os_withdraw_window(1);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(5);
    let withdrawn = find_window_entity(1, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(withdrawn)
            .is_some()
    );
    let plan = plan(&mut harness, vec![stack(1, 0)]);
    let writes = harness.mock_state.frame_write_attempts(1);
    harness
        .world()
        .run_system_cached_with(crate::ecs::layout_ops::apply_layout_plan, plan)
        .unwrap();
    assert_eq!(columns(&mut harness), vec![vec![0, 1], vec![2]]);
    assert_eq!(harness.mock_state.frame_write_attempts(1), writes);
}

#[test]
fn arrangement_does_not_need_a_native_display_parent() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(10);
    let plan = plan(
        &mut harness,
        vec![stack(1, 0), LayoutOp::Unstack(1), LayoutOp::Swap(1, 2)],
    );
    let world = harness.world();
    let strip = world
        .query_filtered::<Entity, With<LayoutStrip>>()
        .single(world)
        .unwrap();
    world.entity_mut(strip).remove::<ChildOf>();
    world
        .run_system_cached_with(crate::ecs::layout_ops::apply_layout_plan, plan)
        .unwrap();
    assert_eq!(columns(&mut harness), vec![vec![0], vec![2], vec![1]]);
}

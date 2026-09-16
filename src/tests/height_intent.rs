use super::*;
use crate::commands::{Action, Operation, ResizeAxis, ResizeDirection};
use crate::ecs::layout::LayoutStrip;
use crate::events::Event;
use bevy::prelude::*;
use spool_shared_types::commands::SpaceLayoutOperation;

fn action(harness: &mut TestHarness, action: Action) {
    harness
        .world()
        .write_message(Event::action_requested(action));
    harness.world().run_schedule(PreUpdate);
}
fn stack(harness: &mut TestHarness, id: i32) {
    action(
        harness,
        Action::TargetedWindow {
            window_id: id,
            operation: Operation::ToggleStack,
        },
    );
}
fn owner(harness: &mut TestHarness, id: i32) -> (Entity, Entity) {
    let window = find_window_entity(id, harness.world());
    let world = harness.world();
    let strip = world
        .query::<(Entity, &LayoutStrip)>()
        .iter(world)
        .find(|(_, strip)| strip.contains(window))
        .unwrap()
        .0;
    (strip, window)
}

#[test]
fn background_stack_and_last_item_height_realize_only_latest_intent() {
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, 77],
        )
        .with_workspace_window(0, 77, |_| {})
        .with_workspace_window(1, 77, |_| {});
    harness.pump_frames(20);
    let writes = [
        harness.mock_state.frame_write_attempts(0),
        harness.mock_state.frame_write_attempts(1),
    ];
    harness.mock_state.take_focus_requests();
    stack(&mut harness, 1);
    let (owner, window) = owner(&mut harness, 1);
    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    assert_eq!(strip.len(), 1);
    assert_eq!(strip.column_height_items(0).unwrap().len(), 2);
    let viewport = strip.height_viewport().unwrap();
    let initial = strip.effective_stack_heights(0, Some(viewport)).unwrap()[1].requested;
    let step = f64::from(
        harness
            .world()
            .resource::<crate::config::Config>()
            .floating_window_resize_step(),
    );
    for _ in 0..2 {
        action(
            &mut harness,
            Action::TargetedWindow {
                window_id: 1,
                operation: Operation::Resize {
                    axis: ResizeAxis::Height,
                    direction: ResizeDirection::Grow,
                },
            },
        );
    }
    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    let heights = strip.effective_stack_heights(0, Some(viewport)).unwrap();
    assert!((heights[1].requested - initial - 2.0 * step).abs() < 0.01);
    let expected = heights[1].slot;
    let latest = strip.height_state(window).unwrap().clone();
    harness.pump_frames(20);
    assert_eq!(
        [
            harness.mock_state.frame_write_attempts(0),
            harness.mock_state.frame_write_attempts(1)
        ],
        writes
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
    assert!(harness.mock_state.native_space_intents().is_empty());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, 77, false);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(90);
    assert_eq!(
        harness.mock_state.actual_window_frame(1).unwrap().height(),
        expected
    );
    let current = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .height_state(window)
        .unwrap();
    assert_eq!(current.id, latest.id);
    assert_eq!(current.weight.to_bits(), latest.weight.to_bits());
}

#[test]
fn infeasible_stack_blocks_old_frames_without_deleting_members() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.pump_frames(15);
    let writes = (0..4)
        .map(|id| harness.mock_state.frame_write_attempts(id))
        .collect::<Vec<_>>();
    harness.mock_state.take_focus_requests();
    for id in 1..4 {
        stack(&mut harness, id);
    }
    let (owner, _) = owner(&mut harness, 0);
    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    assert_eq!(strip.len(), 1);
    assert_eq!(strip.all_windows().len(), 4);
    assert!(strip.height_projection_is_blocked());
    harness.pump_frames(20);
    assert_eq!(
        (0..4)
            .map(|id| harness.mock_state.frame_write_attempts(id))
            .collect::<Vec<_>>(),
        writes
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn equalize_without_viewport_changes_only_raw_height_intent() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    stack(&mut harness, 1);
    let (owner, window) = owner(&mut harness, 1);
    {
        let mut strip = harness.world().get_mut::<LayoutStrip>(owner).unwrap();
        strip.set_height_weight(window, 3.0).unwrap();
        strip.set_height_context(None);
    }
    harness.world().entity_mut(owner).remove::<ChildOf>();
    let writes = harness.mock_state.frame_write_attempts(1);
    action(
        &mut harness,
        Action::SpaceLayout {
            space_id: Some(TEST_WORKSPACE_ID),
            operation: SpaceLayoutOperation::Equalize { column: Some(1) },
        },
    );
    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    assert!(
        strip
            .column_height_items(0)
            .unwrap()
            .iter()
            .all(|item| item.weight.to_bits() == 1.0_f64.to_bits())
    );
    assert_eq!(harness.mock_state.frame_write_attempts(1), writes);
}

fn ordered_out_entities(harness: &mut TestHarness) -> std::collections::HashSet<Entity> {
    let world = harness.world();
    world
        .query_filtered::<Entity, With<crate::ecs::reconcile::WindowUnavailable>>()
        .iter(world)
        .filter(|entity| {
            world
                .get::<crate::ecs::reconcile::WindowUnavailable>(*entity)
                .is_some_and(
                    crate::ecs::reconcile::WindowUnavailable::excludes_from_layout_projection,
                )
        })
        .collect()
}

/// A drag on one participant must not readmit, corrupt, or consult an
/// ordered-out sibling: the adopted weights keep the excluded raw weight and
/// the participating cohort keeps its own total weight units.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered-out cohort assertion covers exclusion, donation and preservation"
)]
fn external_height_adoption_ignores_an_ordered_out_middle_member() {
    let config: crate::config::Config = (
        crate::config::MainOptions {
            animation_speed: Some(10_000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(3);
    harness.pump_frames(30);
    for id in 1..3 {
        stack(&mut harness, id);
    }
    let (owner, _) = owner(&mut harness, 0);
    {
        let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
        assert_eq!(strip.column_height_items(0).unwrap().len(), 3);
        let viewport = strip.height_viewport().unwrap();
        let heights = strip.effective_stack_heights(0, Some(viewport)).unwrap();
        assert_eq!(heights.len(), 3);
        assert_eq!(
            heights.iter().map(|height| height.slot).sum::<i32>(),
            viewport
        );
    }
    // Order the middle member out through the real lifecycle, not by hand.
    harness.mock_state.update_window(1, |window| {
        window.published = false;
        window.ordered_out = true;
        window.visible = false;
    });
    harness.pump_frames(40);
    let middle = find_window_entity(1, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(middle)
            .is_some_and(crate::ecs::reconcile::WindowUnavailable::excludes_from_layout_projection),
        "the middle member must be ordered out before adoption is exercised"
    );
    let excluded_before = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .height_state(middle)
        .unwrap()
        .weight;
    let viewport = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .height_viewport()
        .unwrap();
    let ordered_out = ordered_out_entities(&mut harness);
    let visible = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .effective_stack_heights_for(0, Some(viewport), &|entity| !ordered_out.contains(&entity))
        .unwrap();
    let half = viewport / 2;
    let mut expected = vec![half, half];
    expected[usize::try_from(viewport % 2).unwrap()] += viewport % 2;
    // Deterministic remainder assignment puts the extra point on the first item.
    expected = vec![viewport - viewport / 2, viewport / 2];
    assert_eq!(
        visible
            .iter()
            .map(|(_, height)| height.slot)
            .collect::<Vec<_>>(),
        expected
    );

    // Bottom-edge drag on the lower participant grows only the participants.
    let lower = find_window_entity(2, harness.world());
    let lower_before = harness.mock_state.actual_window_frame(2).unwrap();
    let incarnation = harness.world().get::<Window>(lower).unwrap().incarnation();
    let holder = harness
        .world()
        .spawn(crate::ecs::MouseHeldMarker(lower))
        .id();
    harness.mock_state.os_set_window_frame_silently(
        2,
        IRect::from_corners(lower_before.min, lower_before.max + IVec2::new(0, 200)),
    );
    harness.world().write_message(Event::WindowResized {
        window_id: 2,
        incarnation,
    });
    harness.pump_frames(5);
    harness.world().entity_mut(holder).despawn();
    harness.pump_frames(20);

    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    assert_eq!(
        strip.height_state(middle).unwrap().weight.to_bits(),
        excluded_before.to_bits(),
        "an ordered-out member's raw height weight must survive an external drag"
    );
    let total = strip
        .column_height_items(0)
        .unwrap()
        .iter()
        .filter(|item| item.id != strip.height_state(middle).unwrap().id)
        .map(|item| item.weight)
        .sum::<f64>();
    assert!(
        (total - 2.0).abs() < 0.000_001,
        "the participating cohort must keep its raw weight units, got {total}"
    );
}

/// A top-edge drag takes space from the previous *participant*, not from a
/// retained sibling that happens to sit between them in the strip.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one top-edge assertion covers the donor choice, weight units and clamped projection"
)]
fn external_top_edge_adoption_donates_from_the_previous_participant() {
    let config: crate::config::Config = (
        crate::config::MainOptions {
            animation_speed: Some(10_000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let mut harness = TestHarness::new().with_config(config).with_windows(3);
    harness.pump_frames(30);
    for id in 1..3 {
        stack(&mut harness, id);
    }
    let (owner, _) = owner(&mut harness, 0);
    harness.mock_state.update_window(1, |window| {
        window.published = false;
        window.ordered_out = true;
        window.visible = false;
    });
    harness.pump_frames(40);
    let middle = find_window_entity(1, harness.world());
    let excluded_before = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .height_state(middle)
        .unwrap()
        .weight;
    let viewport = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .height_viewport()
        .unwrap();
    let ordered_out = ordered_out_entities(&mut harness);
    let (weights_before, middle_before) = {
        let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
        let middle_id = strip.height_state(middle).unwrap().id;
        let items = strip.column_height_items(0).unwrap();
        (
            items.iter().map(|item| item.weight).collect::<Vec<_>>(),
            items.iter().position(|item| item.id == middle_id).unwrap(),
        )
    };

    // Top-edge drag on the last participant: shift its top edge up by 200.
    let lower = find_window_entity(2, harness.world());
    let lower_before = harness.mock_state.actual_window_frame(2).unwrap();
    let incarnation = harness.world().get::<Window>(lower).unwrap().incarnation();
    let holder = harness
        .world()
        .spawn(crate::ecs::MouseHeldMarker(lower))
        .id();
    harness.mock_state.os_set_window_frame_silently(
        2,
        IRect::from_corners(lower_before.min + IVec2::new(0, -200), lower_before.max),
    );
    harness.world().write_message(Event::WindowResized {
        window_id: 2,
        incarnation,
    });
    harness.pump_frames(5);
    harness.world().entity_mut(holder).despawn();
    harness.pump_frames(20);

    let projection = harness
        .world()
        .get::<LayoutStrip>(owner)
        .unwrap()
        .effective_stack_heights_for(0, Some(viewport), &|entity| !ordered_out.contains(&entity))
        .unwrap()
        .into_iter()
        .map(|(_, height)| height.slot)
        .collect::<Vec<_>>();
    let strip = harness.world().get::<LayoutStrip>(owner).unwrap();
    // The participating donor is the first retained item; the ordered-out
    // middle item must not move even though it sits directly above the target.
    let after = strip
        .column_height_items(0)
        .unwrap()
        .iter()
        .map(|item| item.weight)
        .collect::<Vec<_>>();
    assert_eq!(
        after[middle_before].to_bits(),
        weights_before[middle_before].to_bits(),
        "the ordered-out member must keep its exact raw weight"
    );
    assert_eq!(after[middle_before].to_bits(), excluded_before.to_bits());
    let donors = (0..after.len())
        .filter(|index| *index != middle_before && after[*index] < weights_before[*index])
        .collect::<Vec<_>>();
    let growth = (0..after.len())
        .filter(|index| *index != middle_before && after[*index] > weights_before[*index])
        .collect::<Vec<_>>();
    assert_eq!(
        donors.len(),
        1,
        "exactly the previous participant donates, got {donors:?}"
    );
    assert_eq!(growth.len(), 1, "exactly the dragged item grows");
    assert_eq!(
        donors[0], 0,
        "the ordered-out middle member must not be the donor"
    );
    assert_eq!(
        growth[0],
        middle_before + 1,
        "the dragged item is the lower one"
    );
    assert!(
        (weights_before[donors[0]]
            - after[donors[0]]
            - (after[growth[0]] - weights_before[growth[0]]))
            .abs()
            < 0.000_001,
        "the donated and received weight units must match"
    );
    // The 200-point drag moves that much across the boundary between the two
    // participants: the donor requests half minus the delta and the dragged
    // item requests half plus the delta, while every untouched share holds.
    let half = f64::from(viewport) / 2.0;
    let heights = strip
        .effective_stack_heights_for(0, Some(viewport), &|entity| !ordered_out.contains(&entity))
        .unwrap();
    assert!(
        (heights[0].1.requested - (half - 200.0)).abs() < 0.000_001,
        "the donor requests half the viewport minus the drag, got {:?}",
        heights[0].1
    );
    assert!(
        (heights[1].1.requested - (half + 200.0)).abs() < 0.000_001,
        "the dragged item requests half the viewport plus the drag, got {:?}",
        heights[1].1
    );
    assert_eq!(
        heights.iter().map(|(_, height)| height.slot).sum::<i32>(),
        viewport
    );
    // The donor's requested 174 points is below the 200-point stack minimum,
    // so the water fill pins it there and the dragged item takes the rest.
    assert_eq!(heights[0].1.slot, 200);
    assert_eq!(heights[1].1.slot, viewport - 200);
    assert_eq!(projection.len(), 2);
}

/// A height edit the display never realized follows the display once its round is
/// over: the item's authored weight becomes the one that derives the height the
/// window actually shows, so state and display agree again (ADR 0011). Before
/// that decision the edited share was kept forever.
#[test]
fn a_constrained_height_edit_aligns_to_what_the_window_shows() {
    use crate::ecs::alignment::{AlignedField, AlignmentReason, RealizationAlignments};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(15);
    stack(&mut harness, 1);
    harness.pump_frames(10);
    let (owner, window) = owner(&mut harness, 1);
    let (viewport, prior_weight) = {
        let strip = harness.world().get::<LayoutStrip>(owner).expect("strip");
        (
            strip.height_viewport().expect("a known viewport"),
            strip.height_state(window).expect("item state").weight,
        )
    };
    let observed_height = harness
        .world()
        .get::<crate::ecs::Bounds>(window)
        .expect("bounds")
        .0
        .y;
    assert!(observed_height > 0);

    // The platform stops moving either window, then the owner asks for a much
    // taller item. The split cannot change, so the round never reaches its target.
    harness.mock_state.constrain_frame_writes(0, true);
    harness.mock_state.constrain_frame_writes(1, true);
    harness
        .world()
        .get_mut::<LayoutStrip>(owner)
        .expect("strip")
        .set_height_weight(window, prior_weight * 4.0)
        .expect("weight edit");
    harness.pump_frames(60);

    let (weight, derived) = {
        let strip = harness.world().get::<LayoutStrip>(owner).expect("strip");
        let item = strip.height_state(window).expect("item state");
        let heights = strip
            .effective_stack_heights_for(0, Some(viewport), &|_| true)
            .expect("a complete projection");
        let derived = heights
            .iter()
            .find(|(id, _)| *id == item.id)
            .map(|(_, height)| height.slot)
            .expect("the item's derived height");
        (item.weight, derived)
    };
    assert!(
        (weight - prior_weight).abs() < 0.001,
        "the authored weight follows the displayed split: {weight} vs {prior_weight}"
    );
    assert_eq!(
        derived, observed_height,
        "and it derives exactly the height the window shows"
    );
    let records = {
        let alignments = harness.world().resource::<RealizationAlignments>();
        alignments
            .records(window)
            .map(|record| (record.field, record.reason))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        records,
        vec![(
            AlignedField::StackItemHeight {
                item: {
                    let strip = harness.world().get::<LayoutStrip>(owner).expect("strip");
                    strip.height_state(window).expect("item state").id
                },
                prior: prior_weight * 4.0,
                adopted: weight,
            },
            AlignmentReason::NotRealizedWithinGrace,
        )],
        "one alignment is recorded, with both values"
    );
}

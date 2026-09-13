use super::*;
use crate::commands::Action;
use crate::ecs::layout::{LayoutStrip, WidthConstraint, WidthIntent};
use crate::events::Event;
use bevy::prelude::*;
use spool_shared_types::commands::{ColumnWidth, SpaceLayoutOperation};

#[test]
fn startup_new_column_preserves_initial_window_width() {
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(0, 0, 900, TEST_WINDOW_HEIGHT);
    });
    harness.pump_frames(90);
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        900
    );
    let world = harness.world();
    let strip = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap();
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::Absolute(900.0)
    );
}

#[test]
fn late_new_column_preserves_its_own_width_across_config_reload() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(20);
    harness = harness.with_window(1, |window| {
        window.frame = IRect::new(0, 0, 700, TEST_WINDOW_HEIGHT);
    });
    harness.pump_frames(90);
    let config: crate::config::Config = (
        crate::config::MainOptions {
            preset_column_widths: vec![0.75],
            ..Default::default()
        },
        vec![],
    )
        .into();
    harness.world().insert_resource(config);
    harness.pump_frames(90);
    for (id, width) in [(0, TEST_WINDOW_WIDTH), (1, 700)] {
        assert_eq!(
            harness.mock_state.actual_window_frame(id).unwrap().width(),
            width
        );
    }
}

fn edit(harness: &mut TestHarness, space: u64, column: usize, width: ColumnWidth) {
    harness
        .world()
        .write_message(Event::action_requested(Action::SpaceLayout {
            space_id: Some(space),
            operation: SpaceLayoutOperation::SetWidth { column, width },
        }));
    harness.world().run_schedule(PreUpdate);
}

#[test]
fn background_latest_intent_is_admitted_without_native_effects_or_focus() {
    let mut harness = TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            vec![TEST_WORKSPACE_ID, 77],
        )
        .with_workspace_window(0, 77, |_| {});
    harness.pump_frames(20);
    let window = find_window_entity(0, harness.world());
    let retained = {
        let world = harness.world();
        world
            .query::<(Entity, &LayoutStrip)>()
            .iter(world)
            .find(|(_, strip)| strip.id() == 77)
            .unwrap()
            .0
    };
    let column = harness
        .world()
        .get::<LayoutStrip>(retained)
        .unwrap()
        .column_id(window)
        .unwrap();
    let writes = harness.mock_state.frame_write_attempts(0);
    harness.mock_state.take_focus_requests();
    edit(&mut harness, 77, 1, ColumnWidth::Points(800.0));
    edit(&mut harness, 77, 1, ColumnWidth::Points(900.0));
    let strip = harness.world().get::<LayoutStrip>(retained).unwrap();
    assert_eq!(strip.column_state(0).unwrap().id, column);
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::Absolute(900.0)
    );
    assert_eq!(strip.column_state(0).unwrap().intent_revision, 2);
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    assert!(harness.mock_state.take_focus_requests().is_empty());
    harness.pump_frames(20);
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    assert!(harness.mock_state.native_space_intents().is_empty());
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, 77, false);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(90);
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        900
    );
}

#[test]
fn background_ratio_survives_an_unknown_viewport() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);
    let window = find_window_entity(0, harness.world());
    let mut strip = LayoutStrip::new(77);
    {
        let world = harness.world();
        for mut owner in world.query::<&mut LayoutStrip>().iter_mut(world) {
            owner.remove(window);
        }
    }
    strip.insert_at(0, window);
    let retained = harness.world().spawn(strip).id();
    edit(&mut harness, 77, 1, ColumnWidth::Ratio(0.75));
    let strip = harness.world().get::<LayoutStrip>(retained).unwrap();
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::ViewportRatio(0.75)
    );
    assert_eq!(
        strip.effective_column_width(0),
        Err(crate::ecs::layout::WidthProjectionBlocked::UnknownViewport)
    );
}

#[test]
fn accepted_width_projects_and_constraints_do_not_overwrite_intent() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness.mock_state.take_focus_requests();
    edit(
        &mut harness,
        TEST_WORKSPACE_ID,
        1,
        ColumnWidth::Points(800.0),
    );
    let world = harness.world();
    let mut strips = world.query::<&mut LayoutStrip>();
    let mut strip = strips
        .iter_mut(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap();
    let id = strip.column_state(0).unwrap().id;
    strip
        .set_column_constraints(
            id,
            vec![WidthConstraint::Interval {
                min: 1000.0,
                max: 1200.0,
            }],
        )
        .unwrap();
    assert_eq!(strip.effective_column_width(0).unwrap().slot, 1000);
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::Absolute(800.0)
    );
    strip.set_column_constraints(id, vec![]).unwrap();
    harness.pump_frames(90);
    let entity = find_window_entity(0, harness.world());
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::DesiredWindowFrame>(entity)
            .unwrap()
            .0
            .width(),
        800
    );
    assert!(harness.mock_state.take_focus_requests().is_empty());
}

#[test]
fn same_width_does_not_advance_intent_revision_and_maximize_restores_raw_variant() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);
    edit(
        &mut harness,
        TEST_WORKSPACE_ID,
        1,
        ColumnWidth::Points(800.0),
    );
    edit(
        &mut harness,
        TEST_WORKSPACE_ID,
        1,
        ColumnWidth::Points(800.0),
    );
    let world = harness.world();
    let mut strips = world.query::<&mut LayoutStrip>();
    let mut strip = strips
        .iter_mut(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID)
        .unwrap();
    let state = strip.column_state(0).unwrap();
    assert_eq!(state.intent_revision, 1);
    let id = state.id;
    strip.toggle_full_width(id).unwrap();
    strip.toggle_full_width(id).unwrap();
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::Absolute(800.0)
    );
}

#[test]
fn constrained_realization_does_not_turn_external_noise_into_original_intent() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);
    edit(
        &mut harness,
        TEST_WORKSPACE_ID,
        1,
        ColumnWidth::Points(800.0),
    );
    {
        let world = harness.world();
        let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
        let id = strip.column_state(0).unwrap().id;
        strip
            .set_column_constraints(
                id,
                vec![WidthConstraint::Interval {
                    min: 1000.0,
                    max: 1200.0,
                }],
            )
            .unwrap();
    }
    harness.pump_frames(90);
    assert_eq!(
        harness.mock_state.actual_window_frame(0).unwrap().width(),
        1000
    );
    harness
        .mock_state
        .os_resize_window(0, IVec2::new(1020, 748));
    harness.pump_frames(30);
    let world = harness.world();
    let strip = world.query::<&LayoutStrip>().single(world).unwrap();
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::Absolute(800.0)
    );
}

#[test]
fn unavailable_identity_accepts_width_without_a_native_write() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(10);
    harness.mock_state.os_withdraw_window(0);
    harness.world().write_message(Event::SpaceChanged);
    harness.pump_frames(5);
    let entity = find_window_entity(0, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::reconcile::WindowUnavailable>(entity)
            .is_some()
    );
    let writes = harness.mock_state.frame_write_attempts(0);
    harness
        .world()
        .write_message(Event::action_requested(Action::TargetedWindow {
            window_id: 0,
            operation: crate::commands::Operation::SetWidth(0.75),
        }));
    harness.world().run_schedule(PreUpdate);
    let world = harness.world();
    let strip = world.query::<&LayoutStrip>().single(world).unwrap();
    assert_eq!(
        strip.column_state(0).unwrap().width,
        WidthIntent::ViewportRatio(0.75)
    );
    assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
}

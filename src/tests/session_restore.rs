//! The first intent slice has no cross-daemon binding provider. These tests
//! exercise candidate isolation and explicitly supplied mock mappings only.

use bevy::prelude::World;

use crate::ecs::layout::{ColumnId, LayoutStrip, WidthIntent};
use crate::ecs::restore::{RestoreCandidates, TrustedColumnBinding, import_intents};
use crate::ecs::state::{INTENT_STATE_VERSION, SavedColumn, SavedSpace, SpoolState};
use crate::tests::{TEST_WORKSPACE_ID, TestHarness};

fn candidates(widths: &[WidthIntent]) -> RestoreCandidates {
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 7,
        floating: Vec::new(),
        spaces: vec![SavedSpace {
            // Intentionally different from the current Space. Matching is
            // supplied externally, not guessed from a coincident numeric ID.
            space_id: TEST_WORKSPACE_ID + 100,
            columns: widths
                .iter()
                .enumerate()
                .map(|(index, width)| SavedColumn {
                    column_id: ColumnId(u64::try_from(index).unwrap() + 100),
                    width: *width,
                    kind: spool_shared_types::windowset::ColumnKind::Single,
                    items: vec![crate::ecs::state::SavedItem {
                        item_id: 1000 + u64::try_from(index).unwrap(),
                        weight: 1.0,
                        tabs: false,
                        members: Vec::new(),
                    }],
                })
                .collect(),
        }],
    }
    .into()
}

fn strip() -> LayoutStrip {
    let mut world = World::new();
    let mut strip = LayoutStrip::new(TEST_WORKSPACE_ID);
    strip.append(world.spawn_empty().id());
    strip.append(world.spawn_empty().id());
    strip
}

#[test]
fn trusted_mock_mapping_imports_raw_width_without_recreating_columns() {
    let mut target = strip();
    let original_windows = target.all_windows();
    let ids = target
        .column_states()
        .map(|state| state.id)
        .collect::<Vec<_>>();
    let structure_revision = target.structure_revision();
    let mut candidates = candidates(&[
        WidthIntent::Absolute(800.0),
        WidthIntent::ViewportRatio(0.75),
    ]);
    candidates.freeze_initial_layouts([&target]).unwrap();
    let mapping = vec![
        TrustedColumnBinding::new(0, 0, &candidates, &target, ids[1]).unwrap(),
        TrustedColumnBinding::new(0, 1, &candidates, &target, ids[0]).unwrap(),
    ];
    assert_eq!(
        import_intents(&candidates, &mut target, &mapping).unwrap(),
        2
    );
    assert_eq!(
        target.column_state(0).unwrap().width,
        WidthIntent::ViewportRatio(0.75)
    );
    assert_eq!(
        target.column_state(1).unwrap().width,
        WidthIntent::Absolute(800.0)
    );
    assert_eq!(target.all_windows(), original_windows);
    assert_eq!(target.structure_revision(), structure_revision);
    assert_eq!(
        target
            .column_states()
            .map(|state| state.id)
            .collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn late_import_cannot_replace_an_edit_before_or_after_binding() {
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    let mut target = strip();
    candidates.freeze_initial_layouts([&target]).unwrap();
    let id = target.column_state(0).unwrap().id;
    let mapping = TrustedColumnBinding::new(0, 0, &candidates, &target, id).unwrap();
    target
        .set_width_intent(id, WidthIntent::Absolute(900.0))
        .unwrap();
    assert!(import_intents(&candidates, &mut target, &[mapping]).is_err());
    assert_eq!(
        target.column_state(0).unwrap().width,
        WidthIntent::Absolute(900.0)
    );
    assert!(TrustedColumnBinding::new(0, 0, &candidates, &target, id).is_err());
}

#[test]
fn invalid_mapping_does_not_partially_import_an_earlier_width() {
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    let mut target = strip();
    candidates.freeze_initial_layouts([&target]).unwrap();
    let mapping = vec![
        TrustedColumnBinding::new(
            0,
            0,
            &candidates,
            &target,
            target.column_state(0).unwrap().id,
        )
        .unwrap(),
        TrustedColumnBinding::new(
            0,
            9,
            &candidates,
            &target,
            target.column_state(1).unwrap().id,
        )
        .unwrap(),
    ];
    assert!(import_intents(&candidates, &mut target, &mapping).is_err());
    assert!(
        target
            .column_states()
            .all(|state| state.width == WidthIntent::InheritConfig && state.intent_revision == 0)
    );
}

#[test]
fn structural_edit_or_expired_candidates_reject_frozen_import() {
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    let mut target = strip();
    candidates.freeze_initial_layouts([&target]).unwrap();
    let mapping = TrustedColumnBinding::new(
        0,
        0,
        &candidates,
        &target,
        target.column_state(0).unwrap().id,
    )
    .unwrap();
    target.swap(0, 1);
    assert!(import_intents(&candidates, &mut target, &[mapping]).is_err());
    assert!(
        TrustedColumnBinding::new(
            0,
            0,
            &candidates,
            &target,
            target.column_state(0).unwrap().id
        )
        .is_err()
    );
    assert!(candidates.freeze_initial_layouts([&target]).is_err());
    candidates.close_import_window();
    assert!(import_intents(&candidates, &mut target, &[]).is_err());
    assert!(
        target
            .column_states()
            .all(|state| state.width == WidthIntent::InheritConfig)
    );
}

#[test]
fn loading_candidates_does_not_bind_or_apply_old_widths() {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(30);
    let before = harness.mock_state.frame_write_attempts(0);
    let before_frame = harness.mock_state.actual_window_frame(0);
    let (before_width, before_id) = {
        let mut query = harness.world().query::<&LayoutStrip>();
        let strip = query.single(harness.world()).unwrap();
        (
            strip.column_state(0).unwrap().width,
            strip.column_state(0).unwrap().id,
        )
    };
    harness
        .world()
        .insert_resource(candidates(&[WidthIntent::Absolute(13.0)]));
    harness.pump_frames(10);
    let mut query = harness.world().query::<&LayoutStrip>();
    let strip = query.single(harness.world()).unwrap();
    assert_eq!(strip.column_state(0).unwrap().width, before_width);
    assert_eq!(strip.column_state(0).unwrap().id, before_id);
    assert_eq!(harness.mock_state.frame_write_attempts(0), before);
    assert_eq!(harness.mock_state.actual_window_frame(0), before_frame);
    assert_eq!(
        harness
            .world()
            .resource::<RestoreCandidates>()
            .state()
            .revision,
        7
    );
}

#[test]
fn a_mapping_arriving_after_user_split_cannot_refresh_the_startup_baseline() {
    let mut target = strip();
    let member = target.all_windows()[1];
    target.stack(member).unwrap();
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    candidates.freeze_initial_layouts([&target]).unwrap();
    target.unstack(member).unwrap();
    let child = target.column_id(member).unwrap();
    assert_eq!(
        target
            .column_state(target.index_of(member).unwrap())
            .unwrap()
            .intent_revision,
        0
    );
    assert!(
        TrustedColumnBinding::new(0, 0, &candidates, &target, child).is_err(),
        "even an inherited, revision-zero split column is absent from the frozen initial set"
    );
    assert!(
        candidates.freeze_initial_layouts([&target]).is_err(),
        "a late binding must not manufacture a newer baseline after the edit"
    );
    assert!(
        target
            .column_states()
            .all(|state| state.width == WidthIntent::InheritConfig)
    );
}

#[test]
fn candidates_alone_cannot_construct_an_import_binding() {
    let target = strip();
    let candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    assert!(
        TrustedColumnBinding::new(
            0,
            0,
            &candidates,
            &target,
            target.column_state(0).unwrap().id
        )
        .is_err()
    );
}

#[test]
fn expired_startup_window_rejects_a_previously_valid_mapping() {
    let mut target = strip();
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    candidates.freeze_initial_layouts([&target]).unwrap();
    let mapping = TrustedColumnBinding::new(
        0,
        0,
        &candidates,
        &target,
        target.column_state(0).unwrap().id,
    )
    .unwrap();
    candidates.close_import_window();
    assert!(import_intents(&candidates, &mut target, &[mapping]).is_err());
    assert_eq!(
        target.column_state(0).unwrap().width,
        WidthIntent::InheritConfig
    );
}

#[test]
fn trusted_height_mapping_imports_raw_weight_and_rejects_later_edits() {
    let mut target = strip();
    let entity = target.all_windows()[0];
    let id = target.column_state(0).unwrap().id;
    let mut saved = candidates(&[WidthIntent::Absolute(800.0)]).state().clone();
    saved.spaces[0].columns[0].items[0].weight = 3.0;
    let mut candidates = RestoreCandidates::from(saved);
    candidates.freeze_initial_layouts([&target]).unwrap();
    let binding = TrustedColumnBinding::new(0, 0, &candidates, &target, id)
        .unwrap()
        .with_heights(&candidates, &target, &[(0, entity)])
        .unwrap();
    let mut edited = target.clone();
    edited.set_height_weight(entity, 2.0).unwrap();
    let before_width = edited.column_state(0).unwrap().width;
    assert!(import_intents(&candidates, &mut edited, std::slice::from_ref(&binding)).is_err());
    assert_eq!(
        edited.height_state(entity).unwrap().weight.to_bits(),
        2.0_f64.to_bits()
    );
    assert_eq!(edited.column_state(0).unwrap().width, before_width);
    assert_eq!(
        import_intents(&candidates, &mut target, &[binding]).unwrap(),
        2
    );
    assert_eq!(
        target.height_state(entity).unwrap().weight.to_bits(),
        3.0_f64.to_bits()
    );
}

#[test]
fn height_edits_before_binding_cannot_be_imported_over() {
    let mut target = strip();
    let entity = target.all_windows()[0];
    let id = target.column_state(0).unwrap().id;
    let mut candidates = candidates(&[WidthIntent::Absolute(800.0)]);
    candidates.freeze_initial_layouts([&target]).unwrap();
    target.set_height_weight(entity, 2.0).unwrap();
    let binding = TrustedColumnBinding::new(0, 0, &candidates, &target, id).unwrap();
    assert!(
        binding
            .clone()
            .with_heights(&candidates, &target, &[(0, entity)])
            .is_err()
    );
    assert!(import_intents(&candidates, &mut target, &[binding]).is_err());
}

#[test]
fn height_import_requires_matching_arrangement_and_complete_trusted_slots() {
    let mut target = strip();
    let windows = target.all_windows();
    target.stack(windows[1]).unwrap();
    let id = target.column_state(0).unwrap().id;
    let mut candidate = candidates(&[WidthIntent::Absolute(800.0)]);
    candidate.freeze_initial_layouts([&target]).unwrap();
    let binding = TrustedColumnBinding::new(0, 0, &candidate, &target, id).unwrap();
    assert!(
        binding
            .with_heights(&candidate, &target, &[(0, windows[0])])
            .is_err(),
        "a saved single is not a proved stack mapping"
    );

    let mut saved = SpoolState::from_layouts([&target], |_| None, std::iter::empty());
    saved.spaces[0].columns[0].items[0].weight = 2.0;
    let mut candidate = RestoreCandidates::from(saved);
    candidate.freeze_initial_layouts([&target]).unwrap();
    let binding = TrustedColumnBinding::new(0, 0, &candidate, &target, id).unwrap();
    assert!(
        binding
            .clone()
            .with_heights(&candidate, &target, &[(0, windows[0])])
            .is_err()
    );
    let binding = binding
        .with_heights(&candidate, &target, &[(0, windows[0]), (1, windows[1])])
        .unwrap();
    assert_eq!(
        import_intents(&candidate, &mut target, &[binding]).unwrap(),
        1
    );
    assert_eq!(
        target.height_state(windows[0]).unwrap().weight.to_bits(),
        2.0_f64.to_bits()
    );
}

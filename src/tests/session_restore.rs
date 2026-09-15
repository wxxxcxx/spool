//! The first intent slice has no cross-daemon binding provider. These tests
//! exercise candidate isolation and explicitly supplied mock mappings only.

use bevy::prelude::World;

use crate::ecs::layout::{ColumnId, LayoutStrip, WidthIntent};
use crate::ecs::restore::{RestoreCandidates, TrustedColumnBinding, import_intents};
use crate::ecs::state::{INTENT_STATE_VERSION, SavedColumn, SavedSpace, SpoolState};
use crate::tests::{TEST_PROCESS_ID, TEST_WORKSPACE_ID, TestHarness};

fn candidates(widths: &[WidthIntent]) -> RestoreCandidates {
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 7,
        floating: Vec::new(),
        membership: Vec::new(),
        focus: Vec::new(),
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

    let mut saved = SpoolState::from_layouts(
        [&target],
        |_| None,
        std::iter::empty(),
        std::iter::empty(),
        std::iter::empty(),
    );
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

// --- Trusted intent import with a runtime owner (issue 26) ------------------

/// A candidate document with one floating window, whose cached identity is the
/// harness's first window.
fn floating_candidates(pid: i32, bundle_id: &str) -> RestoreCandidates {
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 7,
        floating: vec![crate::ecs::state::SavedFloatingWindow {
            window_id: 0,
            pid,
            bundle_id: bundle_id.into(),
            frame: spool_shared_types::state::Frame {
                x: 240,
                y: 120,
                width: 400,
                height: 400,
            },
        }],
        membership: Vec::new(),
        focus: Vec::new(),
        spaces: Vec::new(),
    }
    .into()
}

/// The startup owner freezes the baseline on its own, and a corroborated binding
/// imports the frame as retained intent.
#[test]
fn a_trusted_floating_binding_imports_the_candidate_frame() {
    use spool_shared_types::commands::{RestoreBindings, RestoreFloatingBinding};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(floating_candidates(TEST_PROCESS_ID, "test"));
    // The owner freezes the baseline once the session can be read.
    harness.pump_frames(2);
    assert_eq!(
        harness
            .world()
            .resource::<RestoreCandidates>()
            .state()
            .floating
            .len(),
        1
    );

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: vec![RestoreFloatingBinding {
                candidate_window: 0,
                target_window_id: 0,
            }],
            membership: Vec::new(),
            focus: Vec::new(),
        },
    );
    assert!(result.is_ok(), "{result:?}");

    let entity = crate::tests::find_window_entity(0, harness.world());
    let geometry = harness
        .world()
        .get::<crate::ecs::floating_geometry::FloatingGeometry>(entity)
        .expect("the imported frame is retained state");
    assert_eq!(
        geometry.frame,
        bevy::math::IRect::new(240, 120, 640, 520),
        "the candidate's frame is what was imported"
    );
    // Applying an import closes the window its owner owns.
    assert!(
        !harness
            .world()
            .resource::<RestoreCandidates>()
            .import_window_open(),
        "an explicit import ends the startup window"
    );
}

/// A binding the candidate does not corroborate is refused, and nothing is
/// applied: the caller's claim alone never authorizes a binding.
#[test]
fn an_uncorroborated_floating_binding_is_refused() {
    use spool_shared_types::commands::{RestoreBindings, RestoreFloatingBinding};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    // The candidate claims a different application.
    harness
        .world()
        .insert_resource(floating_candidates(TEST_PROCESS_ID + 7, "somewhere-else"));
    harness.pump_frames(2);

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: vec![RestoreFloatingBinding {
                candidate_window: 0,
                target_window_id: 0,
            }],
            membership: Vec::new(),
            focus: Vec::new(),
        },
    );
    let error = result
        .expect("the system runs")
        .expect_err("a corroboration failure is a rejection");
    assert_eq!(error.admission_code(), "import_binding_rejected");

    let entity = crate::tests::find_window_entity(0, harness.world());
    assert!(
        harness
            .world()
            .get::<crate::ecs::floating_geometry::FloatingGeometry>(entity)
            .is_none(),
        "a refused import applies nothing"
    );
}

/// After the startup window closes, an import is refused rather than applied late.
#[test]
fn a_late_import_is_refused() {
    use spool_shared_types::commands::{RestoreBindings, RestoreFloatingBinding};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(floating_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);
    harness
        .world()
        .resource_mut::<RestoreCandidates>()
        .close_import_window();

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: vec![RestoreFloatingBinding {
                candidate_window: 0,
                target_window_id: 0,
            }],
            membership: Vec::new(),
            focus: Vec::new(),
        },
    );
    let error = result
        .expect("the system runs")
        .expect_err("a closed window refuses the import");
    assert_eq!(error.admission_code(), "import_window_expired");
}

/// Without a frozen baseline the import is refused: the owner must have taken
/// its baseline before current-session edits, not after.
#[test]
fn an_import_without_a_frozen_baseline_is_refused() {
    use spool_shared_types::commands::{RestoreBindings, RestoreFloatingBinding};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    // No owner ran: the resource exists with its candidates but no baseline.
    harness
        .world()
        .insert_resource(floating_candidates(TEST_PROCESS_ID, "test"));

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: vec![RestoreFloatingBinding {
                candidate_window: 0,
                target_window_id: 0,
            }],
            membership: Vec::new(),
            focus: Vec::new(),
        },
    );
    let error = result
        .expect("the system runs")
        .expect_err("an unfrozen baseline refuses the import");
    assert_eq!(error.admission_code(), "import_baseline_missing");
}

// --- Space membership candidates (issue 25's second domain) ------------------

/// A candidate document with one membership entry for the harness's first window.
fn membership_candidates(pid: i32, bundle_id: &str) -> RestoreCandidates {
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 7,
        floating: Vec::new(),
        membership: vec![crate::ecs::state::SavedMembership {
            window_id: 0,
            pid,
            bundle_id: bundle_id.into(),
            space_id: TEST_WORKSPACE_ID,
        }],
        focus: Vec::new(),
        spaces: Vec::new(),
    }
    .into()
}

/// The harness with two user Spaces, so a declared Space can move between them.
fn two_space_harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(2).with_display(
        crate::tests::TEST_DISPLAY_ID,
        bevy::math::IRect::new(
            0,
            0,
            crate::tests::TEST_DISPLAY_WIDTH,
            crate::tests::TEST_DISPLAY_HEIGHT,
        ),
        vec![TEST_WORKSPACE_ID, TEST_WORKSPACE_ID + 1],
    );
    harness.pump_frames(10);
    harness
}

/// A corroborated membership binding declares the Space it names, and the
/// declaration is state: no platform write happens here.
#[test]
fn a_trusted_membership_binding_declares_the_space() {
    use spool_shared_types::commands::{RestoreBindings, RestoreMembershipBinding};

    let mut harness = two_space_harness();
    harness
        .world()
        .insert_resource(membership_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);
    let entity = crate::tests::find_window_entity(0, harness.world());
    let before = harness
        .world()
        .get::<crate::ecs::native_space::DeclaredSpace>(entity)
        .expect("the reconciliation declares a Space")
        .target;

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: Vec::new(),
            membership: vec![RestoreMembershipBinding {
                candidate_membership: 0,
                target_window_id: 0,
                target_space: TEST_WORKSPACE_ID + 1,
            }],
            focus: Vec::new(),
        },
    );
    assert!(result.is_ok(), "{result:?}");

    let declared = harness
        .world()
        .get::<crate::ecs::native_space::DeclaredSpace>(entity)
        .expect("a declared Space");
    assert_eq!(declared.target, Some(TEST_WORKSPACE_ID + 1));
    assert_ne!(before, declared.target, "the import is what changed it");
    assert!(
        declared.repairs.is_empty(),
        "an import is authored state, not a repair: {:?}",
        declared.repairs
    );
}

/// A binding the candidate does not corroborate is refused, and the declaration
/// keeps the Space it had.
#[test]
fn an_uncorroborated_membership_binding_is_refused() {
    use spool_shared_types::commands::{RestoreBindings, RestoreMembershipBinding};

    let mut harness = two_space_harness();
    harness
        .world()
        .insert_resource(membership_candidates(TEST_PROCESS_ID + 5, "elsewhere"));
    harness.pump_frames(2);
    let entity = crate::tests::find_window_entity(0, harness.world());
    let before = harness
        .world()
        .get::<crate::ecs::native_space::DeclaredSpace>(entity)
        .unwrap()
        .target;

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: Vec::new(),
            membership: vec![RestoreMembershipBinding {
                candidate_membership: 0,
                target_window_id: 0,
                target_space: TEST_WORKSPACE_ID + 1,
            }],
            focus: Vec::new(),
        },
    );
    let error = result
        .expect("the system runs")
        .expect_err("an uncorroborated binding is refused");
    assert_eq!(error.admission_code(), "import_binding_rejected");
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::native_space::DeclaredSpace>(entity)
            .unwrap()
            .target,
        before
    );
}

/// A declared Space must exist and be a user Space: the invariant is checked at
/// import rather than repaired afterwards.
#[test]
fn a_membership_import_to_an_unknown_space_is_refused() {
    use spool_shared_types::commands::{RestoreBindings, RestoreMembershipBinding};

    let mut harness = two_space_harness();
    harness
        .world()
        .insert_resource(membership_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: Vec::new(),
            membership: vec![RestoreMembershipBinding {
                candidate_membership: 0,
                target_window_id: 0,
                target_space: TEST_WORKSPACE_ID + 100,
            }],
            focus: Vec::new(),
        },
    );
    let error = result
        .expect("the system runs")
        .expect_err("an unknown Space is refused");
    assert_eq!(error.admission_code(), "import_target_space_not_found");
}

/// One bad binding refuses the whole import: the membership group is not applied
/// when another group fails.
#[test]
fn one_bad_binding_refuses_the_whole_import() {
    use spool_shared_types::commands::{
        RestoreBindings, RestoreFloatingBinding, RestoreMembershipBinding,
    };

    let mut harness = two_space_harness();
    // The membership candidate is fine; the floating one does not exist.
    harness
        .world()
        .insert_resource(membership_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);
    let entity = crate::tests::find_window_entity(0, harness.world());
    let before = harness
        .world()
        .get::<crate::ecs::native_space::DeclaredSpace>(entity)
        .unwrap()
        .target;

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        RestoreBindings {
            columns: Vec::new(),
            floating: vec![RestoreFloatingBinding {
                candidate_window: 0,
                target_window_id: 0,
            }],
            membership: vec![RestoreMembershipBinding {
                candidate_membership: 0,
                target_window_id: 0,
                target_space: TEST_WORKSPACE_ID + 1,
            }],
            focus: Vec::new(),
        },
    );
    assert!(result.expect("the system runs").is_err());
    assert_eq!(
        harness
            .world()
            .get::<crate::ecs::native_space::DeclaredSpace>(entity)
            .unwrap()
            .target,
        before,
        "a refused import applies nothing, including its valid group"
    );
}

// --- Per-Space focus memory candidates (issue 25's third domain) -------------

/// A candidate document carrying exactly the focus memory given to it.
fn focus_candidates_with(focus: Vec<crate::ecs::state::SavedFocus>) -> RestoreCandidates {
    SpoolState {
        version: INTENT_STATE_VERSION,
        revision: 7,
        floating: Vec::new(),
        membership: Vec::new(),
        focus,
        spaces: Vec::new(),
    }
    .into()
}

fn saved_focus_hint(pid: i32, bundle_id: &str, window_id: i32) -> crate::ecs::state::SavedWindow {
    crate::ecs::state::SavedWindow {
        window_id,
        pid,
        bundle_id: bundle_id.into(),
    }
}

/// One Space remembering a preference and a different logical selection.
fn focus_candidates(pid: i32, bundle_id: &str) -> RestoreCandidates {
    focus_candidates_with(vec![crate::ecs::state::SavedFocus {
        space_id: TEST_WORKSPACE_ID,
        preference: Some(saved_focus_hint(pid, bundle_id, 0)),
        selection: Some(saved_focus_hint(pid, bundle_id, 1)),
    }])
}

/// One import document naming exactly the bindings given to it.
fn focus_bindings(
    bindings: Vec<spool_shared_types::commands::RestoreFocusBinding>,
) -> spool_shared_types::commands::RestoreBindings {
    spool_shared_types::commands::RestoreBindings {
        columns: Vec::new(),
        floating: Vec::new(),
        membership: Vec::new(),
        focus: bindings,
    }
}

fn focus_binding(
    candidate_focus: usize,
    role: spool_shared_types::commands::FocusRole,
    target_window_id: i32,
) -> spool_shared_types::commands::RestoreFocusBinding {
    focus_binding_in(TEST_WORKSPACE_ID, candidate_focus, role, target_window_id)
}

fn focus_binding_in(
    target_space: u64,
    candidate_focus: usize,
    role: spool_shared_types::commands::FocusRole,
    target_window_id: i32,
) -> spool_shared_types::commands::RestoreFocusBinding {
    spool_shared_types::commands::RestoreFocusBinding {
        candidate_focus,
        target_space,
        role,
        target_window_id,
    }
}

/// A corroborated binding restores both roles of the Space's focus memory as
/// authored state: it creates no activation and advances no observation.
#[test]
fn a_trusted_focus_binding_restores_the_per_space_memory() {
    use spool_shared_types::commands::FocusRole;

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(focus_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);

    let preference = crate::tests::find_window_entity(0, harness.world());
    let selection = crate::tests::find_window_entity(1, harness.world());
    let before = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .snapshot();

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![
            focus_binding(0, FocusRole::Preference, 0),
            focus_binding(0, FocusRole::Selection, 1),
        ]),
    );
    assert!(result.is_ok(), "{result:?}");

    let focus = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>();
    assert_eq!(focus.preference_entity(TEST_WORKSPACE_ID), Some(preference));
    assert_eq!(focus.navigation_entity(TEST_WORKSPACE_ID), Some(selection));
    assert_eq!(
        focus.snapshot(),
        before,
        "restoring memory is authored state, not an activation request or an observation"
    );
}

/// A binding the candidate's cached identity does not corroborate is refused,
/// and the Space keeps the memory it had.
#[test]
fn an_uncorroborated_focus_binding_is_refused() {
    use spool_shared_types::commands::FocusRole;

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(focus_candidates(TEST_PROCESS_ID + 5, "elsewhere"));
    harness.pump_frames(2);

    let target = crate::tests::find_window_entity(0, harness.world());
    let before = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .preference_entity(TEST_WORKSPACE_ID);
    assert_ne!(
        before,
        Some(target),
        "the import is what would have changed the preference"
    );

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![focus_binding(0, FocusRole::Preference, 0)]),
    );
    let error = result
        .expect("the system runs")
        .expect_err("an uncorroborated binding is refused");
    assert_eq!(error.admission_code(), "import_binding_rejected");
    assert_eq!(
        harness
            .world()
            .resource::<crate::ecs::focus::FocusCoordinator>()
            .preference_entity(TEST_WORKSPACE_ID),
        before,
        "a refused import writes nothing"
    );
}

/// A candidate that remembers no selection offers no mapping for one: the
/// caller cannot prove a hint that does not exist.
#[test]
fn a_focus_binding_for_a_role_the_candidate_does_not_hold_is_refused() {
    use spool_shared_types::commands::FocusRole;

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(focus_candidates_with(vec![crate::ecs::state::SavedFocus {
            space_id: TEST_WORKSPACE_ID,
            preference: Some(saved_focus_hint(TEST_PROCESS_ID, "test", 0)),
            selection: None,
        }]));
    harness.pump_frames(2);

    let target = crate::tests::find_window_entity(1, harness.world());
    let before = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .navigation_entity(TEST_WORKSPACE_ID);
    assert_ne!(
        before,
        Some(target),
        "the import is what would have changed the selection"
    );

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![focus_binding(0, FocusRole::Selection, 1)]),
    );
    let error = result
        .expect("the system runs")
        .expect_err("an absent hint is not a mapping");
    assert_eq!(error.admission_code(), "import_binding_rejected");
    assert_eq!(
        harness
            .world()
            .resource::<crate::ecs::focus::FocusCoordinator>()
            .navigation_entity(TEST_WORKSPACE_ID),
        before,
        "a refused import writes nothing"
    );
}

/// Focus memory belongs to a user Space that exists, checked at the import
/// rather than repaired afterwards.
#[test]
fn a_focus_import_to_an_unknown_space_is_refused() {
    use spool_shared_types::commands::{FocusRole, RestoreFocusBinding};

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(focus_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![RestoreFocusBinding {
            candidate_focus: 0,
            target_space: TEST_WORKSPACE_ID + 100,
            role: FocusRole::Preference,
            target_window_id: 0,
        }]),
    );
    let error = result
        .expect("the system runs")
        .expect_err("an unknown Space is refused");
    assert_eq!(error.admission_code(), "import_target_space_not_found");
}

/// One bad binding refuses the whole group: a validated sibling is not applied
/// just because it was listed first.
#[test]
fn one_bad_focus_binding_refuses_the_whole_group() {
    use spool_shared_types::commands::FocusRole;

    let mut harness = two_space_harness();
    // The first Space is corroborated; the second claims another application.
    harness.world().insert_resource(focus_candidates_with(vec![
        crate::ecs::state::SavedFocus {
            space_id: TEST_WORKSPACE_ID,
            preference: Some(saved_focus_hint(TEST_PROCESS_ID, "test", 0)),
            selection: None,
        },
        crate::ecs::state::SavedFocus {
            space_id: TEST_WORKSPACE_ID + 1,
            preference: Some(saved_focus_hint(TEST_PROCESS_ID + 5, "elsewhere", 1)),
            selection: None,
        },
    ]));
    harness.pump_frames(2);

    let target = crate::tests::find_window_entity(0, harness.world());
    let before = harness
        .world()
        .resource::<crate::ecs::focus::FocusCoordinator>()
        .preference_entity(TEST_WORKSPACE_ID);
    assert_ne!(before, Some(target), "the import is what would change it");

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![
            focus_binding_in(TEST_WORKSPACE_ID, 0, FocusRole::Preference, 0),
            focus_binding_in(TEST_WORKSPACE_ID + 1, 1, FocusRole::Preference, 1),
        ]),
    );
    let error = result
        .expect("the system runs")
        .expect_err("the uncorroborated binding refuses the group");
    assert_eq!(error.admission_code(), "import_binding_rejected");
    assert_eq!(
        harness
            .world()
            .resource::<crate::ecs::focus::FocusCoordinator>()
            .preference_entity(TEST_WORKSPACE_ID),
        before,
        "a refused import applies nothing, including its valid binding"
    );
}

/// One Space holds one preference and one selection, so a request cannot claim
/// the same role twice for it.
#[test]
fn a_duplicate_focus_binding_is_refused() {
    use spool_shared_types::commands::FocusRole;

    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
        .world()
        .insert_resource(focus_candidates(TEST_PROCESS_ID, "test"));
    harness.pump_frames(2);

    let result = harness.world().run_system_cached_with(
        crate::ecs::restore::restore_intents,
        focus_bindings(vec![
            focus_binding(0, FocusRole::Preference, 0),
            focus_binding(0, FocusRole::Preference, 0),
        ]),
    );
    let error = result
        .expect("the system runs")
        .expect_err("a duplicate role is refused");
    assert_eq!(error.admission_code(), "import_failed");
}

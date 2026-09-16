use bevy::prelude::*;

use super::*;
use crate::commands::{Action, Direction, MoveFocus, Operation};
use crate::ecs::floating_geometry::{FloatingGeometry, FloatingRepair};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::window_frame::DesiredWindowFrame;
use crate::ecs::{Floating, WindowVisibility};
use crate::events::Event;

/// A window that floats and holds no strip membership, so its frame comes from
/// the floating intent rather than from a layout.
fn floating_window(harness: &mut TestHarness, id: i32) -> Entity {
    let entity = find_window_entity(id, harness.world());
    let world = harness.world();
    for (_, mut strip) in world.query::<(Entity, &mut LayoutStrip)>().iter_mut(world) {
        strip.remove(entity);
    }
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(5);
    harness.mock_state.take_focus_requests();
    entity
}

fn harness() -> TestHarness {
    let mut harness = TestHarness::new().with_windows(2);
    harness.pump_frames(10);
    harness
}

fn intent(harness: &mut TestHarness, entity: Entity) -> FloatingGeometry {
    harness
        .world()
        .get::<FloatingGeometry>(entity)
        .expect("a floating frame intent")
        .clone()
}

fn dispatched(harness: &mut TestHarness, entity: Entity) -> Option<IRect> {
    harness
        .world()
        .get::<DesiredWindowFrame>(entity)
        .map(|frame| frame.0)
}

fn last_repair(repairs: &[FloatingRepair]) -> Option<&FloatingRepair> {
    repairs.last()
}

/// A floating edit states the intent, and the derived frame follows it: the
/// command no longer leaves the frame's meaning in the last thing it wrote.
#[test]
fn a_floating_edit_states_the_intent_and_the_frame_follows() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    let step = harness
        .world()
        .resource::<crate::config::Config>()
        .floating_window_move_step();
    let before = intent(&mut harness, entity).frame;

    harness
        .world()
        .write_message(Event::action_requested(Action::Window(Operation::Move(
            Direction::East,
        ))));
    harness.world().run_schedule(PreUpdate);
    harness.pump_frames(2);

    assert_eq!(
        intent(&mut harness, entity).frame.min.x,
        before.min.x + step
    );
    assert_eq!(
        dispatched(&mut harness, entity),
        Some(intent(&mut harness, entity).frame)
    );
}

/// A floating command states its intent and the derived frame equals it, so a
/// second command compounds on the first instead of on a stale base.
#[test]
fn successive_floating_moves_compound_on_the_intent() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    let step = harness
        .world()
        .resource::<crate::config::Config>()
        .floating_window_move_step();
    let before = intent(&mut harness, entity).frame;

    for _ in 0..3 {
        harness
            .world()
            .write_message(Event::action_requested(Action::Window(Operation::Move(
                Direction::East,
            ))));
        harness.world().run_schedule(PreUpdate);
        harness.pump_frames(2);
    }

    assert_eq!(
        intent(&mut harness, entity).frame.min.x,
        before.min.x + 3 * step
    );
}

/// A display that moves is still the same display: the frame is clamped onto it,
/// and the repair writes nothing to the platform.
#[test]
fn a_relocated_display_clamps_the_intent() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    let intents = harness.mock_state.native_space_intents().len();

    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 5000, TEST_DISPLAY_WIDTH, 5000 + TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID],
    );
    harness.pump_frames(10);

    let repaired = intent(&mut harness, entity);
    assert_eq!(
        last_repair(&repaired.repairs).map(|repair| repair.reason),
        Some("clamped_to_viewport")
    );
    assert!(
        repaired.frame.min.y >= 5000,
        "the frame lands on the display that exists now: {:?}",
        repaired.frame
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        intents,
        "a repair never writes to the platform"
    );
}

/// A display that is gone moves the window onto the display that exists, keeping
/// the offset it had on the display it was authored against.
#[test]
fn a_removed_display_relocates_the_intent_keeping_the_offset() {
    let mut harness = harness();
    // A second display to the right, and a frame authored on it.
    harness.mock_state.add_display(
        3,
        IRect::new(
            TEST_DISPLAY_WIDTH,
            0,
            TEST_DISPLAY_WIDTH * 2,
            TEST_DISPLAY_HEIGHT,
        ),
        vec![TEST_WORKSPACE_ID + 10],
    );
    harness.pump_frames(10);
    let entity = floating_window(&mut harness, 0);
    let former = IRect::new(
        TEST_DISPLAY_WIDTH,
        0,
        TEST_DISPLAY_WIDTH * 2,
        TEST_DISPLAY_HEIGHT,
    );
    harness
        .world()
        .entity_mut(entity)
        .insert(FloatingGeometry::authored(
            IRect::new(TEST_DISPLAY_WIDTH + 100, 60, TEST_DISPLAY_WIDTH + 500, 460),
            Some((3, former)),
        ));
    let intents = harness.mock_state.native_space_intents().len();
    harness.pump_frames(5);
    assert_eq!(
        intent(&mut harness, entity).frame.min.x,
        TEST_DISPLAY_WIDTH + 100,
        "the frame is realizable where it is"
    );

    harness.mock_state.remove_display(3);
    harness.pump_frames(10);

    let relocated = intent(&mut harness, entity);
    assert_eq!(
        last_repair(&relocated.repairs).map(|repair| repair.reason),
        Some("display_gone")
    );
    assert_eq!(
        relocated.frame.min.x, 100,
        "the offset from the former display's origin is kept: {:?}",
        relocated.frame
    );
    assert_eq!(
        harness.mock_state.native_space_intents().len(),
        intents,
        "a repair never writes to the platform"
    );
}

/// A display whose usable area shrinks clamps the intent into it, with the clamp
/// recorded as the repair the approved order asks for.
#[test]
fn a_shrunk_display_clamps_the_intent() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    // Put the frame near the right edge, then take most of the display away.
    harness
        .world()
        .entity_mut(entity)
        .insert(FloatingGeometry::authored(
            IRect::new(600, 40, 900, 400),
            Some((
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
            )),
        ));
    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, 400, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID],
    );
    harness.pump_frames(10);

    let clamped = intent(&mut harness, entity);
    assert_eq!(
        last_repair(&clamped.repairs).map(|repair| repair.reason),
        Some("clamped_to_viewport")
    );
    assert!(clamped.frame.max.x <= 400, "{:?}", clamped.frame);
    assert_eq!(clamped.frame.width(), 300, "the size is kept");
}

/// A fact that cannot be read is unknown, not absence: with no display geometry
/// to check against, the intent is kept and nothing is derived from it.
#[test]
fn an_unreadable_inventory_leaves_the_intent_unresolved() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    let before = intent(&mut harness, entity);

    harness.mock_state.set_display_inventory_available(false);
    harness.pump_frames(10);

    let after = intent(&mut harness, entity);
    assert!(after.unresolved);
    assert_eq!(after.frame, before.frame);
    assert_eq!(after.repairs, before.repairs);
}

/// The intent is window state, so tiling the window does not discard it: a later
/// float restores the frame the user left it at.
#[test]
fn the_intent_survives_a_tiled_spell() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    harness
        .world()
        .entity_mut(entity)
        .insert(FloatingGeometry::authored(
            IRect::new(240, 120, 640, 520),
            None,
        ));
    harness.pump_frames(5);
    assert_eq!(
        dispatched(&mut harness, entity),
        Some(IRect::new(240, 120, 640, 520))
    );

    // Tiled: the layout owns the frame and the floating intent is dormant.
    harness
        .world()
        .write_message(Event::action_requested(Action::Window(
            Operation::ToggleFloating,
        )));
    harness.world().run_schedule(PreUpdate);
    harness.pump_frames(10);

    harness
        .world()
        .write_message(Event::action_requested(Action::Window(
            Operation::ToggleFloating,
        )));
    harness.world().run_schedule(PreUpdate);
    harness.pump_frames(10);

    assert_eq!(
        intent(&mut harness, entity).frame,
        IRect::new(240, 120, 640, 520),
        "the retained frame is what the window returns to"
    );
}

/// A hidden floating window keeps its intent: visibility is not the frame's
/// authority, and nothing is repaired while it is away.
#[test]
fn a_minimized_floating_window_keeps_its_intent() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 0);
    let before = intent(&mut harness, entity);

    harness
        .world()
        .entity_mut(entity)
        .insert(WindowVisibility::Minimized);
    harness.pump_frames(10);

    let after = intent(&mut harness, entity);
    assert_eq!(after.frame, before.frame);
    assert_eq!(after.repairs, before.repairs);
}

/// A focused-window command on a floating window still reaches it, and the
/// intent is what a later query reports: the migration did not make the frame
/// unreachable.
#[test]
fn a_targeted_floating_command_states_the_intent() {
    let mut harness = harness();
    let entity = floating_window(&mut harness, 1);
    let before = intent(&mut harness, entity).frame;

    harness
        .world()
        .write_message(Event::action_requested(Action::TargetedWindow {
            window_id: 1,
            operation: Operation::Move(Direction::South),
        }));
    harness.world().run_schedule(PreUpdate);
    harness.pump_frames(2);

    let step = harness
        .world()
        .resource::<crate::config::Config>()
        .floating_window_move_step();
    assert_eq!(
        intent(&mut harness, entity).frame.min.y,
        before.min.y + step
    );
    let _ = MoveFocus::Stay;
}

/// A refused floating move drives nothing — the frame follows the observation on
/// the next pass — but the refusal is kept so `window inspect` can answer why the
/// window did not move (ADR 0011, the owner's 3+2 choice).
#[test]
fn a_refused_floating_move_is_kept_as_a_diagnostic() {
    use crate::ecs::floating_geometry::{FloatingGeometry, FloatingMoveRefused};

    let mut harness = TestHarness::new().with_windows(1);
    harness.pump_frames(15);
    let entity = find_window_entity(0, harness.world());
    harness.world().entity_mut(entity).insert(Floating);
    harness.pump_frames(10);
    let intent = harness
        .world()
        .get::<FloatingGeometry>(entity)
        .expect("a floating window keeps an intent")
        .frame;
    let observed_before = harness.mock_state.actual_window_frame(0);

    // The platform refuses frame writes as invalid for this exact request.
    let code = accessibility_sys::kAXErrorIllegalArgument;
    harness
        .mock_state
        .refuse_frame_writes_with_code(0, Some(code));
    let moved = IRect::new(
        intent.min.x + 120,
        intent.min.y + 80,
        intent.width(),
        intent.height(),
    );
    harness
        .world()
        .entity_mut(entity)
        .insert(FloatingGeometry::authored(moved, None));
    harness.pump_frames(20);

    let refused = harness
        .world()
        .get::<FloatingMoveRefused>(entity)
        .expect("the refusal is recorded for diagnosis");
    assert_eq!(refused.code, code);
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        observed_before,
        "a refused write leaves the window where it was, and the intent follows it"
    );
    let intent_after = harness
        .world()
        .get::<FloatingGeometry>(entity)
        .expect("a floating window keeps an intent")
        .frame;
    assert_ne!(
        intent_after, moved,
        "the refused target does not survive as intent"
    );
}

use bevy::prelude::*;

use crate::ecs::native_space::{NativeSpace, SpaceKind};
use crate::ecs::{ActiveDisplayMarker, SpawnWindowTrigger};
use crate::manager::{Display, Window, WindowManager};

use super::*;

fn allow_exit_cleanup(harness: &mut TestHarness) {
    let mut window_manager = harness.mock_state.create_window_manager();
    window_manager.expect_dim_windows().return_const(());
    harness
        .world()
        .insert_resource(WindowManager(Box::new(window_manager)));
}

fn refresh_window_frame(harness: &mut TestHarness, window_id: i32) {
    let entity = find_window_entity(window_id, harness.world());
    harness
        .world()
        .get_mut::<Window>(entity)
        .expect("window")
        .update_frame()
        .expect("refresh window frame");
}

fn set_current_frame(harness: &mut TestHarness, window_id: i32, frame: IRect) {
    harness
        .mock_state
        .os_set_window_frame_silently(window_id, frame);
    refresh_window_frame(harness, window_id);
}

fn exit_spool(harness: &mut TestHarness) {
    allow_exit_cleanup(harness);
    harness.world().write_message(AppExit::Success);
    harness.app.update();
}

fn set_native_space_kind(harness: &mut TestHarness, kind: SpaceKind) {
    let world = harness.world();
    let mut spaces = world.query::<&mut NativeSpace>();
    let mut space = spaces
        .iter_mut(world)
        .find(|space| space.id == TEST_WORKSPACE_ID)
        .expect("test native Space");
    space.kind = kind;
}

#[test]
fn exit_restores_the_exact_startup_window_frame() {
    let startup_frame = IRect::new(120, 90, 620, 490);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = startup_frame;
    });
    harness.pump_frames(10);

    assert_ne!(
        harness.mock_state.actual_window_frame(0),
        Some(startup_frame),
        "Spool must first arrange the startup window so the exit assertion can detect rollback"
    );
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(startup_frame)
    );
}

#[test]
fn exit_clamps_partly_offscreen_startup_frame_on_unchanged_display() {
    for x in [-400, -1200] {
        let mut harness = TestHarness::new().with_window(0, |window| {
            window.frame = IRect::new(x, 80, x + 500, 480);
        });
        harness.pump_frames(10);
        exit_spool(&mut harness);
        assert_eq!(
            harness.mock_state.actual_window_frame(0),
            Some(IRect::new(0, 80, 500, 480))
        );
    }
}

#[test]
fn exit_fits_oversized_startup_window_within_the_screen() {
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(
            -100,
            -100,
            TEST_DISPLAY_WIDTH + 100,
            TEST_DISPLAY_HEIGHT + 100,
        );
    });
    harness.pump_frames(10);
    exit_spool(&mut harness);
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(IRect::new(
            0,
            TEST_MENUBAR_HEIGHT,
            TEST_DISPLAY_WIDTH,
            TEST_DISPLAY_HEIGHT
        ))
    );
}

#[test]
fn exit_brings_new_offscreen_windows_back_onto_the_screen() {
    let mut harness = TestHarness::new();
    harness.pump_frames(10);
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(40, 60, 440, 360),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(5);
    set_current_frame(&mut harness, 0, IRect::new(-800, 140, -300, 540));
    exit_spool(&mut harness);
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(IRect::new(0, 140, 500, 540))
    );
}

#[test]
fn exit_does_not_restore_a_window_that_ax_reported_fullscreen_at_startup() {
    let current_frame = IRect::new(200, 160, 700, 560);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT);
        window.is_full_screen = true;
    });
    harness.pump_frames(10);

    harness.mock_state.update_window(0, |window| {
        window.frame = current_frame;
        window.is_full_screen = false;
    });
    refresh_window_frame(&mut harness, 0);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn exit_does_not_restore_a_window_that_ax_reports_currently_fullscreen() {
    let current_frame = IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(120, 90, 620, 490);
    });
    harness.pump_frames(10);

    harness.mock_state.update_window(0, |window| {
        window.frame = current_frame;
        window.is_full_screen = true;
    });
    refresh_window_frame(&mut harness, 0);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn exit_leaves_windows_opened_after_startup_unchanged() {
    let current_frame = IRect::new(180, 140, 680, 540);
    let mut harness = TestHarness::new();
    harness.pump_frames(10);

    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(40, 60, 440, 360),
    );
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(5);
    set_current_frame(&mut harness, 0, current_frame);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn windowed_fullscreen_windowed_restores_the_startup_frame() {
    let startup_frame = IRect::new(100, 80, 600, 480);
    let current_frame = IRect::new(220, 170, 720, 570);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = startup_frame;
    });
    harness.pump_frames(10);

    harness.mock_state.update_window(0, |window| {
        window.is_full_screen = true;
    });
    harness.mock_state.update_window(0, |window| {
        window.is_full_screen = false;
        window.frame = current_frame;
    });
    refresh_window_frame(&mut harness, 0);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(startup_frame)
    );
}

#[test]
fn native_fullscreen_space_excludes_a_startup_window_when_ax_reports_windowed() {
    let current_frame = IRect::new(220, 170, 720, 570);
    let mut harness = TestHarness::new();
    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, true);
    harness = harness.with_window(0, |window| {
        window.frame = IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT);
        window.is_full_screen = false;
    });
    harness.pump_frames(10);

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
    set_native_space_kind(&mut harness, SpaceKind::User);
    set_current_frame(&mut harness, 0, current_frame);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn native_fullscreen_space_at_exit_is_left_untouched_when_ax_reports_windowed() {
    let current_frame = IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(100, 80, 600, 480);
    });
    harness.pump_frames(10);

    harness
        .mock_state
        .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, true);
    set_native_space_kind(&mut harness, SpaceKind::Fullscreen);
    set_current_frame(&mut harness, 0, current_frame);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn uncertain_fullscreen_state_at_startup_never_creates_a_restore_snapshot() {
    let current_frame = IRect::new(220, 170, 720, 570);
    let mut harness = TestHarness::new();
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        0,
        IRect::new(100, 80, 600, 480),
    );
    harness.mock_state.fail_full_screen_queries(0, 1);
    harness
        .world()
        .trigger(SpawnWindowTrigger::new(vec![window]));
    harness.pump_frames(10);

    set_current_frame(&mut harness, 0, current_frame);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn uncertain_fullscreen_state_at_exit_skips_restoration() {
    let current_frame = IRect::new(220, 170, 720, 570);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(100, 80, 600, 480);
    });
    harness.pump_frames(10);

    set_current_frame(&mut harness, 0, current_frame);
    harness.mock_state.fail_full_screen_queries(0, 1);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(current_frame)
    );
}

#[test]
fn reused_window_id_does_not_inherit_the_old_launch_snapshot() {
    let replacement_frame = IRect::new(220, 170, 720, 570);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = IRect::new(100, 80, 600, 480);
    });
    harness.pump_frames(15);

    drop(
        harness
            .mock_state
            .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 0, replacement_frame),
    );
    harness.pump_frames(20);
    set_current_frame(&mut harness, 0, replacement_frame);
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(replacement_frame)
    );
}

#[test]
fn changed_display_geometry_clamps_the_startup_frame_without_resizing_it() {
    let startup_frame = IRect::new(800, 500, 1100, 800);
    let expected_frame = IRect::new(500, 300, 800, 600);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = startup_frame;
    });
    harness.pump_frames(10);
    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, 800, 600),
        vec![TEST_WORKSPACE_ID],
    );

    let display_entity = {
        let world = harness.world();
        world
            .query::<(Entity, &Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == TEST_DISPLAY_ID).then_some(entity))
            .expect("test display")
    };
    harness.world().entity_mut(display_entity).insert((
        Display::new(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 800, 600),
            TEST_MENUBAR_HEIGHT,
        ),
        ActiveDisplayMarker,
    ));
    set_current_frame(&mut harness, 0, IRect::new(100, 100, 400, 400));
    exit_spool(&mut harness);

    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(expected_frame)
    );
}

#[test]
fn missing_launch_display_leaves_the_window_on_its_current_display() {
    for (current_frame, expected_frame) in [
        (
            IRect::new(1200, 140, 1700, 540),
            IRect::new(1200, 140, 1700, 540),
        ),
        (
            IRect::new(-800, 140, -300, 540),
            IRect::new(TEST_DISPLAY_WIDTH, 140, TEST_DISPLAY_WIDTH + 500, 540),
        ),
    ] {
        let mut harness = TestHarness::new().with_window(0, |window| {
            window.frame = IRect::new(100, 80, 600, 480);
        });
        harness.pump_frames(10);
        harness.mock_state.remove_display(TEST_DISPLAY_ID);
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

        let display_entity = {
            let world = harness.world();
            world
                .query::<(Entity, &Display)>()
                .iter(world)
                .find_map(|(entity, display)| (display.id() == TEST_DISPLAY_ID).then_some(entity))
                .expect("launch display")
        };
        harness
            .world()
            .entity_mut(display_entity)
            .remove::<(Display, ActiveDisplayMarker)>();
        harness.world().spawn((
            Display::new(
                EXT_DISPLAY_ID,
                IRect::new(
                    TEST_DISPLAY_WIDTH,
                    0,
                    TEST_DISPLAY_WIDTH + EXT_DISPLAY_WIDTH,
                    EXT_DISPLAY_HEIGHT,
                ),
                TEST_MENUBAR_HEIGHT,
            ),
            ActiveDisplayMarker,
        ));
        set_current_frame(&mut harness, 0, current_frame);
        exit_spool(&mut harness);

        assert_eq!(
            harness.mock_state.actual_window_frame(0),
            Some(expected_frame)
        );
    }
}

#[test]
fn restoring_geometry_does_not_change_minimized_state() {
    let startup_frame = IRect::new(100, 80, 600, 480);
    let mut harness = TestHarness::new().with_window(0, |window| {
        window.frame = startup_frame;
    });
    harness.pump_frames(10);

    harness.mock_state.update_window(0, |window| {
        window.minimized = true;
    });
    exit_spool(&mut harness);

    let entity = find_window_entity(0, harness.world());
    assert!(
        harness
            .world()
            .get::<Window>(entity)
            .expect("window")
            .is_minimized()
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(0),
        Some(startup_frame)
    );
}

#[test]
fn one_constrained_window_does_not_block_other_launch_frame_restores() {
    let first_startup = IRect::new(80, 80, 480, 480);
    let second_startup = IRect::new(520, 100, 920, 500);
    let mut harness = TestHarness::new()
        .with_window(0, |window| window.frame = first_startup)
        .with_window(1, |window| window.frame = second_startup);
    harness.pump_frames(10);

    harness.mock_state.constrain_frame_writes(0, true);
    exit_spool(&mut harness);

    assert_ne!(
        harness.mock_state.actual_window_frame(0),
        Some(first_startup),
        "the constrained app should keep the frame it accepted"
    );
    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(second_startup),
        "another window must still restore after a constrained write"
    );
}

#[test]
fn rejected_offscreen_correction_is_bounded_and_does_not_block_other_windows() {
    let second_startup = IRect::new(520, 100, 920, 500);
    let mut harness = TestHarness::new()
        .with_window(0, |window| window.frame = IRect::new(80, 80, 480, 480))
        .with_window(1, |window| window.frame = second_startup);
    harness.pump_frames(10);
    set_current_frame(&mut harness, 0, IRect::new(-800, 80, -400, 480));
    harness.mock_state.constrain_frame_writes(0, true);
    let attempts = harness.mock_state.frame_write_attempts(0);
    exit_spool(&mut harness);
    assert_eq!(harness.mock_state.frame_write_attempts(0) - attempts, 2);
    assert_eq!(
        harness.mock_state.actual_window_frame(1),
        Some(second_startup)
    );
}

use bevy::app::AppExit;
use bevy::ecs::change_detection::{DetectChanges, Ref};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::{ChildOf, Children};
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::ecs::query::{Added, Changed, Has, Or, With, Without};
use bevy::ecs::system::{
    Commands, Local, NonSend, NonSendMut, Populated, Query, Res, ResMut, Single, SystemParam,
};
use bevy::math::IRect;
use bevy::tasks::AsyncComputeTaskPool;
use bevy::tasks::futures_lite::future;
use bevy::time::Time;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};
use tracing::{Level, debug, error, info, instrument, trace, warn};

use super::{
    ActiveDisplayMarker, BProcess, ExistingMarker, FreshMarker, RepositionMarker, ResizeMarker,
    RetryFrontSwitch, SpawnWindowTrigger, Timeout, VerifyWindowPosition,
};

use crate::config::{Config, decorations::BorderRadiusOption};
use crate::ecs::display::FloatingLayer;
use crate::ecs::focus::{FocusCoordinator, FocusSignal};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::params::{FrameActivity, Windows};
use crate::ecs::reconcile::WindowUnavailable;
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, BruteforceWindows, DesiredWindowFrame, DockPosition,
    FlashMessage, Floating, Initializing, LowPowerMode, MissionControlActive, ObservedWindowFrame,
    Position, PresentedWindowFrame, ReadDisplayProperties, RestoreWindowState, Scrolling,
    SendMessageTrigger, SpawnCommandsExt, WidthRatio, WindowProperties, WindowVisibility,
};
use crate::events::{Event, FocusSource, InputEvent};
use crate::manager::{
    Application, Display, Process, Window, WindowManager, WindowOS, bruteforce_windows,
};
use crate::overlay::{FlashMessageManager, OverlayManager};
use crate::platform::{PlatformCallbacks, WinID, WindowIncarnation};

/// Keeps `WindowServer`'s per-window close notification subscription aligned
/// with the ECS inventory on macOS versions that require explicit requests.
pub(super) fn refresh_window_notifications(
    windows: Query<&Window>,
    added: Query<(), Added<Window>>,
    mut removed: RemovedComponents<Window>,
    window_manager: Res<WindowManager>,
    time: Res<Time>,
    mut since_retry: Local<Duration>,
) {
    let inventory_changed = !added.is_empty() || removed.read().next().is_some();
    *since_retry = since_retry.saturating_add(time.delta());
    if !inventory_changed && *since_retry < Duration::from_secs(5) {
        return;
    }
    *since_retry = Duration::ZERO;

    let window_ids = windows.iter().map(|window| window.id()).collect::<Vec<_>>();
    _ = window_manager
        .request_window_notifications(&window_ids)
        .inspect_err(|error| warn!(%error, "unable to refresh WindowServer notifications"));
}

/// Processes and applications still inside their spawn grace period, with the
/// `FreshMarker` that says whether the spawn actually completed in time.
type TimedOutSpawns<'w, 's> = Populated<
    'w,
    's,
    (Entity, Has<FreshMarker>, &'static Timeout),
    Or<(With<BProcess>, With<Application>)>,
>;

type PendingWindowFrames<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Window,
        Ref<'static, PresentedWindowFrame>,
        &'static DesiredWindowFrame,
        &'static mut WidthRatio,
        Option<&'static mut ObservedWindowFrame>,
        Has<Floating>,
    ),
    (
        Changed<PresentedWindowFrame>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type PositionVerificationWindows<'w, 's> = Populated<
    'w,
    's,
    (
        Entity,
        &'static mut Window,
        &'static DesiredWindowFrame,
        &'static mut VerifyWindowPosition,
        Option<&'static mut ObservedWindowFrame>,
    ),
    Without<WindowSpaceReassignmentPending>,
>;

type AnimatedPositions<'w, 's> = Populated<
    'w,
    's,
    (&'static mut Position, Entity, &'static RepositionMarker),
    (Without<WindowSpaceReassignmentPending>, Without<Window>),
>;

type AnimatedSizes<'w, 's> = Populated<
    'w,
    's,
    (&'static mut Bounds, Entity, &'static ResizeMarker),
    (Without<WindowSpaceReassignmentPending>, Without<Window>),
>;

const ANIAMTE_SNAP_THRESHOLD: f32 = 5.0;
const LOOP_MAX_TIMEOUT_LOWPOWER_MS: u32 = 500;
const LOOP_MAX_TIMEOUT_MS: u32 = 50;
const LOOP_TIMEOUT_STEP: u32 = 1;

fn next_pump_timeout(current: u32, drained: bool, frame_active: bool, low_power: bool) -> u32 {
    // While a presentation is in flight, return to the ECS/AX pipeline almost
    // immediately. Sleeping for a display frame here leaves no budget for the
    // synchronous accessibility writes that actually present the windows.
    if !drained || frame_active {
        return LOOP_TIMEOUT_STEP;
    }

    let timeout_limit = if low_power {
        LOOP_MAX_TIMEOUT_LOWPOWER_MS
    } else {
        LOOP_MAX_TIMEOUT_MS
    };
    current.min(timeout_limit) + LOOP_TIMEOUT_STEP
}

/// How long [`pump_events`] may spend draining the incoming channel before it
/// has to hand the frame back, and how many events it may take in one go.
///
/// The drain previously ran until a full millisecond passed with nothing
/// arriving, a condition a sustained burst (drag, resize animation, a churning
/// app) never satisfies — the loop never exited and the window manager stopped
/// responding until the burst let up. Nothing is dropped when a cap is hit:
/// leftover events stay in the channel for the next frame to pick up.
const PUMP_BUDGET: Duration = Duration::from_millis(4);
const PUMP_MAX_EVENTS: usize = 256;

/// Gathers all present displays and spawns them as entities in the Bevy world.
/// The currently active display (identified by `window_manager.active_display_id()`) is marked with `ActiveDisplayMarker`.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for querying display information.
/// * `commands` - Bevy commands to spawn entities.
pub fn gather_displays(window_manager: Res<WindowManager>, mut commands: Commands) {
    let Ok(active_display_id) = window_manager.active_display_id() else {
        error!("Unable to get active display id!");
        return;
    };
    for (display, workspaces) in window_manager.present_displays() {
        let display_id = display.id();
        let origin = Position(display.bounds().min);
        let entity = if display_id == active_display_id {
            commands.spawn((display, ActiveDisplayMarker))
        } else {
            commands.spawn(display)
        }
        .id();

        commands.trigger(ReadDisplayProperties(entity));

        let Ok(visible_space) = window_manager.active_display_space(display_id) else {
            error!(display_id, "Unable to get visible Space id");
            continue;
        };

        for (ordinal, id) in workspaces.into_iter().enumerate() {
            let visible = id == visible_space;
            let active = display_id == active_display_id && visible;
            let mut strip =
                commands.spawn_layout_strip(LayoutStrip::new(id), origin.0, entity, active);
            strip.insert(NativeSpace::new(
                id,
                ordinal,
                window_manager.workspace_is_fullscreen(id),
            ));
            if visible {
                strip.insert(VisibleNativeSpaceMarker);
            }
            commands.spawn((FloatingLayer::new(id), ChildOf(entity)));
        }
    }
}

/// Adds an existing process to the window manager. This is used during initial setup for already running applications.
/// It attempts to create a new `Application` instance from the `BProcess` and attaches it as a child entity.
/// The `ExistingMarker` is then removed from the process entity.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for creating new application instances.
/// * `process_query` - A query for existing `BProcess` entities marked with `ExistingMarker`.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn add_existing_process(
    window_manager: Res<WindowManager>,
    processes: Populated<(Entity, &BProcess), With<ExistingMarker>>,
    mut commands: Commands,
) {
    for (entity, process) in processes {
        let Ok(app) = window_manager.new_application(&*process.0) else {
            error!("creating aplication from process '{}'", process.name());
            return;
        };
        commands.spawn((app, ExistingMarker, ChildOf(entity)));
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<ExistingMarker>();
        }
    }
}

/// Adds an existing application to the window manager. This is used during initial setup.
/// It observes the application, adds its windows to the manager, and then triggers `SpawnWindowTrigger` events for newly found windows.
/// The `ExistingMarker` is removed from the application entity after processing.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for interacting with window management logic.
/// * `displays` - A query for all `Display` entities, used to gather all existing space IDs.
/// * `app_query` - A query for existing `Application` entities marked with `ExistingMarker`.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn add_existing_application(
    window_manager: Res<WindowManager>,
    workspaces: Query<&LayoutStrip>,
    fresh_apps: Populated<(&mut Application, Entity), With<ExistingMarker>>,
    config: Res<Config>,
    mut commands: Commands,
) {
    let spaces = workspaces
        .into_iter()
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    let thread_pool = AsyncComputeTaskPool::get();

    for (mut app, entity) in fresh_apps {
        let mut offscreen_windows = vec![];

        match app.observe() {
            Ok(true) => {}
            Ok(false) => debug!(pid = app.pid(), "some application observers need retry"),
            Err(error) => {
                warn!(pid = app.pid(), %error, "unable to register application observers");
            }
        }
        if let Ok((found_windows, offscreen)) = window_manager
            .find_existing_application_windows(&mut app, &spaces, &config)
            .inspect_err(|err| warn!("{err}"))
        {
            offscreen_windows.extend(offscreen);
            commands.trigger(SpawnWindowTrigger::for_application(entity, found_windows));
        }
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<ExistingMarker>();
        }

        if !offscreen_windows.is_empty() {
            let pid = app.pid();
            let bundle_id = app.bundle_id();
            let config = config.clone();
            let bruteforce_task = thread_pool.spawn(async move {
                bruteforce_windows(pid, bundle_id.as_deref(), offscreen_windows, &config)
            });
            commands.spawn(BruteforceWindows(bruteforce_task));
        }
    }
}

/// Finishes initialization once all initial windows are loaded. The active
/// application's AX focus is observed; startup never chooses a window itself.
///
/// # Arguments
///
/// * `windows` - A mutable query for all tracked window components.
/// * `displays` - A query for all `Display` entities, including whether they have the `ActiveDisplayMarker`.
/// * `window_manager` - The `WindowManager` resource for refreshing displays and getting active space information.
/// * `commands` - Bevy commands used to publish the observed AX focus.
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn finish_setup(
    process_query: Query<Entity, With<ExistingMarker>>,
    windows: Windows,
    applications: Query<&Application>,
    mut bruteforce_tasks: Query<(Entity, &mut BruteforceWindows)>,
    mut workspaces: Query<&mut LayoutStrip>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !process_query.is_empty() {
        // The other two add_* functions are still running..
        return;
    }

    // Reap the bruteforced windows.
    if !bruteforce_tasks.is_empty() {
        for (entity, mut job) in &mut bruteforce_tasks {
            if let Some(found_windows) = future::block_on(future::poll_once(&mut job.0)) {
                commands.trigger(SpawnWindowTrigger::new(found_windows));
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_despawn();
                }
            }
        }
        // Wait for the next tick to finish initialization.
        return;
    }

    info!(
        "Initialization: found {:?} windows.",
        windows.iter().size_hint()
    );

    for mut strip in &mut workspaces {
        debug!("space {}: before refresh {strip:?}", strip.id());
        let workspace_windows = window_manager
            .windows_in_workspace(strip.id())
            .inspect_err(|err| {
                warn!("failed to get windows on workspace {}: {err}", strip.id());
            })
            .ok()
            .map(|workspace_windows| {
                workspace_windows
                    .into_iter()
                    .filter_map(|window_id| windows.find_tiled(window_id))
                    .filter(|(window, entity)| {
                        if window.is_minimized() {
                            if let Ok(mut entity_commands) = commands.get_entity(*entity) {
                                entity_commands.try_insert(WindowVisibility::Minimized);
                            }
                            false
                        } else {
                            true
                        }
                    })
                    .collect::<Vec<_>>()
            });
        let Some(workspace_windows) = workspace_windows else {
            continue;
        };

        // Preserve the order - do not flush existing windows.
        for entity in strip.all_windows() {
            if !workspace_windows.iter().any(|(_, e)| *e == entity) {
                strip.remove(entity);
            }
        }
        for (_, entity) in workspace_windows {
            if !strip.contains(entity) {
                strip.append(entity);
            }
        }
        debug!("space {}: after refresh {strip:?}", strip.id());
    }

    if let Some(focused_window_id) = applications
        .iter()
        .find(|app| app.is_frontmost())
        .and_then(|app| app.focused_window_id().ok())
    {
        commands.trigger(SendMessageTrigger(Event::window_focused(focused_window_id)));
    }

    commands.remove_resource::<Initializing>();
    commands.trigger(RestoreWindowState);
}

/// Handles the event when a new application is launched. It creates a `Process` and `Application` object,
/// observes the application for events, and adds its windows to the manager.
/// This system processes `BProcess` entities marked with `FreshMarker`.
/// If the process is not yet ready, it continues observing it. If ready, it attempts to create and observe an `Application`.
/// A `Timeout` is added to the application if it takes too long to become observable.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for creating new application instances.
/// * `process_query` - A `Populated` query for `(Entity, &mut BProcess, Has<Children>)` with `With<FreshMarker>`.
/// * `commands` - Bevy commands to spawn entities and manage components.
pub(super) fn add_launched_process(
    window_manager: Res<WindowManager>,
    fresh_processes: Populated<(Entity, &mut BProcess, Has<Children>), With<FreshMarker>>,
    config: Res<Config>,
    mut commands: Commands,
) {
    const APP_OBSERVABLE_TIMEOUT_SEC: u64 = 5;
    let mut already_seen = HashSet::new();

    for (entity, mut process, children) in fresh_processes {
        let process = &mut *process.0;

        if !already_seen.insert(process.psn()) {
            continue;
        }

        if config.should_force_track_process(process) {
            debug!(
                "Forcing tracking of launched process '{}' despite unobservable policy.",
                process.name()
            );
            process.force_track(true);
        }

        if !process.ready() {
            continue;
        }

        if children {
            // Process already has an attached Application, so finish.
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<FreshMarker>();
            }
            continue;
        }

        let Ok(mut app) = window_manager.new_application(process) else {
            error!("creating aplication from process '{}'", process.name());
            return;
        };

        if app.observe().is_ok_and(|good| good) {
            let timeout = Timeout::new(
                Duration::from_secs(APP_OBSERVABLE_TIMEOUT_SEC),
                Some(format!(
                    "{app} did not become observable in {APP_OBSERVABLE_TIMEOUT_SEC}s.",
                )),
                &mut commands,
            );
            commands.spawn((app, FreshMarker, timeout, ChildOf(entity)));
        } else {
            debug!("failed to register some observers {}", process.name());
        }
    }
}

/// Adds windows for a newly launched application.
/// This system processes `Application` entities marked with `FreshMarker`.
/// It queries the application's window list, filters out already existing windows, and triggers `SpawnWindowTrigger` events for new windows.
/// The `FreshMarker` is removed from the application entity after processing.
///
/// # Arguments
///
/// * `app_query` - A `Populated` query for `(&mut Application, Entity)` with `With<FreshMarker>`.
/// * `windows` - A query for all `Window` components, used to check for existing windows.
/// * `commands` - Bevy commands to spawn entities and manage components.
pub(super) fn add_launched_application(
    app_query: Populated<(&mut Application, Entity, Has<Children>), With<FreshMarker>>,
    windows: Windows,
    config: Res<Config>,
    mut commands: Commands,
) {
    // TODO: maybe refactor this with add_existing_application_windows()
    let find_window = |window_id| windows.find(window_id);

    for (app, entity, has_children) in app_query {
        let mut create_windows = app.window_list(&config);
        // Retain the non-existing windows, so they can be created.
        create_windows.retain(|window| find_window(window.id()).is_none());

        if !create_windows.is_empty() {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<FreshMarker>();
            }
            debug!(
                "spawn! (polling path found {} new windows for {entity})",
                create_windows.len(),
            );
            commands.trigger(SpawnWindowTrigger::for_application(entity, create_windows));
        } else if has_children {
            // Windows were already created via AXCreated notification path.
            // Remove FreshMarker so the Timeout gets cleaned up.
            debug!("removing FreshMarker from {entity}: windows already created via AXCreated");
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<FreshMarker>();
            }
        }
    }
}

/// Cleans up entities which have been initializing for too long, specifically `BProcess` or `Application` entities.
/// This system removes the `Timeout` component from entities that are no longer `Fresh`.
///
/// This can be processes which are not yet observable or applications which keep failing to
/// register some of the observers.
///
/// # Arguments
///
/// * `cleanup` - A `Populated` query for `(Entity, Has<FreshMarker>, &Timeout)` components, targeting `BProcess` or `Application` entities.
/// * `commands` - Bevy commands to remove components.
pub(super) fn fresh_marker_cleanup(cleanup: TimedOutSpawns, mut commands: Commands) {
    for (entity, fresh, _) in cleanup {
        if !fresh && let Ok(mut entity_commands) = commands.get_entity(entity) {
            // Process was ready before the timer finished.
            entity_commands.try_remove::<Timeout>();
        }
    }
}

/// A Bevy system that ticks `Timeout` timers and despawns entities when their timers finish.
/// This system is responsible for cleaning up entities that have exceeded their allotted time for an operation.
///
/// # Arguments
///
/// * `timers` - A `Populated` query for `(Entity, &mut Timeout)` components.
/// * `clock` - The Bevy `Time` resource for getting the delta time.
/// * `commands` - Bevy commands to despawn entities.
pub(super) fn timeout_ticker(
    timers: Populated<(Entity, &mut Timeout)>,
    clock: Res<Time>,
    mut commands: Commands,
) {
    for (entity, mut timeout) in timers {
        if timeout.timer.is_finished() {
            trace!("Despawning entity {entity} due to timeout.");
            if let Some(system_id) = timeout.system_id.take() {
                commands.run_system(system_id);
                commands.unregister_system(system_id);
            }
            trace!("Removing timer {entity}");
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
        } else {
            timeout.timer.tick(clock.delta());
        }
    }
}

/// Retries querying the focused window for applications that had a transient AX error
/// during `ApplicationFrontSwitched`. Runs each frame until success or timeout.
pub(super) fn retry_front_switch(
    retries: Populated<(Entity, &mut RetryFrontSwitch)>,
    applications: Query<&Application>,
    clock: Res<Time>,
    mut focus: ResMut<FocusCoordinator>,
    mut commands: Commands,
) {
    for (entity, mut retry) in retries {
        if !focus.is_current(retry.generation) {
            debug!(
                "Discarding stale focus retry from generation {}.",
                retry.generation
            );
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        }
        let Ok(app) = applications.get(retry.app_entity) else {
            // Application entity no longer exists, clean up.
            focus.observe(FocusSignal::Unresolved {
                generation: retry.generation,
            });
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        };
        if !app.is_frontmost() {
            // App is no longer frontmost — this retry is stale.
            debug!("Discarding stale front switch retry (app no longer frontmost).");
            focus.observe(FocusSignal::Unresolved {
                generation: retry.generation,
            });
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        }
        retry.timer.tick(clock.delta());
        retry.probe.tick(clock.delta());
        if retry.probe.just_finished()
            && let Ok(focused_id) = app.focused_window_id()
        {
            debug!("Front switch retry succeeded for window {focused_id}.");
            commands.trigger(SendMessageTrigger(Event::resolved_focus(
                focused_id,
                app.pid(),
                FocusSource::Retry,
                retry.generation,
            )));
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        }
        if retry.timer.is_finished() {
            warn!(
                "Focused-window query for '{}' timed out; actual focus remains unknown.",
                app.name()
            );
            focus.observe(FocusSignal::Unresolved {
                generation: retry.generation,
            });
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
        }
    }
}

/// Animates window movement.
/// Fraction of the remaining distance an exponential ease-out consumes in a
/// frame, given a decay `rate` (per second) and the frame's `delta` in seconds.
/// Shared by the reposition and resize animators so the two never drift out of step.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to [0, 1], well within f32's range; only sub-pixel precision is lost"
)]
fn ease_out_factor(rate: f64, delta: f64) -> f32 {
    (1.0 - (-rate * delta).exp()).clamp(0.0, 1.0) as f32
}

/// This is a Bevy system that runs on `Update`. It smoothly moves windows to their target
/// positions, as indicated by the `RepositionMarker` component.
/// Animation speed is controlled by the `animation_speed` in the `Config`.
/// When a window reaches its target position, the `RepositionMarker` is removed.
///
/// # Arguments
///
/// * `windows` - A `Populated` query for `(&mut Window, Entity, &RepositionMarker)` components.
/// * `displays` - A query for all `Display` entities, used to get display bounds and menubar height.
/// * `time` - The Bevy `Time` resource for calculating delta time.
/// * `config` - The `Config` resource, used for animation speed.
/// * `commands` - Bevy commands to remove the `RepositionMarker` when animation is complete.
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn animate_entities(
    animate: AnimatedPositions,
    time: Res<Time>,
    config: Res<Config>,
    mut commands: Commands,
) {
    // Frame-rate-independent exponential smoothing (ease-out).
    // `animation_speed` is the decay rate (per second); higher = snappier.
    let t = ease_out_factor(config.animation_speed(), time.delta_secs_f64());

    animate
        .into_iter()
        .for_each(|(mut position, entity, RepositionMarker(origin))| {
            let target = origin.as_vec2();
            let current = position.0.as_vec2();
            let lerped = current.lerp(target, t);

            // Snap once we're within a pixel of the target (or after one effectively-
            // complete tick), so the marker is dropped promptly.
            let finished = (target - lerped).length() <= ANIAMTE_SNAP_THRESHOLD;
            let new_pos = if finished {
                *origin
            } else {
                lerped.round().as_ivec2()
            };

            trace!(
                "entity {entity} source {} dest {origin} t {t:.3} moving to {new_pos}",
                position.0,
            );
            position.0 = new_pos;
            if finished && let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<RepositionMarker>();
            }
        });
}

/// Animates window resizing.
/// This is a Bevy system that runs on `Update`. It resizes windows to their target
/// dimensions, as indicated by the `ResizeMarker` component.
/// When a window reaches its target size, the `ResizeMarker` is removed.
///
/// # Arguments
///
/// * `windows` - A `Populated` query for `(&mut Window, Entity, &ResizeMarker)` components.
/// * `commands` - Bevy commands to remove the `ResizeMarker` when resizing is complete.
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn animate_resize_entities(
    animate: AnimatedSizes,
    time: Res<Time>,
    config: Res<Config>,
    mut commands: Commands,
) {
    // Matches animate_entities: exponential ease-out, frame-rate independent.
    let t = ease_out_factor(config.animation_speed(), time.delta_secs_f64());

    animate
        .into_iter()
        .for_each(|(mut bounds, entity, ResizeMarker(size))| {
            let target = size.as_vec2();
            let current = bounds.0.as_vec2();
            let lerped = current.lerp(target, t);

            let finished = (target - lerped).length() <= ANIAMTE_SNAP_THRESHOLD;
            let new_size = if finished {
                *size
            } else {
                lerped.round().as_ivec2()
            };

            trace!(
                "entity {entity} source {} dest {size} t {t:.3} resizing to {new_size}",
                bounds.0,
            );
            bounds.0 = new_size;
            if finished && let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<ResizeMarker>();
            }
        });
}

/// Republishes the pointer and gesture events onto [`InputEvent`], so the input
/// systems do not have to sift the whole stream for them every frame.
///
/// Runs right after [`pump_events`], which is what puts this frame's events on
/// the stream in the first place.
pub(crate) fn demux_input_events(
    mut messages: MessageReader<Event>,
    mut input: MessageWriter<InputEvent>,
) {
    for event in messages.read() {
        if event.is_input() {
            input.write(InputEvent(event.clone()));
        }
    }
}

pub(crate) fn pump_events(
    mut exit: MessageWriter<AppExit>,
    mut messages: MessageWriter<Event>,
    low_power_mode: Option<Res<LowPowerMode>>,
    incoming_events: Option<NonSend<Receiver<Event>>>,
    platform: Option<NonSendMut<Pin<Box<PlatformCallbacks>>>>,
    activity: FrameActivity,
    mut timeout: Local<u32>,
) {
    let Some((ref mut platform, incoming_events)) = platform.zip(incoming_events) else {
        // No platform interface or incoming event pipe - probably executing in a unit test.
        return;
    };

    // Deliberately not paced to a frame period: waiting a full period put a
    // floor under how soon a pump could start, so an event landing right after
    // one returned had to wait out the rest of it. That latency is worse than
    // the redundant work skipping the wait costs.
    platform.pump_cocoa_event_loop(f64::from(*timeout) / 1000.0);

    let deadline = Instant::now() + PUMP_BUDGET;
    let mut received_events = Vec::new();
    let mut pending_mouse = None;

    // `true` when the channel went quiet, `false` when a cap sent us home with
    // events still queued. Only the quiet case may back the poll timeout off.
    let drained = loop {
        // Checked before the receive so a burst cannot keep extending the stay:
        // whatever is left stays in the channel for the next frame.
        if received_events.len() >= PUMP_MAX_EVENTS || Instant::now() >= deadline {
            trace!(
                "pump_events: yielding the frame with {} events taken",
                received_events.len()
            );
            break false;
        }

        // Polled before any timed wait: the Cocoa pump above already did this
        // frame's sleeping, so a quiet channel used to cost another millisecond.
        let received = match incoming_events.try_recv() {
            Err(TryRecvError::Empty) => incoming_events.recv_timeout(Duration::from_millis(1)),
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            Ok(event) => Ok(event),
        };
        match received {
            Ok(Event::Exit) | Err(RecvTimeoutError::Disconnected) => {
                exit.write(AppExit::Success);
                return;
            }
            Ok(event) => {
                if matches!(event, Event::MouseMoved { .. }) {
                    pending_mouse = Some(event);
                } else {
                    received_events.extend(pending_mouse.take());
                    received_events.push(event);
                }
                *timeout = LOOP_TIMEOUT_STEP;
            }
            Err(RecvTimeoutError::Timeout) => break true,
        }
    };

    received_events.extend(pending_mouse.take());
    messages.write_batch(received_events);

    let frame_active = activity.mid_frame();
    let low_power = low_power_mode.is_some_and(|low_power| low_power.0);
    *timeout = next_pump_timeout(*timeout, drained, frame_active, low_power);
}

pub(crate) fn gather_initial_processes(
    receiver: Option<NonSendMut<Receiver<Event>>>,
    existing_config: Option<Res<Config>>,
    mut displays: Query<&mut Display>,
    mut commands: Commands,
) {
    let Some(receiver) = receiver else {
        // Probably running in a mock environment, ignore.
        return;
    };
    let mut initial_processes: Vec<BProcess> = Vec::new();
    let mut toml_config = None;
    loop {
        match receiver.recv().expect("error reading initial processes") {
            Event::ProcessesLoaded | Event::Exit => break,
            Event::ApplicationLaunched { psn, observer } => {
                let process: BProcess = Process::new(&psn, observer.clone()).into();
                if process.pid() != 0 {
                    initial_processes.push(process);
                } else {
                    debug!("Skipping process with PID 0 (likely kernel_task).");
                }
            }
            Event::InitialConfig(config) => {
                toml_config = Some(config);
            }
            event => warn!("Stray event during initial process gathering: {event:?}"),
        }
    }

    // A Lua `spool.setup{...}` config is inserted at build time and wins; the
    // TOML config drained from the channel is only the fallback. Use whichever
    // is authoritative for the force-track and menubar decisions below.
    let effective = existing_config
        .as_deref()
        .cloned()
        .or_else(|| toml_config.clone());

    if let Some(config) = &effective {
        let height = config.menubar_height();
        for mut display in &mut displays {
            display.set_menubar_height_override(height);
        }
    }

    while let Some(mut process) = initial_processes.pop() {
        let forced = effective
            .as_ref()
            .is_some_and(|c| c.should_force_track_process(&**process));

        if process.is_observable() || forced {
            if forced {
                debug!(
                    "Forcing tracking of existing process '{}' despite unobservable policy.",
                    process.name()
                );
                process.force_track(true);
            } else {
                debug!("Adding existing process {}", process.name());
            }
            commands.spawn((ExistingMarker, process));
        } else {
            debug!(
                "Existing application '{}' is not observable, ignoring it.",
                process.name(),
            );
        }
    }

    // The input event tap holds its own clone of the `Config` handle from
    // `InitialConfig` and reads swipe/scroll settings off it per event. A Lua
    // `spool.setup{...}` builds a fresh handle, so its settings must be
    // published into the tap's existing handle rather than replacing it, or
    // gestures would keep reading stale settings.
    match (existing_config.as_deref(), toml_config) {
        #[cfg(feature = "lua")]
        (Some(lua_config), Some(shared)) => {
            shared.replace_inner_from(lua_config);
            commands.insert_resource(shared);
        }
        (None, Some(config)) => commands.insert_resource(config),
        _ => {}
    }
}

#[derive(Default)]
pub(super) struct OverlayWindowConfigCache {
    window_id: Option<WinID>,
    application: Option<Entity>,
    incarnation: Option<WindowIncarnation>,
    focused_border_radius: Option<f64>,
    detected_border_radius: Option<f64>,
}

impl OverlayWindowConfigCache {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy)]
enum OverlayLayoutMode {
    Tiled,
    Floating,
}

#[derive(Clone, Copy)]
struct OverlayTargetState {
    mode: OverlayLayoutMode,
    eligible: bool,
    in_active_strip: bool,
    in_active_space: bool,
}

fn is_overlay_target(state: OverlayTargetState) -> bool {
    state.eligible
        && if matches!(state.mode, OverlayLayoutMode::Floating) {
            state.in_active_space
        } else {
            state.in_active_strip
        }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayDisposition {
    Render,
    Hide,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayWorkspaceState {
    Missing,
    Suppressed,
    Active,
}

fn overlay_disposition(
    enabled: bool,
    workspace: OverlayWorkspaceState,
    has_target: bool,
) -> OverlayDisposition {
    if !enabled || matches!(workspace, OverlayWorkspaceState::Missing) {
        OverlayDisposition::Remove
    } else if matches!(workspace, OverlayWorkspaceState::Suppressed) {
        OverlayDisposition::Hide
    } else if !has_target {
        OverlayDisposition::Remove
    } else {
        OverlayDisposition::Render
    }
}

fn confirmed_overlay_frame(observed: Option<&ObservedWindowFrame>) -> Option<IRect> {
    observed.map(|frame| frame.0)
}

#[cfg(test)]
mod overlay_target_tests {
    use super::{
        OverlayDisposition, OverlayLayoutMode, OverlayTargetState, OverlayWorkspaceState,
        confirmed_overlay_frame, is_overlay_target, overlay_disposition,
    };
    use crate::ecs::ObservedWindowFrame;
    use bevy::math::{IRect, IVec2};

    #[test]
    fn accepts_visible_tiled_or_floating_focus_in_active_space() {
        assert!(is_overlay_target(OverlayTargetState {
            mode: OverlayLayoutMode::Tiled,
            eligible: true,
            in_active_strip: true,
            in_active_space: true,
        }));
        assert!(is_overlay_target(OverlayTargetState {
            mode: OverlayLayoutMode::Floating,
            eligible: true,
            in_active_strip: false,
            in_active_space: true,
        }));
    }

    #[test]
    fn rejects_hidden_fullscreen_and_off_space_focus() {
        let mut state = OverlayTargetState {
            mode: OverlayLayoutMode::Floating,
            eligible: false,
            in_active_strip: false,
            in_active_space: true,
        };
        assert!(!is_overlay_target(state));
        state.eligible = false;
        assert!(!is_overlay_target(state));
        state.eligible = true;
        state.in_active_space = false;
        assert!(!is_overlay_target(state));
        state.mode = OverlayLayoutMode::Tiled;
        state.in_active_space = true;
        assert!(!is_overlay_target(state));
    }

    #[test]
    fn overlay_lifecycle_removes_when_configuration_is_disabled() {
        assert_eq!(
            overlay_disposition(false, OverlayWorkspaceState::Active, true),
            OverlayDisposition::Remove
        );
    }

    #[test]
    fn overlay_lifecycle_removes_without_an_active_workspace() {
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Missing, true),
            OverlayDisposition::Remove
        );
    }

    #[test]
    fn overlay_lifecycle_removes_when_the_target_disappears() {
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Active, false),
            OverlayDisposition::Remove
        );
    }

    #[test]
    fn overlay_lifecycle_hides_only_for_temporary_suppression() {
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Suppressed, true),
            OverlayDisposition::Hide
        );
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Active, true),
            OverlayDisposition::Render
        );
    }

    #[test]
    fn overlay_geometry_requires_a_confirmed_observed_frame() {
        let frame = IRect::from_corners(IVec2::new(10, 20), IVec2::new(310, 220));
        let observed = ObservedWindowFrame(frame);

        assert_eq!(confirmed_overlay_frame(Some(&observed)), Some(frame));
        assert_eq!(confirmed_overlay_frame(None), None);
    }
}

#[cfg(test)]
mod event_pump_timing_tests {
    use super::next_pump_timeout;

    #[test]
    fn active_window_animation_does_not_sleep_away_its_frame_budget() {
        assert_eq!(
            next_pump_timeout(16, true, true, false),
            1,
            "window animation must return to layout and AX presentation without first waiting a full frame"
        );
    }
}

#[derive(SystemParam)]
pub(super) struct OverlayInputs<'w, 's> {
    windows: Windows<'w, 's>,
    focus: Res<'w, FocusCoordinator>,
    applications: Query<'w, 's, &'static Application>,
    observed_frames: Query<'w, 's, &'static ObservedWindowFrame>,
    window_manager: Res<'w, WindowManager>,
    mission_control_active: Res<'w, MissionControlActive>,
    config: Res<'w, Config>,
}

fn resolve_overlay_target(
    inputs: &OverlayInputs,
    active_strip: &LayoutStrip,
) -> crate::errors::Result<Option<(NSRect, Entity)>> {
    let Some(entity) = inputs.focus.snapshot().confirmed_entity() else {
        return Ok(None);
    };
    let Some((window, _, state)) = inputs.windows.get_tracked(entity) else {
        return Ok(None);
    };
    let Some(frame) = confirmed_overlay_frame(inputs.observed_frames.get(entity).ok()) else {
        return Ok(None);
    };
    let floating = state.is_floating();
    let in_active_space = if floating {
        inputs
            .window_manager
            .windows_in_workspace(active_strip.id())?
            .contains(&window.id())
    } else {
        true
    };
    let target = OverlayTargetState {
        mode: if floating {
            OverlayLayoutMode::Floating
        } else {
            OverlayLayoutMode::Tiled
        },
        eligible: state.is_visible() && !window.is_full_screen(),
        in_active_strip: active_strip.contains(entity),
        in_active_space,
    };
    if !is_overlay_target(target) {
        return Ok(None);
    }

    let h_pad = window.horizontal_padding();
    let v_pad = window.vertical_padding();
    let frame = NSRect::new(
        NSPoint::new(
            f64::from(frame.min.x + h_pad),
            f64::from(frame.min.y + v_pad),
        ),
        NSSize::new(
            f64::from(frame.width() - 2 * h_pad),
            f64::from(frame.height() - 2 * v_pad),
        ),
    );
    Ok(Some((frame, entity)))
}

fn overlay_border_params(
    inputs: &OverlayInputs,
    window: &Window,
    application: Entity,
    cache: &mut OverlayWindowConfigCache,
) -> Option<crate::overlay::BorderParams> {
    use crate::overlay::BorderParams;

    if cache.window_id != Some(window.id())
        || cache.application != Some(application)
        || cache.incarnation != Some(window.incarnation())
        || inputs.config.is_changed()
    {
        let app = inputs.applications.get(application).ok()?;
        let properties = WindowProperties::new(app, window, &inputs.config);
        cache.window_id = Some(window.id());
        cache.application = Some(application);
        cache.incarnation = Some(window.incarnation());
        cache.focused_border_radius = properties.border_radius();
        cache.detected_border_radius = window.border_radius();
    }

    let calculated_radius = match inputs.config.border_radius() {
        BorderRadiusOption::Auto => cache.detected_border_radius.unwrap_or(10.0),
        BorderRadiusOption::Value(value) => value.max(0.0),
    };
    Some(BorderParams {
        color: inputs.config.border_color(),
        opacity: inputs.config.border_opacity(),
        width: inputs.config.border_width(),
        radius: cache.focused_border_radius.unwrap_or(calculated_radius),
    })
}

pub(super) fn update_overlays(
    // Gating lives in the `overlay_dirty` run condition; this query just
    // resolves the current active workspace.
    active_workspace: Query<(Has<Scrolling>, &LayoutStrip), With<ActiveWorkspaceMarker>>,
    inputs: OverlayInputs,
    overlay_mgr: Option<NonSendMut<OverlayManager>>,
    mut window_config_cache: Local<OverlayWindowConfigCache>,
) {
    let Some(mut overlay_mgr) = overlay_mgr else {
        return;
    };

    let dim_opacity = inputs.config.dim_inactive_opacity();
    let border_enabled = inputs.config.border_active_window();
    let enabled = dim_opacity != 0.0 || border_enabled;

    // Hide overlays during swipe, mission control, native fullscreen spaces,
    // or briefly after a space change (macOS space-switch animation).
    let active_workspace = active_workspace.iter().next();
    let workspace_state = match active_workspace {
        None => OverlayWorkspaceState::Missing,
        Some((swiping, strip))
            if swiping || inputs.mission_control_active.0 || strip.is_fullscreen() =>
        {
            OverlayWorkspaceState::Suppressed
        }
        Some(_) => OverlayWorkspaceState::Active,
    };
    match overlay_disposition(enabled, workspace_state, true) {
        OverlayDisposition::Remove => {
            overlay_mgr.remove_all();
            window_config_cache.clear();
            return;
        }
        OverlayDisposition::Hide => {
            overlay_mgr.hide_all();
            window_config_cache.clear();
            return;
        }
        OverlayDisposition::Render => {}
    }
    let Some((_, active_strip)) = active_workspace else {
        unreachable!("overlay disposition rejects a missing active workspace")
    };

    // Tiled membership belongs to ECS; floating Space membership belongs to
    // macOS. Both require a confirmed observed frame.
    let overlay_target = match resolve_overlay_target(&inputs, active_strip) {
        Ok(target) => target,
        Err(error) => {
            // Space membership is a transient private-API read. Preserve the
            // overlay allocation and retry on the periodic refresh instead of
            // turning one failed query into a persistent missing border.
            warn!(%error, "unable to resolve overlay target in active Space");
            overlay_mgr.hide_all();
            window_config_cache.clear();
            return;
        }
    };
    if overlay_disposition(
        enabled,
        OverlayWorkspaceState::Active,
        overlay_target.is_some(),
    ) == OverlayDisposition::Remove
    {
        // The target is no longer part of the authoritative inventory. Destroy
        // the surfaces so a missed close notification cannot leave a ghost border.
        overlay_mgr.remove_all();
        window_config_cache.clear();
        return;
    }
    let Some((focused_abs_cg, focused_entity)) = overlay_target else {
        unreachable!("overlay disposition rejects a missing target")
    };
    let Some((focused_window, _, focused_application)) = inputs.windows.get_parent(focused_entity)
    else {
        overlay_mgr.remove_all();
        window_config_cache.clear();
        return;
    };
    let focused_window_id = focused_window.id();

    let border_params = if border_enabled {
        let Some(params) = overlay_border_params(
            &inputs,
            focused_window,
            focused_application,
            &mut window_config_cache,
        ) else {
            overlay_mgr.remove_all();
            window_config_cache.clear();
            return;
        };
        Some(params)
    } else {
        window_config_cache.clear();
        None
    };

    let dim_color = inputs.config.dim_inactive_color();
    overlay_mgr.update(
        dim_opacity,
        dim_color,
        Some(focused_abs_cg),
        Some(focused_window_id),
        border_params.as_ref(),
    );
}

#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn commit_window_frame(
    mut windows: PendingWindowFrames,
    layout_strips: Query<(&LayoutStrip, &ChildOf), Without<PendingSpaceDestruction>>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    config: Res<Config>,
    settling: Res<super::window_geometry::WindowGeometrySettling>,
    mut commands: Commands,
) {
    for (entity, mut window, presented, desired, mut width_ratio, observed, floating) in
        &mut windows
    {
        let native_fullscreen = layout_strips
            .iter()
            .any(|(strip, _)| strip.is_fullscreen() && strip.contains(entity));
        if native_fullscreen || window.try_is_full_screen().unwrap_or(true) {
            continue;
        }
        if settling.contains(entity) {
            continue;
        }
        let target = presented.0;
        if !floating
            && let Some(display_width) = layout_strips
                .iter()
                .find_map(|(strip, child)| strip.contains(entity).then_some(child.parent()))
                .and_then(|display_entity| displays.get(display_entity).ok())
                .map(|(display, dock)| display.actual_display_bounds(dock, &config).width())
                .filter(|width| *width > 0)
        {
            let desired_ratio = f64::from(desired.0.width()) / f64::from(display_width);
            if (width_ratio.0 - desired_ratio).abs() > f64::EPSILON {
                width_ratio.0 = desired_ratio;
            }
        }
        let observed_frame = observed.as_ref().map(|observed| observed.0);
        let needs_size_write =
            observed_frame.is_none_or(|observed| observed.size() != target.size());
        let height_only_resize = observed_frame.is_some_and(|observed| {
            observed.min == target.min
                && observed.width() == target.width()
                && observed.height() != target.height()
        });
        let result = if height_only_resize {
            // Dock/menu-bar changes commonly alter only the usable height.
            // Avoid the complete frame sequence (size/read/position/size/read)
            // unless AX unexpectedly moves the origin or the width also changes.
            window.resize_preserving_origin(target)
        } else if needs_size_write {
            // A size write may move the window. Supply the complete presented
            // frame so position and size remain one atomic projection.
            window.set_frame(target)
        } else {
            window.reposition(target.min)
        };

        match result {
            Ok(frame) => {
                if let Some(mut observed) = observed {
                    if observed.0 != frame {
                        observed.0 = frame;
                    }
                } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_insert(ObservedWindowFrame(frame));
                }
            }
            Err(error) => {
                warn!(window_id = window.id(), %error, "unable to commit window frame");
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_remove::<ObservedWindowFrame>();
                }
            }
        }
    }
}

#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn verify_window_position(
    mut windows: PositionVerificationWindows,
    mut commands: Commands,
) {
    for (entity, mut window, desired, mut verification, mut observed) in &mut windows {
        let mut confirmed_frame = window.update_frame().ok();
        let already_positioned = confirmed_frame.is_some_and(|frame| frame.min == desired.0.min);

        if !already_positioned {
            match window.reposition(desired.0.min) {
                Ok(frame) => confirmed_frame = Some(frame),
                Err(error) => {
                    warn!(window_id = window.id(), %error, "unable to retry window position");
                    confirmed_frame = None;
                }
            }
        }

        if let Some(frame) = confirmed_frame {
            if let Some(observed) = observed.as_deref_mut() {
                if observed.0 != frame {
                    observed.0 = frame;
                }
            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(ObservedWindowFrame(frame));
            }
        } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<ObservedWindowFrame>();
        }

        let positioned = confirmed_frame.is_some_and(|frame| frame.min == desired.0.min);
        if (positioned || verification.tick())
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            entity_commands.try_remove::<VerifyWindowPosition>();
        }
    }
}

/// Clears visual effects owned by Spool before shutdown. Launch-frame
/// restoration is handled separately in the final schedule so no layout or
/// animation system can overwrite it.
pub(super) fn cleanup_on_exit(
    mut exit_events: MessageReader<AppExit>,
    all_windows: Query<&Window>,
    window_manager: Res<WindowManager>,
    mut overlay_mgr: Option<NonSendMut<OverlayManager>>,
) {
    for _ in exit_events.read() {
        let ids = all_windows.iter().map(|w| w.id()).collect::<Vec<_>>();
        info!("exit cleanup: restoring {} window(s)", ids.len());
        window_manager.dim_windows(&ids, 0.0);

        if let Some(ref mut overlay_mgr) = overlay_mgr {
            overlay_mgr.remove_all();
        }
    }
}

pub(crate) fn update_flash_messages(
    messages: Populated<(Entity, &FlashMessage, &Timeout)>,
    active_display: Single<(&Display, Entity), With<ActiveDisplayMarker>>,
    flash_mgr: Option<NonSendMut<FlashMessageManager>>,
    mut commands: Commands,
) {
    let Some(mut flash_manager) = flash_mgr else {
        return;
    };

    if messages.is_empty() {
        flash_manager.remove();
        return;
    }

    let (display, _) = *active_display;
    let bounds = display.bounds();
    let top_right = NSPoint::new(f64::from(bounds.max.x), f64::from(bounds.min.y));

    // When several FlashMessages coexist (rapid keypresses spawn a fresh
    // one per workspace switch before the previous timer expires), the
    // naïve loop would call `show()` for every one of them in arbitrary
    // order — the OSD ends up flickering between strings, and the moment
    // any one of them expires its `is_finished()` branch calls
    // `flash_manager.remove()` even though the newer ones are still
    // alive. Keep the newest (most time remaining), despawn the rest,
    // and render exactly once.
    let mut alive: Option<(Entity, &str, &Timeout)> = None;
    let mut stale: Vec<Entity> = Vec::new();
    for (entity, FlashMessage(flash), timeout) in messages {
        if timeout.timer.is_finished() {
            stale.push(entity);
            continue;
        }
        match alive {
            None => alive = Some((entity, flash, timeout)),
            Some((prev_entity, _, prev_timeout)) => {
                if timeout.timer.remaining() > prev_timeout.timer.remaining() {
                    stale.push(prev_entity);
                    alive = Some((entity, flash, timeout));
                } else {
                    stale.push(entity);
                }
            }
        }
    }

    for entity in stale {
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_despawn();
        }
    }

    if let Some((_, flash, timeout)) = alive {
        let opacity = timeout.timer.fraction_remaining();
        flash_manager.show(flash, opacity, top_right);
    } else {
        flash_manager.remove();
    }
}

pub(crate) fn update_low_power_state(low_power_mode: Option<ResMut<LowPowerMode>>) {
    let Some(mut state) = low_power_mode else {
        return;
    };
    let process_info = objc2_foundation::NSProcessInfo::processInfo();
    state.0 = process_info.isLowPowerModeEnabled();
}

#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn window_creation_event(mut messages: MessageReader<Event>, mut commands: Commands) {
    for event in messages.read() {
        let Event::WindowCreated { element } = event else {
            continue;
        };

        if let Ok(window) = WindowOS::new(element)
            .inspect_err(|err| {
                trace!("not adding window {element:?}: {err}");
            })
            .map(|window| Window::new(Box::new(window)))
        {
            commands.trigger(SpawnWindowTrigger::new(vec![window]));
        }
    }
}

pub(crate) fn detect_tabbed_windows(
    created: Populated<(Entity, &ObservedWindowFrame, &ChildOf), Added<Window>>,
    windows: Query<(Entity, &Window, &ObservedWindowFrame, &ChildOf), With<Window>>,
    apps: Query<Entity, With<Application>>,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    window_manager: Res<WindowManager>,
    active_display: Single<&Display, With<ActiveDisplayMarker>>,
    mut commands: Commands,
) {
    let display_bounds = active_display.bounds();
    let Some(workspace_entities) = workspaces
        .iter()
        .find_map(|(strip, active)| active.then_some(strip.all_windows()))
    else {
        return;
    };

    for (entity, ObservedWindowFrame(frame), child) in created {
        let Ok(app_entity) = apps.get(child.parent()) else {
            continue;
        };

        // First find all the windows which have the same size and the same parent app.
        // .. and in the same workspace.
        let mut same_size = workspace_entities
            .iter()
            .filter_map(|e| windows.get(*e).ok())
            .filter(|(leader, _, leader_frame, child)| {
                *leader != entity
                    && child.parent() == app_entity
                    && leader_frame.0.size().chebyshev_distance(frame.size()) <= 1
            })
            .collect::<Vec<_>>();

        // Now check whether any of these found windows have the same position?
        let tabbed = same_size
            .iter()
            .find_map(|(leader, window, leader_frame, _)| {
                // If the window has a positional match, it's tabbed!
                (leader_frame.0.min.chebyshev_distance(frame.min) <= 1)
                    .then_some((*leader, window.id()))
            })
            .or_else(|| {
                // Otherwise if no windows were found by position, sort all the windows by distance
                // and then pick the one which is currently offscreen.
                // This heuristic relaxes the position matching, because the window is bumped into view.
                same_size.sort_by_key(|(_, _, candidate_frame, _)| {
                    frame.min.x.abs_diff(candidate_frame.0.min.x)
                });
                same_size
                    .into_iter()
                    .find_map(|(leader, window, leader_frame, _)| {
                        let offscreen = !display_bounds.contains(leader_frame.0.min)
                            || !display_bounds.contains(leader_frame.0.max);
                        offscreen.then_some((leader, window.id()))
                    })
            });

        if let Some((leader, leader_id)) = tabbed
            && window_manager
                .windows_on_screen()
                .is_some_and(|ids| !ids.contains(&leader_id))
            && let Some((mut strip, _)) =
                workspaces.iter_mut().find(|strip| strip.0.contains(leader))
            && strip.contains(leader)
        {
            debug!("Tabbed window detected: adding {entity} to leader {leader}");
            if strip
                .convert_to_tabs(leader, entity)
                .inspect_err(|err| error!("Failed to convert to tabs: {err}"))
                .is_ok()
            {
                commands.focus_entity(entity, false);
            }
        }
    }
}

#[cfg(all(test, feature = "lua"))]
mod tests {
    use std::sync::mpsc::channel;

    use bevy::prelude::*;

    use super::gather_initial_processes;
    use crate::config::Config;
    use crate::events::Event;

    /// The input event tap keeps the handle it received on `InitialConfig` and
    /// reads swipe settings off it per event, so the Lua config must land in
    /// *that* handle rather than in a fresh one only the ECS can see.
    #[test]
    fn lua_config_reaches_the_handle_the_event_tap_holds() {
        let lua_config: Config = "[options]\n[swipe.gesture]\nfingers_count = 3\n"
            .try_into()
            .expect("config should parse");
        let tap_config = Config::defaults().expect("defaults should parse");
        assert_eq!(tap_config.swipe_gesture_fingers(), None);

        let (sender, receiver) = channel();
        sender
            .send(Event::InitialConfig(tap_config.clone()))
            .expect("send initial config");
        sender.send(Event::ProcessesLoaded).expect("send loaded");

        let mut app = App::new();
        app.insert_resource(lua_config);
        app.insert_non_send(receiver);
        app.add_systems(Update, gather_initial_processes);
        app.update();

        assert_eq!(tap_config.swipe_gesture_fingers(), Some(3));
    }
}

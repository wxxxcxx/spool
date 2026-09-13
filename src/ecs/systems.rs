use bevy::app::AppExit;
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::{ChildOf, Children};
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::ecs::query::{Added, Changed, Has, Or, With, Without};
use bevy::ecs::system::{
    Commands, Local, NonSend, NonSendMut, Populated, Query, Res, ResMut, Single, SystemParam,
};
use bevy::math::IRect;
use bevy::time::Time;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};
use tracing::{Level, debug, error, info, instrument, trace, warn};

use super::{
    ActiveDisplayMarker, BProcess, ExistingMarker, FreshMarker, RepositionMarker, ResizeMarker,
    RetryFrontSwitch, SpawnWindowTrigger, Timeout, VerifyWindowPosition,
};

use crate::config::{Config, decorations::BorderRadiusOption};
use crate::ecs::focus::{FocusCoordinator, FocusSignal};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space::{NativeMoveOwner, NativeSpace, SpaceKind, VisibleNativeSpaceMarker};
use crate::ecs::params::{FrameActivity, Windows};
use crate::ecs::reconcile::{WindowStateSync, WindowUnavailable};
use crate::ecs::window_frame::{
    DefaultWindowFrame, DisplayTransferFrame, DisplayTransferReadback, InteractiveWindowFrame,
    WindowFrameCorrection,
};
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DesiredWindowFrame, DockPosition, FlashMessage, Floating,
    Initializing, LowPowerMode, MissionControlActive, ObservedWindowFrame, Position,
    PresentedWindowFrame, ReadDisplayProperties, Scrolling, SendMessageTrigger,
    WindowFrameCommitSuspended, WindowFrameMotion, WindowProperties, WindowVisibility,
};
use crate::events::{Event, FocusSource, InputEvent};
use crate::manager::discovery::{DiscoveryOwner, WindowDiscovery};
use crate::manager::{Application, Display, Process, Window, WindowManager, WindowOS};
use crate::overlay::{FlashMessageManager, OverlayManager, SpaceOverlayTarget};
use crate::platform::{PlatformCallbacks, WinID, WindowIncarnation, WorkspaceId};

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
        &'static mut PresentedWindowFrame,
        &'static mut DesiredWindowFrame,
        Option<&'static mut ObservedWindowFrame>,
        (&'static mut Position, &'static mut Bounds),
        (
            Option<&'static DefaultWindowFrame>,
            Has<super::WindowDefaultsPending>,
        ),
        (
            Option<&'static DisplayTransferFrame>,
            Has<NativeMoveOwner>,
            Has<WindowVisibility>,
        ),
        (
            Has<Floating>,
            Option<&'static WindowFrameCorrection>,
            Option<&'static InteractiveWindowFrame>,
            Has<WindowFrameCommitSuspended>,
            Has<WindowFrameMotion>,
            Has<WindowUnavailable>,
            Has<WindowSpaceReassignmentPending>,
        ),
    ),
    Or<(
        Changed<PresentedWindowFrame>,
        With<WindowFrameCorrection>,
        With<InteractiveWindowFrame>,
        With<DefaultWindowFrame>,
        With<DisplayTransferFrame>,
    )>,
>;

type PositionVerificationWindows<'w, 's> = Populated<
    'w,
    's,
    (
        Entity,
        &'static mut Window,
        &'static mut PresentedWindowFrame,
        &'static mut VerifyWindowPosition,
        Option<&'static mut ObservedWindowFrame>,
    ),
    (
        Without<WindowSpaceReassignmentPending>,
        Without<WindowUnavailable>,
        Without<WindowFrameCommitSuspended>,
        Without<WindowFrameMotion>,
    ),
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
pub fn gather_displays(topology: Res<super::topology::NativeTopology>, mut commands: Commands) {
    let Some(active_display_id) = topology.active_display() else {
        error!("Unable to get active display id!");
        return;
    };
    for observation in topology.displays().into_iter().flatten() {
        let display = &observation.display;
        let display_id = display.id();
        let entity = if display_id == active_display_id {
            commands.spawn((display.clone(), ActiveDisplayMarker))
        } else {
            commands.spawn(display.clone())
        }
        .id();

        commands.trigger(ReadDisplayProperties(entity));
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
    processes: Populated<(Entity, &mut BProcess), With<ExistingMarker>>,
    mut commands: Commands,
) {
    for (entity, mut process) in processes {
        if !process.ready() {
            commands
                .entity(entity)
                .remove::<ExistingMarker>()
                .insert(FreshMarker);
            continue;
        }
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
    mut discovery: Option<NonSendMut<WindowDiscovery>>,
    mut commands: Commands,
) {
    let spaces = workspaces
        .into_iter()
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();

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
            // Only real platform setup installs this NonSend owner. Mock
            // inventories must never fall through to native AX discovery.
            if let Some(discovery) = discovery.as_mut() {
                discovery.enqueue(
                    DiscoveryOwner {
                        application: entity,
                        pid: app.pid(),
                        psn: app.psn(),
                    },
                    app.bundle_id(),
                    offscreen_windows,
                    config.clone(),
                );
            } else {
                debug!(pid = app.pid(), "native discovery is not installed");
            }
        }
    }
}

fn discovery_owner_is_running(
    owner: DiscoveryOwner,
    application: Option<&Application>,
) -> crate::errors::Result<bool> {
    let Some(application) = application else {
        return Ok(false);
    };
    if application.pid() != owner.pid || application.psn() != owner.psn {
        return Ok(false);
    }
    application.is_running()
}

pub(crate) fn advance_window_discovery(
    mut discovery: Option<NonSendMut<WindowDiscovery>>,
    applications: Query<&Application>,
    exiting: Option<Res<super::exit_restore::ExitInProgress>>,
    mut commands: Commands,
) {
    let Some(discovery) = discovery.as_mut() else {
        return;
    };
    if exiting.is_some() {
        discovery.cancel();
        return;
    }
    for (owner, window) in discovery.advance(|owner| {
        discovery_owner_is_running(owner, applications.get(owner.application).ok())
    }) {
        // Discovery revalidates the exact owner after probing, before returning
        // publications. No native calls intervene before these spawn commands.
        commands.trigger(SpawnWindowTrigger::for_application(
            owner.application,
            vec![window],
        ));
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
    discovery: Option<NonSend<WindowDiscovery>>,
    mut workspaces: Query<&mut LayoutStrip>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !process_query.is_empty() {
        // The other two add_* functions are still running..
        return;
    }

    // Keep Initializing until discovery drains and its final spawn commands
    // have had a tick to settle. Restore's grace period must start afterwards.
    if discovery.is_some_and(|discovery| discovery.is_pending()) {
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
}

/// Handles the event when a new application is launched. It creates a `Process` and `Application` object,
/// observes the application for events, and adds its windows to the manager.
/// This system processes `BProcess` entities marked with `FreshMarker`.
/// If the process is not yet ready, it continues observing it. If ready, it attempts to create and observe an `Application`.
/// Observer registration is retried by reconciliation; an empty window list does
/// not limit the lifetime of an otherwise running application.
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

        if !app.observe().is_ok_and(|good| good) {
            debug!("some observers need reconciliation for {}", process.name());
        }
        // A running accessory app can have no windows for hours. Observation
        // failure or an empty inventory is not an application lifetime signal.
        commands.spawn((app, FreshMarker, ChildOf(entity)));
        commands.entity(entity).remove::<FreshMarker>();
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
    app_query: Populated<(&mut Application, Entity), With<FreshMarker>>,
    windows: Windows,
    config: Res<Config>,
    mut commands: Commands,
) {
    // TODO: maybe refactor this with add_existing_application_windows()
    let find_window = |window_id| windows.find(window_id);

    for (app, entity) in app_query {
        let mut create_windows = app.window_list(&config);
        // Retain the non-existing windows, so they can be created.
        create_windows.retain(|window| find_window(window.id()).is_none());

        // Subsequent discoveries and observer retries belong to reconciliation,
        // including applications that have no eligible window yet.
        commands.entity(entity).remove::<FreshMarker>();
        if !create_windows.is_empty() {
            debug!(
                "spawn! (polling path found {} new windows for {entity})",
                create_windows.len(),
            );
            commands.trigger(SpawnWindowTrigger::for_application(entity, create_windows));
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
    let frame_active = activity.mid_frame();
    if frame_active {
        // Apply before sleeping as well as after draining: discovery may have
        // been queued in Update after the previous pump chose an idle timeout.
        *timeout = LOOP_TIMEOUT_STEP;
    }
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
    let mut initial_config = None;
    loop {
        match receiver.recv().expect("error reading initial processes") {
            Event::ProcessesLoaded | Event::Exit => break,
            Event::ApplicationLaunched { psn, observer } => {
                let process: BProcess = Process::new(&psn, observer.clone()).into();
                if process.pid() == 0 {
                    debug!("Skipping process with PID 0 (likely kernel_task).");
                } else if crate::ecs::is_own_process(process.pid()) {
                    debug!("Skipping Spool's own process in the window lifecycle");
                } else {
                    initial_processes.push(process);
                }
            }
            Event::InitialConfig(config) => {
                initial_config = Some(config);
            }
            event => warn!("Stray event during initial process gathering: {event:?}"),
        }
    }

    // A Lua `spool.setup{...}` config is inserted at build time and wins; the
    // initial config drained from the channel is only the fallback. Use whichever
    // is authoritative for the force-track and menubar decisions below.
    let effective = existing_config
        .as_deref()
        .cloned()
        .or_else(|| initial_config.clone());

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
    match (existing_config.as_deref(), initial_config) {
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

fn overlay_target_is_eligible(visible: bool, fullscreen: bool) -> bool {
    visible && !fullscreen
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayDisposition {
    Render,
    Preserve,
    Hide,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayWorkspaceState {
    Missing,
    Suppressed,
    Active,
}

fn overlay_disposition(enabled: bool, workspace: OverlayWorkspaceState) -> OverlayDisposition {
    if !enabled {
        OverlayDisposition::Remove
    } else if matches!(workspace, OverlayWorkspaceState::Suppressed) {
        OverlayDisposition::Hide
    } else if matches!(workspace, OverlayWorkspaceState::Missing) {
        OverlayDisposition::Preserve
    } else {
        OverlayDisposition::Render
    }
}

fn overlay_workspace_state(
    active_workspace: Option<(bool, &LayoutStrip)>,
    mission_control_active: bool,
) -> OverlayWorkspaceState {
    if mission_control_active {
        return OverlayWorkspaceState::Suppressed;
    }
    match active_workspace {
        None => OverlayWorkspaceState::Missing,
        Some((true, _)) => OverlayWorkspaceState::Suppressed,
        Some(_) => OverlayWorkspaceState::Active,
    }
}

fn confirmed_overlay_frame(observed: Option<&ObservedWindowFrame>) -> Option<IRect> {
    observed.map(|frame| frame.0)
}

fn native_space_has_overlay(kind: SpaceKind) -> bool {
    !matches!(kind, SpaceKind::Fullscreen)
}

#[cfg(test)]
mod overlay_target_tests {
    use super::{
        OverlayDisposition, OverlayLayoutMode, OverlayTargetState, OverlayWorkspaceState,
        confirmed_overlay_frame, is_overlay_target, native_space_has_overlay, overlay_disposition,
        overlay_target_is_eligible, overlay_workspace_state,
    };
    use crate::ecs::ObservedWindowFrame;
    use crate::ecs::native_space::SpaceKind;
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
    fn rejects_a_visible_fullscreen_focus_as_the_overlay_cutout() {
        assert!(!overlay_target_is_eligible(true, true));
    }

    #[test]
    fn creates_overlays_only_for_ordinary_native_spaces() {
        assert!(native_space_has_overlay(SpaceKind::User));
        assert!(!native_space_has_overlay(SpaceKind::Fullscreen));
    }

    #[test]
    fn rejects_hidden_and_off_space_focus() {
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
            overlay_disposition(false, OverlayWorkspaceState::Active),
            OverlayDisposition::Remove
        );
    }

    #[test]
    fn overlay_lifecycle_preserves_surfaces_without_an_active_workspace() {
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Missing),
            OverlayDisposition::Preserve
        );
    }

    #[test]
    fn mission_control_hides_overlays_even_without_an_active_workspace() {
        assert_eq!(
            overlay_disposition(true, overlay_workspace_state(None, true)),
            OverlayDisposition::Hide,
        );
        assert_eq!(
            overlay_disposition(true, overlay_workspace_state(None, false)),
            OverlayDisposition::Preserve,
        );
    }

    #[test]
    fn overlay_lifecycle_hides_only_for_temporary_suppression() {
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Suppressed),
            OverlayDisposition::Hide
        );
        assert_eq!(
            overlay_disposition(true, OverlayWorkspaceState::Active),
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
    use bevy::ecs::{system::SystemState, world::World};

    use super::next_pump_timeout;
    use crate::ecs::{Initializing, params::FrameActivity};

    #[test]
    fn active_window_animation_does_not_sleep_away_its_frame_budget() {
        assert_eq!(
            next_pump_timeout(16, true, true, false),
            1,
            "window animation must return to layout and AX presentation without first waiting a full frame"
        );
    }

    #[test]
    fn pending_discovery_keeps_the_startup_pump_at_one_millisecond() {
        let mut world = World::new();
        // finish_setup retains this until discovery and its final publication settle.
        world.insert_resource(Initializing);
        let mut activity = SystemState::<FrameActivity>::new(&mut world);
        for (low_power, current) in [(false, 50), (true, 500)] {
            assert_eq!(
                next_pump_timeout(
                    current,
                    true,
                    activity.get(&world).unwrap().mid_frame(),
                    low_power
                ),
                1,
                "discovery must progress even without OS events, including low-power mode"
            );
        }
        world.remove_resource::<Initializing>();
        assert_eq!(
            next_pump_timeout(16, true, activity.get(&world).unwrap().mid_frame(), false),
            17
        );
    }
}

#[cfg(test)]
mod discovery_lifecycle_tests {
    use super::*;
    use crate::errors::{Error, Result};
    use crate::manager::app::MockApplicationApi;
    use crate::platform::ProcessSerialNumber;

    fn owner() -> DiscoveryOwner {
        DiscoveryOwner {
            application: Entity::from_raw_u32(1).unwrap(),
            pid: 42,
            psn: ProcessSerialNumber { high: 0, low: 7 },
        }
    }

    fn application(owner: DiscoveryOwner, running: Result<bool>) -> Application {
        let mut application = MockApplicationApi::new();
        application.expect_pid().return_const(owner.pid);
        application.expect_psn().return_const(owner.psn);
        application
            .expect_is_running()
            .returning(move || running.clone());
        Application::new(Box::new(application))
    }

    #[test]
    fn discovery_rejects_missing_exited_and_reused_application_owners() {
        let original = owner();
        assert!(!discovery_owner_is_running(original, None).unwrap());
        let exited = application(original, Ok(false));
        assert!(!discovery_owner_is_running(original, Some(&exited)).unwrap());
        let replacement = application(
            DiscoveryOwner {
                psn: ProcessSerialNumber { high: 0, low: 8 },
                ..original
            },
            Ok(true),
        );
        assert!(!discovery_owner_is_running(original, Some(&replacement)).unwrap());
        let other_pid = application(
            DiscoveryOwner {
                pid: 43,
                ..original
            },
            Ok(true),
        );
        assert!(!discovery_owner_is_running(original, Some(&other_pid)).unwrap());
    }

    #[test]
    fn discovery_does_not_treat_unknown_liveness_as_process_exit() {
        let original = owner();
        let live = application(original, Ok(true));
        assert!(discovery_owner_is_running(original, Some(&live)).unwrap());
        let unknown = application(original, Err(Error::Generic("unavailable".to_string())));
        assert!(discovery_owner_is_running(original, Some(&unknown)).is_err());
    }

    #[test]
    fn discovery_mock_startup_finishes_without_installing_native_ax() {
        let mut harness = crate::tests::TestHarness::new();
        assert!(!harness.app.world().contains_non_send::<WindowDiscovery>());
        for _ in 0..3 {
            harness.app.update();
        }
        assert!(!harness.app.world().contains_resource::<Initializing>());
        assert!(!harness.app.world().contains_non_send::<WindowDiscovery>());
    }
}

#[derive(SystemParam)]
pub(super) struct OverlayInputs<'w, 's> {
    windows: Windows<'w, 's>,
    focus: Res<'w, FocusCoordinator>,
    applications: Query<'w, 's, &'static Application>,
    displays: Query<'w, 's, &'static Display>,
    observed_frames: Query<'w, 's, &'static ObservedWindowFrame>,
    window_manager: Res<'w, WindowManager>,
    mission_control_active: Res<'w, MissionControlActive>,
    config: Res<'w, Config>,
}

fn resolve_overlay_target(
    inputs: &OverlayInputs,
    active_strip: &LayoutStrip,
    entity: Entity,
) -> crate::errors::Result<Option<(NSRect, Entity)>> {
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
        eligible: overlay_target_is_eligible(state.is_visible(), window.is_full_screen()),
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

fn resolve_space_overlay_target(
    inputs: &OverlayInputs,
    strip: &LayoutStrip,
    active: bool,
    visible: bool,
) -> crate::errors::Result<Option<(NSRect, Entity)>> {
    // A Space visible on an unfocused display remains fully dimmed. Hidden
    // Spaces retain their last navigation target so WindowServer already has
    // complete contents when it begins a Space transition.
    if visible && !active {
        return Ok(None);
    }

    let snapshot = inputs.focus.snapshot();
    let candidates = if active {
        [
            snapshot.requested_entity(),
            snapshot.confirmed_entity(),
            inputs.focus.navigation_entity(strip.id()),
        ]
    } else {
        [inputs.focus.navigation_entity(strip.id()), None, None]
    };

    let mut visited = HashSet::new();
    for entity in candidates.into_iter().flatten() {
        if visited.insert(entity)
            && let Some(target) = resolve_overlay_target(inputs, strip, entity)?
        {
            return Ok(Some(target));
        }
    }
    Ok(None)
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

type OverlaySpaces<'w, 's> = Query<
    'w,
    's,
    (
        Has<Scrolling>,
        &'static LayoutStrip,
        &'static NativeSpace,
        &'static ChildOf,
        Has<ActiveWorkspaceMarker>,
        Has<VisibleNativeSpaceMarker>,
    ),
    Without<PendingSpaceDestruction>,
>;

pub(super) fn update_overlays(
    spaces: OverlaySpaces,
    inputs: OverlayInputs,
    overlay_mgr: Option<NonSendMut<OverlayManager>>,
    mut window_config_cache: Local<HashMap<WorkspaceId, OverlayWindowConfigCache>>,
    mut diagnostic_targets: Local<Vec<SpaceOverlayTarget>>,
) {
    let Some(mut overlay_mgr) = overlay_mgr else {
        return;
    };

    let dim_opacity = inputs.config.dim_inactive_opacity();
    let border_enabled = inputs.config.border_active_window();
    let enabled = dim_opacity != 0.0 || border_enabled;

    if spaces.is_empty() {
        overlay_mgr.remove_all();
        window_config_cache.clear();
        return;
    }

    let active_workspace = spaces
        .iter()
        .find_map(|(scrolling, strip, _, _, active, _)| active.then_some((scrolling, strip)));
    let workspace_state =
        overlay_workspace_state(active_workspace, inputs.mission_control_active.0);
    match overlay_disposition(enabled, workspace_state) {
        OverlayDisposition::Remove => {
            overlay_mgr.remove_all();
            window_config_cache.clear();
            return;
        }
        OverlayDisposition::Hide => {
            overlay_mgr.hide_all();
            return;
        }
        OverlayDisposition::Preserve => return,
        OverlayDisposition::Render => {}
    }

    if !border_enabled {
        window_config_cache.clear();
    }

    let mut target_space_ids = HashSet::new();
    let mut targets = Vec::new();
    for (_, strip, native_space, child, active, visible) in &spaces {
        if !native_space_has_overlay(native_space.kind) {
            continue;
        }
        let Ok(display) = inputs.displays.get(child.parent()) else {
            continue;
        };
        target_space_ids.insert(strip.id());

        let focused = match resolve_space_overlay_target(&inputs, strip, active, visible) {
            Ok(focused) => focused,
            Err(error) => {
                // Space membership is a transient private-API read. Keep all
                // existing Space surfaces unchanged and retry on the periodic
                // refresh rather than publishing a dim-only intermediate.
                warn!(space_id = strip.id(), %error, "unable to resolve Space overlay target");
                return;
            }
        };

        let (focused_abs_cg, focused_window_id, border) = if let Some((frame, entity)) = focused {
            let Some((window, _, application)) = inputs.windows.get_parent(entity) else {
                continue;
            };
            let border = if border_enabled {
                let cache = window_config_cache.entry(strip.id()).or_default();
                overlay_border_params(&inputs, window, application, cache)
            } else {
                None
            };
            (Some(frame), Some(window.id()), border)
        } else {
            window_config_cache.remove(&strip.id());
            (None, None, None)
        };

        targets.push(SpaceOverlayTarget {
            space_id: strip.id(),
            display_id: display.id(),
            focused_abs_cg,
            focused_window_id,
            border,
        });
    }
    window_config_cache.retain(|space_id, _| target_space_ids.contains(space_id));

    if tracing::enabled!(target: "spool::focus_diagnostics", tracing::Level::DEBUG)
        && *diagnostic_targets != targets
    {
        debug!(target: "spool::focus_diagnostics", focus = ?inputs.focus.snapshot(),
            marker_window = ?inputs.windows.focused().map(|(window, _)| window.id()),
            frames = ?targets.iter().map(|target| (target.space_id, target.focused_window_id, target.focused_abs_cg)).collect::<Vec<_>>(),
            "overlay_targets");
        diagnostic_targets.clone_from(&targets);
    }

    let dim_color = inputs.config.dim_inactive_color();
    overlay_mgr.update(dim_opacity, dim_color, &targets);
}

pub(super) fn animate_decoration_overlay(
    config: Res<Config>,
    overlay_mgr: Option<NonSendMut<OverlayManager>>,
) {
    let Some(mut overlay_mgr) = overlay_mgr else {
        return;
    };
    overlay_mgr.animate_decorations(config.animation_speed());
}

#[derive(SystemParam)]
pub(super) struct WindowFrameCommitCtx<'w, 's> {
    participants: super::params::LayoutParticipants<'w, 's>,
    windows: PendingWindowFrames<'w, 's>,
    layout_strips:
        Query<'w, 's, (&'static LayoutStrip, &'static ChildOf), Without<PendingSpaceDestruction>>,
    displays: Query<'w, 's, (&'static Display, Option<&'static DockPosition>)>,
    config: Res<'w, Config>,
    topology: ResMut<'w, super::topology::NativeTopology>,
    window_manager: Res<'w, WindowManager>,
    settling: ResMut<'w, super::window_geometry::WindowGeometrySettling>,
    sync: ResMut<'w, WindowStateSync>,
    time: Res<'w, Time>,
    commands: Commands<'w, 's>,
}

fn write_presented_frame(
    window: &mut Window,
    target: IRect,
    observed: Option<IRect>,
    complete_frame: bool,
) -> crate::errors::Result<IRect> {
    if complete_frame {
        return window.set_frame(target);
    }
    if observed == Some(target) {
        return Ok(target);
    }
    let height_only = observed.is_some_and(|frame| {
        frame.min == target.min
            && frame.width() == target.width()
            && frame.height() != target.height()
    });
    if height_only {
        // Dock/menu-bar changes need only a height write unless AX moves the origin.
        window.resize_preserving_origin(target)
    } else if observed.is_none_or(|frame| frame.size() != target.size()) {
        // Resizing can move the origin, so commit the complete presentation.
        window.set_frame(target)
    } else {
        window.reposition(target.min)
    }
}

fn suspend_failed_frame_commit(
    entity: Entity,
    window: &mut Window,
    desired: IRect,
    error: &crate::errors::Error,
    commands: &mut Commands,
) {
    if matches!(
        error.macos_code(),
        Some(accessibility_sys::kAXErrorCannotComplete | accessibility_sys::kAXErrorAPIDisabled)
    ) {
        // The write may have partially succeeded. Retrying an unresponsive AX
        // endpoint here can spend another timeout; bounded reconciliation owns
        // recovery, and no stale physical geometry may be published meanwhile.
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<(WindowFrameMotion, ObservedWindowFrame)>();
            entity_commands.try_insert(WindowFrameCommitSuspended::new(desired));
        }
        return;
    }
    // AX can partially mutate a frame before returning an error. Publish only
    // readback and leave retries to the bounded correction policy.
    let readback = window.update_frame();
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<WindowFrameMotion>();
        entity_commands.try_insert(WindowFrameCommitSuspended::new(desired));
        match readback {
            Ok(frame) => {
                entity_commands.try_insert(ObservedWindowFrame(frame));
            }
            Err(error) => {
                warn!(window_id = window.id(), %error, "unable to read back failed window frame write");
                entity_commands.try_remove::<ObservedWindowFrame>();
            }
        }
    }
}

#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn commit_window_frame(ctx: WindowFrameCommitCtx) {
    commit_window_frames(ctx, false);
}

pub(super) fn commit_default_window_frames(ctx: WindowFrameCommitCtx) {
    commit_window_frames(ctx, true);
}

#[allow(
    clippy::too_many_lines,
    reason = "serialize default, interactive, and corrective writes with their common readback policy"
)]
fn commit_window_frames(ctx: WindowFrameCommitCtx, defaults_phase: bool) {
    let WindowFrameCommitCtx {
        participants,
        mut windows,
        layout_strips,
        displays,
        config,
        mut topology,
        window_manager,
        mut settling,
        mut sync,
        time,
        mut commands,
    } = ctx;
    for (
        entity,
        mut window,
        mut presented,
        mut desired,
        observed,
        (mut position, mut bounds),
        (default_frame, defaults_pending),
        (transfer, transfer_owned, invisible),
        (floating, correction, interactive, suspended, moving, unavailable, reassigning),
    ) in &mut windows
    {
        if defaults_phase != default_frame.is_some() {
            continue;
        }
        if default_frame.is_some() {
            commands.entity(entity).remove::<DefaultWindowFrame>();
        }
        if correction.is_some() {
            commands.entity(entity).remove::<WindowFrameCorrection>();
        }
        if interactive.is_some() {
            commands.entity(entity).remove::<InteractiveWindowFrame>();
        }
        if transfer.is_some() {
            commands.entity(entity).remove::<DisplayTransferFrame>();
        }
        let transfer = transfer.filter(|request| {
            !defaults_phase
                && transfer_owned
                && reassigning
                && !invisible
                && request.tiled != floating
                && request.incarnation == window.incarnation()
                && topology.observe_visible_window_space(&window_manager, window.id())
                    == Some(request.source_space_id)
                && topology.visible_display_for_space(request.target_space_id)
                    == Some(request.display_id)
                && !topology.is_fullscreen(request.target_space_id)
                && displays.iter().any(|(display, dock)| {
                    display.id() == request.display_id
                        && display.checked_actual_display_bounds(dock, &config)
                            == Some(request.viewport)
                        && topology.known_displays().any(|(native, _)| {
                            native.id() == display.id() && !display.clone().update_geometry(native)
                        })
                })
                && layout_strips.iter().any(|(strip, child)| {
                    strip.id() == request.target_space_id
                        && !strip.is_fullscreen()
                        && displays
                            .get(child.parent())
                            .is_ok_and(|(display, _)| display.id() == request.display_id)
                })
        });
        if unavailable
            || (reassigning && transfer.is_none())
            || (default_frame.is_none() && defaults_pending)
            || !window
                .represented_window_id()
                .is_ok_and(|id| id == window.id())
        {
            sync.finish_frame_attempt(entity, desired.0, false);
            commands
                .entity(entity)
                .remove::<WindowFrameMotion>()
                .insert(WindowFrameCommitSuspended::new(desired.0));
            continue;
        }
        // An inactive native tab is an identity, not another physical window.
        // Keep its logical targets, but never move/resize it independently as
        // part of normal layout, animation, or drift correction.
        if !floating
            && default_frame.is_none()
            && transfer.is_none()
            && layout_strips
                .iter()
                .any(|(strip, _)| strip.is_inactive_tab(entity))
        {
            sync.finish_frame_attempt(entity, desired.0, false);
            commands
                .entity(entity)
                .remove::<WindowFrameMotion>()
                .insert(WindowFrameCommitSuspended::new(desired.0));
            continue;
        }
        if default_frame.is_some_and(|request| request.incarnation != window.incarnation()) {
            continue;
        }
        // Current pointer intent supersedes animation and audit correction;
        // all three still share the same native ownership checks below.
        let interactive = interactive
            .filter(|request| transfer.is_none() && request.incarnation == window.incarnation());
        let correcting = default_frame.is_none()
            && transfer.is_none()
            && interactive.is_none()
            && correction.is_some_and(|request| request.0 == desired.0)
            && !floating
            && !moving;
        if default_frame.is_none()
            && transfer.is_none()
            && interactive.is_none()
            && !correcting
            && (suspended || !presented.is_changed())
        {
            continue;
        }
        let native_fullscreen = layout_strips
            .iter()
            .any(|(strip, _)| strip.is_fullscreen() && strip.contains(entity));
        if native_fullscreen || window.try_is_full_screen().unwrap_or(true) {
            sync.finish_frame_attempt(entity, desired.0, false);
            commands
                .entity(entity)
                .remove::<WindowFrameMotion>()
                .insert(WindowFrameCommitSuspended::new(desired.0));
            continue;
        }
        if default_frame.is_none()
            && interactive.is_none()
            && transfer.is_none()
            && settling.contains(entity)
        {
            sync.finish_frame_attempt(entity, desired.0, false);
            commands
                .entity(entity)
                .remove::<WindowFrameMotion>()
                .insert(WindowFrameCommitSuspended::new(desired.0));
            continue;
        }
        let target = if let Some(request) = transfer {
            request.target
        } else if let Some(request) = default_frame {
            presented.bypass_change_detection().0 = request.target;
            request.target
        } else if let Some(request) = interactive {
            presented.bypass_change_detection().0 = request.target;
            request.target
        } else if correcting {
            desired.0
        } else {
            presented.0
        };
        let coordinated =
            !floating && default_frame.is_none() && interactive.is_none() && transfer.is_none();
        if coordinated {
            // Logical edits to hidden Spaces are valid; native effects wait for
            // the owning Space to become visible, without activating it.
            if invisible
                || !layout_strips.iter().any(|(strip, _)| {
                    strip.contains(entity)
                        && !strip.projection_is_blocked()
                        && !participants.height_blocked(strip)
                        && topology.visible_display_for_space(strip.id()).is_some()
                })
            {
                sync.finish_frame_attempt(entity, desired.0, false);
                commands
                    .entity(entity)
                    .remove::<WindowFrameMotion>()
                    .insert(WindowFrameCommitSuspended::new(desired.0));
                continue;
            }
            if let Some((strip, _)) = layout_strips
                .iter()
                .find(|(strip, _)| strip.contains(entity))
                && let Some(state) = strip
                    .index_of(entity)
                    .ok()
                    .and_then(|index| strip.column_state(index))
            {
                sync.bind_frame_intent(
                    entity,
                    window.incarnation(),
                    desired.0,
                    (
                        state.id,
                        state.intent_revision,
                        strip.structure_revision(),
                        state.height_revision,
                    ),
                );
            }
            if observed.as_ref().is_some_and(|frame| frame.0 == target) {
                sync.confirm_frame_convergence(entity, target, desired.0);
                continue;
            }
            if !sync.begin_frame_attempt(
                entity,
                window.incarnation(),
                desired.0,
                time.elapsed(),
                correcting,
            ) {
                commands
                    .entity(entity)
                    .remove::<WindowFrameMotion>()
                    .insert(WindowFrameCommitSuspended::new(desired.0));
                continue;
            }
        }
        if correcting {
            presented.bypass_change_detection().0 = target;
        }
        let diagnostic_started =
            tracing::enabled!(target: "spool::focus_diagnostics", tracing::Level::DEBUG)
                .then(Instant::now);
        let result = write_presented_frame(
            &mut window,
            target,
            observed.as_ref().map(|frame| frame.0),
            correcting || interactive.is_some() || default_frame.is_some() || transfer.is_some(),
        );

        match result {
            Ok(frame) => {
                if coordinated {
                    if !super::reconcile::frames_equivalent(frame, target) {
                        // A successful AX call is not proof of ownership or a
                        // permanent size constraint. Stop unknown competition.
                        sync.finish_frame_attempt(entity, desired.0, true);
                        commands
                            .entity(entity)
                            .remove::<WindowFrameMotion>()
                            .insert(WindowFrameCommitSuspended::new(desired.0));
                    } else if target == desired.0 {
                        sync.confirm_frame_convergence(entity, frame, desired.0);
                    }
                }
                if let Some(started) = diagnostic_started
                    && observed.as_ref().map(|observed| observed.0) != Some(frame)
                {
                    debug!(target: "spool::focus_diagnostics", window_id = window.id(), ?entity,
                        desired = ?desired.0, ?target, before = ?observed.as_ref().map(|frame| frame.0),
                        readback = ?frame, write_us = started.elapsed().as_micros(),
                        "frame_commit");
                }
                if let Some(request) = transfer {
                    commands.entity(entity).insert(DisplayTransferReadback {
                        frame,
                        incarnation: request.incarnation,
                        tiled: request.tiled,
                    });
                }
                if default_frame.is_some() {
                    if position.0 != frame.min {
                        position.0 = frame.min;
                    }
                    if bounds.0 != frame.size() {
                        bounds.0 = frame.size();
                    }
                    desired.set_if_neq(DesiredWindowFrame(frame));
                    presented.bypass_change_detection().0 = frame;
                    commands
                        .entity(entity)
                        .remove::<(WindowFrameMotion, WindowFrameCommitSuspended)>()
                        .insert(super::WindowDefaultsApplied);
                }
                if let Some(request) = interactive {
                    presented.bypass_change_detection().0 = frame;
                    commands
                        .entity(entity)
                        .remove::<(WindowFrameMotion, WindowFrameCommitSuspended)>();
                    if floating {
                        if position.0 != frame.min {
                            position.0 = frame.min;
                        }
                        if bounds.0 != frame.size() {
                            bounds.0 = frame.size();
                        }
                        desired.set_if_neq(DesiredWindowFrame(frame));
                    } else if let Some(context) = layout_strips.iter().find_map(|(strip, _)| {
                        super::window_geometry::GeometryContext::capture(
                            entity,
                            desired.0,
                            config.last_changed().get(),
                            strip,
                            &|member| participants.contains(member),
                        )
                    }) {
                        settling.record(
                            entity,
                            &window,
                            request.start,
                            frame,
                            time.elapsed(),
                            context,
                            true,
                        );
                    }
                }
                if correcting {
                    // Presented remains the requested endpoint; observation
                    // below independently records what macOS actually did.
                    if sync.confirm_frame_convergence(entity, frame, target) {
                        commands
                            .entity(entity)
                            .remove::<WindowFrameCommitSuspended>();
                    } else {
                        commands
                            .entity(entity)
                            .insert(WindowFrameCommitSuspended::new(desired.0));
                    }
                }
                if let Some(mut observed) = observed {
                    if observed.0 != frame {
                        observed.0 = frame;
                    }
                } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_insert(ObservedWindowFrame(frame));
                }
            }
            Err(error) => {
                if let Some(started) = diagnostic_started {
                    debug!(target: "spool::focus_diagnostics", window_id = window.id(),
                        write_us = started.elapsed().as_micros(), %error, "frame_commit_failed");
                }
                warn!(window_id = window.id(), %error, "unable to commit window frame");
                if coordinated {
                    sync.finish_frame_attempt(entity, desired.0, false);
                }
                suspend_failed_frame_commit(entity, &mut window, desired.0, &error, &mut commands);
            }
        }
    }
}

#[cfg(test)]
mod failed_commit_latency_tests {
    use super::*;
    use crate::manager::MockWindowApi;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::World;

    #[test]
    fn unresponsive_ax_commit_does_not_immediately_probe_again() {
        for code in [-25204, -25211] {
            let mut world = World::new();
            let frame = IRect::new(0, 0, 800, 600);
            let entity = world
                .spawn((ObservedWindowFrame(frame), WindowFrameMotion))
                .id();
            world
                .run_system_once(move |mut commands: Commands| {
                    let mut mock = MockWindowApi::new();
                    mock.expect_update_frame().times(0);
                    let mut window = Window::new(Box::new(mock));
                    suspend_failed_frame_commit(
                        entity,
                        &mut window,
                        frame,
                        &crate::errors::Error::macos("frame commit", code),
                        &mut commands,
                    );
                })
                .unwrap();
            assert!(world.get::<WindowFrameMotion>(entity).is_none());
            assert!(world.get::<WindowFrameCommitSuspended>(entity).is_some());
            assert!(world.get::<ObservedWindowFrame>(entity).is_none());
        }
    }

    #[test]
    fn unresponsive_ax_commit_recovers_through_reconciliation() {
        let mut harness = crate::tests::TestHarness::new().with_windows(1);
        harness.pump_frames(15);
        let entity = crate::tests::find_window_entity(0, harness.world());
        let desired = harness.world().get::<DesiredWindowFrame>(entity).unwrap().0;
        harness
            .world()
            .run_system_once(
                move |mut windows: Query<&mut Window>, mut commands: Commands| {
                    let mut window = windows.get_mut(entity).unwrap();
                    suspend_failed_frame_commit(
                        entity,
                        &mut window,
                        desired,
                        &crate::errors::Error::macos("frame commit", -25204),
                        &mut commands,
                    );
                },
            )
            .unwrap();
        assert!(harness.world().get::<ObservedWindowFrame>(entity).is_none());
        harness.pump_frames(30);
        assert!(harness.world().get::<ObservedWindowFrame>(entity).is_some());
        assert!(
            harness
                .world()
                .get::<WindowFrameCommitSuspended>(entity)
                .is_none()
        );
    }
}

#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn verify_window_position(
    mut windows: PositionVerificationWindows,
    mut commands: Commands,
) {
    for (entity, mut window, mut presented, mut verification, mut observed) in &mut windows {
        let confirmed_frame = window.update_frame().ok();
        let positioned = confirmed_frame.is_some_and(|frame| frame.min == presented.0.min);
        // Retry through the normal committer, never bypass presentation or its
        // AX-failure suspension policy with a second geometry writer.
        if !positioned {
            presented.set_changed();
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
        let lua_config: Config = r#"{"swipe":{"gesture":{"fingers_count":3}}}"#
            .try_into()
            .expect("config should parse");
        let tap_config = Config::default();
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

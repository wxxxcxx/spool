use accessibility_sys::kAXErrorNoValue;
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::{ChildOf, Children};
use bevy::ecs::lifecycle::{Add, Remove, RemovedComponents};
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::ecs::observer::On;
use bevy::ecs::query::{Has, With, Without};
use bevy::ecs::system::{Commands, Populated, Query, Res, ResMut, Single, SystemParam};
use bevy::math::IRect;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{Level, debug, error, info, instrument, trace, warn};

use super::{
    ActiveDisplayMarker, BProcess, Floating, FreshMarker, MissionControlActive, PreviousTiledStrip,
    RetileWindow, RetryFrontSwitch, SpawnWindowTrigger, StrayFocusEvent, SystemTheme, Timeout,
    WindowVisibility,
};
use crate::config::Config;
use crate::ecs::focus::{FocusCoordinator, FocusSignal};
use crate::ecs::layout::{LayoutStrip, WidthIntent};
use crate::ecs::params::{GlobalState, WindowCtx, Windows};
use crate::ecs::reconcile::{WindowStateSync, WindowUnavailable};
use crate::ecs::window_frame::{DefaultWindowFrame, WindowFrameCorrection};
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DesiredWindowFrame, DockPosition, FullscreenDefaultsDeferred,
    InitialWindowMarker, Initializing, LayoutPosition, ObservedWindowFrame, Position,
    PresentedWindowFrame, RepositionMarker, ResizeMarker, RetilePending, Scrolling,
    SendMessageTrigger, SpawnCommandsExt, VerifyWindowPosition, WindowDefaultsApplied,
    WindowDefaultsPending, WindowFrameCommitSuspended, WindowFrameMotion, WindowOwnershipChanged,
    WindowProperties,
};
use crate::events::{DestroySource, Event, FocusObservation, FocusSource};
use crate::manager::{
    Application, Display, Origin, Process, Size, Window, WindowManager, WindowPadding,
};
use crate::platform::{WinID, WindowIncarnation, WorkspaceId};
use crate::util::round_px;
use crate::window_policy::LayoutDecision;

/// The display currently in front, paired with the Dock's edge — together they
/// give the usable viewport a window has to be fitted into.
type ActiveDisplayViewport<'w, 's> =
    Single<'w, 's, (&'static Display, Option<&'static DockPosition>), With<ActiveDisplayMarker>>;

type ResizeVerificationWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Window,
        &'static mut Position,
        &'static mut Bounds,
        &'static mut DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
        Option<&'static mut ObservedWindowFrame>,
        Has<Floating>,
    ),
    Without<WindowSpaceReassignmentPending>,
>;

type DefaultableWindows<'w, 's> = Populated<
    'w,
    's,
    (
        Entity,
        &'static mut Window,
        &'static mut Position,
        &'static mut Bounds,
        &'static mut DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
        Option<&'static mut ObservedWindowFrame>,
        &'static ChildOf,
    ),
    (
        With<WindowDefaultsPending>,
        Without<WindowDefaultsApplied>,
        Without<DefaultWindowFrame>,
        Without<FullscreenDefaultsDeferred>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type PositionedWindows<'w, 's> = Populated<
    'w,
    's,
    (Entity, Has<InitialWindowMarker>),
    (
        With<WindowDefaultsPending>,
        With<WindowDefaultsApplied>,
        With<ObservedWindowFrame>,
        Without<FullscreenDefaultsDeferred>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

#[derive(SystemParam)]
pub(super) struct SpawnWindowCtx<'w, 's> {
    windows: Query<'w, 's, (Entity, &'static Window, &'static ChildOf)>,
    apps: Query<'w, 's, (Entity, &'static mut Application)>,
    launch_capture: crate::ecs::exit_restore::LaunchCapture<'w, 's>,
    initializing: Option<Res<'w, Initializing>>,
    sync: ResMut<'w, WindowStateSync>,
    config: Res<'w, Config>,
    window_manager: Res<'w, WindowManager>,
    topology: Res<'w, super::topology::NativeTopology>,
    targets: Query<'w, 's, &'static DesiredWindowFrame>,
    representation_blocked: Query<'w, 's, (), RepresentationBlocked>,
    commands: Commands<'w, 's>,
}

/// Computes the passthrough keybinding set for the given window/app and
/// publishes it to the input thread. Called on focus change and config reload.
fn update_passthrough(window: &Window, app: &Application, config: &Config) {
    let properties = WindowProperties::new(app, window, config);
    crate::platform::input::set_focused_passthrough(properties.passthrough_keys());
}

fn focus_query_failure_level(error: &crate::errors::Error) -> Level {
    if error.macos_code() == Some(kAXErrorNoValue) {
        Level::DEBUG
    } else {
        Level::WARN
    }
}

fn log_focus_query_failure(error: &crate::errors::Error) {
    if focus_query_failure_level(error) == Level::DEBUG {
        debug!("focused window is not available yet: {error}");
    } else {
        warn!("can not get current focus: {error}");
    }
}

/// Re-applies the configuration side effects that must follow any config change:
/// the per-display menubar-height override and the focused window's passthrough
/// keys after a successful Lua reload.
#[cfg(feature = "lua")]
pub(crate) fn apply_config_side_effects(
    config: &Config,
    displays: &mut Query<&mut Display>,
    windows: &Windows,
    applications: &Query<&Application>,
) {
    let height = config.menubar_height();
    for mut display in &mut *displays {
        display.set_menubar_height_override(height);
    }

    // Recompute passthrough keys for the currently focused window.
    if let Some((_, entity)) = windows.focused()
        && let Some((window, _, parent)) = windows.get_parent(entity)
        && let Ok(app) = applications.get(parent)
    {
        update_passthrough(window, app, config);
    }
}

/// Handles the event when an application switches to the front. It updates the focused window and PSN.
///
/// # Arguments
///
/// * `trigger` - The Bevy event trigger containing the application front switched event.
/// * `processes` - A query for all processes with their children.
/// * `applications` - A query for all applications.
/// * `focused_window` - A query for the focused window.
/// * `focus_follows_mouse_id` - The resource to track focus follows mouse window ID.
/// * `commands` - Bevy commands to trigger events and manage components.
pub(super) fn front_switched_trigger(
    mut messages: MessageReader<Event>,
    processes: Query<(&BProcess, &Children)>,
    applications: Query<(Entity, &Application)>,
    window_manager: Res<WindowManager>,
    mut focus: ResMut<FocusCoordinator>,
    mut config: GlobalState,
    mut commands: Commands,
) {
    const FRONT_SWITCH_RETRY_SEC: u64 = 2;
    for event in messages.read() {
        let (app_entity, app, app_name, source, front_switched) = match event {
            Event::ApplicationFrontSwitched { psn } => {
                let Some((BProcess(process), children)) =
                    processes.iter().find(|process| &process.0.psn() == psn)
                else {
                    debug!("Unable to find process with PSN {psn:?}");
                    continue;
                };

                if children.len() > 1 {
                    warn!("Multiple apps registered to process '{}'.", process.name());
                }
                let Some(&app_entity) = children.first() else {
                    error!("No application for process '{}'.", process.name());
                    continue;
                };
                let Ok((_, app)) = applications.get(app_entity) else {
                    error!("No application for process '{}'.", process.name());
                    continue;
                };
                (
                    app_entity,
                    app,
                    process.name().to_string(),
                    FocusSource::ApplicationFrontSwitch,
                    true,
                )
            }
            Event::FocusRevalidationRequested { pid, source } => {
                let mut uncertain = None;
                let mut current = None;
                for candidate @ (_, app) in applications.iter().filter(|(_, app)| app.pid() == *pid)
                {
                    match app.is_running() {
                        Ok(true) => {
                            current = Some(candidate);
                            break;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            debug!(pid, %error, "focus revalidation liveness check failed open");
                            uncertain.get_or_insert(candidate);
                        }
                    }
                }
                let Some((app_entity, app)) = current.or(uncertain) else {
                    debug!("Unable to revalidate focus for pid {pid} from {source:?}");
                    continue;
                };
                (app_entity, app, app.name().to_string(), *source, false)
            }
            _ => continue,
        };

        // AX focused UI elements are application-local. Background tab changes
        // must not invalidate global focus and retrigger tiled layer raises.
        if !front_switched && !app.is_frontmost() {
            continue;
        }
        debug!("resolving focused window for application: {app_name}");
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: Some(app.pid()),
                candidate: None,
            })
            .generation()
            .expect("starting focus resolution returns a generation");

        if let Ok(focused_id) = app.focused_window_id().inspect_err(|err| {
            log_focus_query_failure(err);
        }) {
            if front_switched
                && let Some(point) = window_manager.cursor_position()
                && window_manager
                    .find_window_at_point(&point)
                    .is_ok_and(|window_id| window_id != focused_id)
            {
                // Window got focus without mouse movement - probably with a Cmd-Tab.
                // If so, bring it into view.
                config.set_skip_reshuffle(false);
                config.set_ffm_flag(None);
            }
            commands.trigger(SendMessageTrigger(Event::resolved_focus(
                focused_id,
                app.pid(),
                source,
                generation,
            )));
        } else {
            // Transient AX error (e.g. kAXErrorCannotComplete during app transitions).
            // Schedule a retry to query the focused window once the app is ready.
            commands.spawn(RetryFrontSwitch::new(
                app_entity,
                generation,
                Duration::from_secs(FRONT_SWITCH_RETRY_SEC),
            ));
        }
    }
}

pub(super) fn theme_change_trigger(
    mut messages: MessageReader<Event>,
    windows: Windows,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut theme: Option<ResMut<SystemTheme>>,
) {
    for event in messages.read() {
        let Event::ThemeChanged = event else {
            continue;
        };

        let Some(ref mut theme) = theme else {
            continue;
        };

        let is_dark = crate::util::is_dark_mode();
        if theme.is_dark == is_dark {
            continue;
        }
        theme.is_dark = is_dark;
        info!("System theme changed: dark_mode={is_dark}");

        let Some(dim_ratio) = config.window_dim_ratio(is_dark) else {
            continue;
        };

        // Re-apply dimming to all windows that are NOT focused.
        let focused_id = windows.focused().map(|(window, _)| window.id());
        let windows_to_dim: Vec<WinID> = windows
            .iter()
            .filter(|(window, _)| Some(window.id()) != focused_id)
            .map(|(window, _)| window.id())
            .collect();

        if !windows_to_dim.is_empty() {
            window_manager.dim_windows(&windows_to_dim, dim_ratio);
        }
    }
}

/// Handles the event when a window gains focus. It updates the focused window, PSN, and reshuffles windows.
/// It also centers the mouse on the focused window if focus-follows-mouse is enabled.
///
/// # Arguments
///
/// * `messages` - The event stream carrying the window focused event.
/// * `applications` - A query for all applications.
/// * `workspaces` - A query for the layout strips, to reorder the focused column.
/// * `focus` - Confirmed, requested, and per-Space navigation focus state.
/// * `global_state` - Focus-follows-mouse and reshuffle flags.
/// * `ctx` - Window queries, configuration and the command buffer.
fn confirm_tracked_focus_observation(
    observation: FocusObservation,
    entity: Entity,
    app: &Application,
    focus: &mut FocusCoordinator,
) -> bool {
    if !app.is_frontmost() {
        return false;
    }
    let Ok(focused_window_id) = app.focused_window_id().inspect_err(|error| {
        debug!(
            window_id = observation.window_id,
            %error,
            "focus observation deferred because AX focus is unavailable"
        );
    }) else {
        return false;
    };
    if focused_window_id != observation.window_id {
        return false;
    }
    let Some(generation) = focus
        .observe(FocusSignal::Candidate {
            generation: observation.generation,
            pid: observation.pid,
            window_id: observation.window_id,
        })
        .generation()
    else {
        return false;
    };
    focus
        .observe(FocusSignal::Tracked {
            generation,
            entity,
            window_id: observation.window_id,
        })
        .accepted()
}

fn queue_untracked_focus_observation(
    observation: FocusObservation,
    focus: &mut FocusCoordinator,
    commands: &mut Commands,
) {
    const RETRY_SEC: u64 = 2;
    let Some(generation) = focus
        .observe(FocusSignal::Candidate {
            generation: observation.generation,
            pid: observation.pid,
            window_id: observation.window_id,
        })
        .generation()
    else {
        return;
    };
    focus.observe(FocusSignal::Untracked {
        generation: Some(generation),
        pid: observation.pid,
        window_id: Some(observation.window_id),
    });
    let timeout = Timeout::new(Duration::from_secs(RETRY_SEC), None, commands);
    commands.spawn((
        timeout,
        StrayFocusEvent(FocusObservation {
            generation: Some(generation),
            ..observation
        }),
    ));
}

#[instrument(level = Level::DEBUG, skip_all)]
#[allow(
    clippy::too_many_lines,
    reason = "focus evidence and workspace projection are updated in one ordered observer"
)]
pub(super) fn window_focused_trigger(
    mut messages: MessageReader<Event>,
    applications: Query<&Application>,
    mut workspaces: Query<(Entity, &mut LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    mut focus: ResMut<FocusCoordinator>,
    global_state: GlobalState,
    mut ctx: WindowCtx,
) {
    for event in messages.read() {
        let Event::WindowFocused(observation) = *event else {
            continue;
        };
        let window_id = observation.window_id;
        if observation
            .generation
            .is_some_and(|generation| !focus.is_current(generation))
        {
            debug!(
                "Discarding stale focus observation for window {window_id} from {:?}.",
                observation.source
            );
            continue;
        }

        let Some((window, entity, parent)) =
            ctx.windows
                .find_parent_matching(window_id, |window, parent| {
                    applications.get(parent).is_ok_and(|app| {
                        observation.pid.is_none_or(|pid| app.pid() == pid)
                            && observation
                                .incarnation
                                .is_none_or(|incarnation| window.incarnation() == incarnation)
                            && app.is_frontmost()
                            && app.owns_window(window).is_ok_and(|owned| owned)
                    })
                })
        else {
            queue_untracked_focus_observation(observation, &mut focus, &mut ctx.commands);
            continue;
        };

        let Ok(app) = applications.get(parent) else {
            warn!("Unable to get parent for window {}.", window.id());
            continue;
        };

        let already_focused = ctx
            .windows
            .focused()
            .is_some_and(|(_, focused_entity)| focused_entity == entity);
        let confirms_request = focus.snapshot().requested_entity() == Some(entity);

        // Delayed cross-app and same-app events must not overwrite a newer
        // focus observation or its passthrough configuration.
        if !confirm_tracked_focus_observation(observation, entity, app, &mut focus) {
            continue;
        }

        // Always keep passthrough in sync. An internal focus_entity call races
        // with the OS WindowFocused event; without this the passthrough keys
        // remain stale from a previously focused window.
        update_passthrough(window, app, &ctx.config);

        let Some((_, _, window_state)) = ctx.windows.get_tracked(entity) else {
            continue;
        };
        if matches!(window_state.visibility(), Some(WindowVisibility::Hidden)) {
            if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
                entity_commands.try_remove::<WindowVisibility>();
            }
            ctx.commands
                .trigger(SendMessageTrigger(Event::window_focused(window_id)));
            continue;
        }

        // Handle tab switching: if the focused window is a tab, make it the leader.
        // Reactivate the owning Space before treating duplicate focus
        // as a no-op; the focus marker can lag a system Space transition.
        // Track the active workspace as a fallback so the coordinator can
        // retain a navigation anchor before the entity is routed into a strip.
        let mut owner = None;
        let mut owning_workspace_id = None;
        let mut active_workspace_id = None;
        for (strip_entity, mut strip, active) in &mut workspaces {
            if active {
                active_workspace_id = Some(strip.id());
            }
            if owner.is_none() && strip.contains(entity) {
                if let Ok(index) = strip.index_of(entity)
                    && strip
                        .bypass_change_detection()
                        .edit_column(index, |column| column.move_to_front(entity))
                {
                    strip.set_changed();
                }
                owning_workspace_id = Some(strip.id());
                owner = Some((strip_entity, active));
            }
        }

        if owner.is_none() && window_state.is_tiled() {
            // The window just spawned and has not yet been inserted into the strip.
            continue;
        }

        if let Some((strip_entity, active)) = owner
            && !active
            && let Ok(mut entity_commands) = ctx.commands.get_entity(strip_entity)
        {
            entity_commands.try_insert(ActiveWorkspaceMarker);
        }

        // Record before the already-focused short-circuit below so tier and
        // per-Space navigation history also sees duplicate confirmations.
        if let Some(workspace_id) = owning_workspace_id.or(active_workspace_id)
            && let Some((_, _, state)) = ctx.windows.get_tracked(entity)
        {
            focus.record_navigation(
                workspace_id,
                entity,
                state.is_floating(),
                state.is_visible(),
            );
        }

        if already_focused {
            if !confirms_request && !global_state.skip_reshuffle() && !global_state.initializing() {
                ctx.commands.reshuffle_around(entity);
            }
            continue;
        }

        debug!("window {} ({entity}) focused.", window.id());
    }
}

/// Handles Mission Control events, updating the `MissionControlActive` resource.
///
/// # Arguments
///
/// * `trigger` - The Bevy event trigger containing the Mission Control event.
/// * `mission_control_active` - The `MissionControlActive` resource.
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn mission_control_trigger(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut workspaces: Query<(
        Entity,
        &mut LayoutStrip,
        Has<ActiveWorkspaceMarker>,
        Option<&Scrolling>,
    )>,
    mut mission_control_active: ResMut<MissionControlActive>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    for event in messages.read() {
        match event {
            Event::MissionControlShowAllWindows
            | Event::MissionControlShowFrontWindows
            | Event::MissionControlShowDesktop => {
                mission_control_active.as_mut().0 = true;
                for (entity, _, _, scroll) in &workspaces {
                    if scroll.is_some()
                        && let Ok(mut entity_commands) = commands.get_entity(entity)
                    {
                        entity_commands.try_remove::<Scrolling>();
                    }
                }
            }
            Event::MissionControlExit => {
                mission_control_active.as_mut().0 = false;

                // Check if some windows disappeared from the current workspace
                // - e.g. they were moved away during mission control.
                if let Some(mut active_strip) = workspaces
                    .iter_mut()
                    .find_map(|(_, strip, active, _)| active.then_some(strip))
                    && let Ok(present_windows) =
                        window_manager.windows_in_workspace(active_strip.id())
                {
                    let moved_windows = active_strip
                        .all_windows()
                        .into_iter()
                        .filter_map(|entity| windows.get(entity).zip(Some(entity)))
                        .filter(|(window, _)| !present_windows.contains(&window.id()));
                    for (window, entity) in moved_windows {
                        debug!(
                            "window {} {entity} moved, removing from workspace {}",
                            window.id(),
                            active_strip.id(),
                        );
                        // Simply removing them from the current strip is enough,
                        // they will be re-detected during the workspace change.
                        active_strip.remove(entity);
                    }
                }
            }
            _ => (),
        }
    }
}

/// Dispatches process-related messages, such as application launch and termination.
///
/// # Arguments
///
/// * `trigger` - The Bevy event trigger containing the application event.
/// * `processes` - A query for all processes.
/// * `commands` - Bevy commands to spawn or despawn entities.
pub(super) fn application_event_trigger(
    mut messages: MessageReader<Event>,
    processes: Query<(&BProcess, Entity)>,
    mut commands: Commands,
) {
    const PROCESS_READY_TIMEOUT_SEC: u64 = 5;
    let find_process = |psn| {
        processes
            .iter()
            .find(|(BProcess(process), _)| process.psn() == psn)
    };

    for event in messages.read() {
        match event {
            Event::ApplicationLaunched { psn, observer } if find_process(*psn).is_none() => {
                let process: BProcess = Process::new(psn, observer.clone()).into();
                if process.pid() == 0 {
                    debug!("Skipping process with PID 0 (likely kernel_task).");
                    continue;
                }
                if crate::ecs::is_own_process(process.pid()) {
                    debug!("Skipping Spool's own process in the window lifecycle");
                    continue;
                }
                let timeout = Timeout::new(
                    Duration::from_secs(PROCESS_READY_TIMEOUT_SEC),
                    Some(format!(
                        "Process '{}' did not become ready in {PROCESS_READY_TIMEOUT_SEC}s.",
                        process.name()
                    )),
                    &mut commands,
                );
                commands.spawn((FreshMarker, timeout, process));
            }

            Event::ApplicationTerminated { psn } => {
                if let Some((_, entity)) = find_process(*psn)
                    && let Ok(mut entity_commands) = commands.get_entity(entity)
                {
                    entity_commands.try_despawn();
                }
            }
            _ => (),
        }
    }
}

/// Dispatches application-related messages, such as window creation, destruction, and resizing.
///
/// # Arguments
///
/// * `trigger` - The Bevy event trigger containing the window event.
/// * `windows` - A query for all windows.
/// * `displays` - A query for the active display.
/// * `main_cid` - The main connection ID resource.
/// * `commands` - Bevy commands to spawn or despawn entities.
pub(super) fn dispatch_application_messages(
    mut messages: MessageReader<Event>,
    windows: Windows,
    applications: Query<(&Application, &Children)>,
    visibility_query: Query<&WindowVisibility>,
    mut commands: Commands,
) {
    let find_window = |window_id, incarnation| match incarnation {
        Some(incarnation) => windows.find_incarnation(window_id, incarnation),
        None => windows.find(window_id),
    };

    for event in messages.read() {
        match event {
            Event::WindowMinimized {
                window_id,
                incarnation,
            } => {
                if let Some((_, entity)) = find_window(*window_id, *incarnation)
                    && let Ok(mut entity_commands) = commands.get_entity(entity)
                {
                    entity_commands.try_insert(WindowVisibility::Minimized);
                }
            }

            Event::WindowDeminimized {
                window_id,
                incarnation,
            } => {
                if let Some((_, entity)) = find_window(*window_id, *incarnation)
                    && matches!(
                        visibility_query.get(entity),
                        Ok(WindowVisibility::Minimized)
                    )
                    && let Ok(mut entity_commands) = commands.get_entity(entity)
                {
                    entity_commands.try_remove::<WindowVisibility>();
                }
            }

            Event::ApplicationHidden { pid } => {
                let mut found = false;
                for (app, children) in applications.iter().filter(|(app, _)| app.pid() == *pid) {
                    match app.is_running() {
                        Ok(false) => continue,
                        Ok(true) => {}
                        Err(error) => {
                            debug!(pid, %error, "application hide liveness check failed open");
                        }
                    }
                    found = true;
                    for entity in children {
                        // Preserve a minimized state; layout mode is independent.
                        if visibility_query.get(*entity).is_err()
                            && let Ok(mut entity_commands) = commands.get_entity(*entity)
                        {
                            entity_commands.try_insert(WindowVisibility::Hidden);
                        }
                    }
                }
                if !found {
                    warn!("Unable to find with pid {pid}");
                }
            }

            Event::ApplicationVisible { pid } => {
                let mut found = false;
                for (app, children) in applications.iter().filter(|(app, _)| app.pid() == *pid) {
                    match app.is_running() {
                        Ok(false) => continue,
                        Ok(true) => {}
                        Err(error) => {
                            debug!(pid, %error, "application show liveness check failed open");
                        }
                    }
                    found = true;
                    for entity in children {
                        // Only restore windows that were hidden by the app hide/show cycle.
                        // Preserve layout mode and minimized state.
                        if matches!(visibility_query.get(*entity), Ok(WindowVisibility::Hidden))
                            && let Ok(mut entity_commands) = commands.get_entity(*entity)
                        {
                            entity_commands.try_remove::<WindowVisibility>();
                        }
                    }
                }
                if !found {
                    warn!("Unable to find application with pid {pid}");
                }
            }
            _ => (),
        }
    }
}

fn clamp_origin_to_bounds(origin: IRect, size: Size, bounds: IRect) -> IRect {
    let max = (bounds.max - size).max(bounds.min);
    let min = origin.min.clamp(bounds.min, max);
    IRect::from_corners(min, min + size)
}

type PendingFloating<'w, 's> = Query<
    'w,
    's,
    Has<RetilePending>,
    bevy::ecs::query::Or<(With<WindowDefaultsPending>, With<RetilePending>)>,
>;

#[derive(SystemParam)]
pub(super) struct FloatingTransactions<'w, 's> {
    pending: PendingFloating<'w, 's>,
}

impl FloatingTransactions<'_, '_> {
    fn handles_transition(&self, entity: Entity) -> bool {
        // Initial placement and retile own geometry; neither can race a
        // second cosmetic active-display grid or pop-out resize here.
        self.pending.get(entity).is_ok()
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn window_floating_trigger(
    trigger: On<Add, Floating>,
    pending_defaults: FloatingTransactions,
    apps: Query<(Entity, &Application)>,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    // `Option<Single>` rather than `Single`: an unresolvable display would skip the
    // whole observer, but the strip removal below must still run without one.
    active_display: Option<ActiveDisplayViewport>,
    initializing: Option<Res<Initializing>>,
    mut ctx: WindowCtx,
) {
    const FLOATING_MAX_SCREEN_RATIO_NUM: i32 = 4;
    const FLOATING_MAX_SCREEN_RATIO_DEN: i32 = 5;
    const FLOATING_POP_OFFSET: i32 = 32;

    fn offset_frame_within_bounds(frame: IRect, bounds: IRect, offset: i32) -> IRect {
        let candidates = [
            (offset, offset),
            (offset, -offset),
            (-offset, offset),
            (-offset, -offset),
            (offset, 0),
            (-offset, 0),
            (0, offset),
            (0, -offset),
        ];

        for (dx, dy) in candidates {
            let moved = IRect::from_corners(
                Origin::new(frame.min.x + dx, frame.min.y + dy),
                Origin::new(frame.max.x + dx, frame.max.y + dy),
            );
            if moved.min.x >= bounds.min.x
                && moved.max.x <= bounds.max.x
                && moved.min.y >= bounds.min.y
                && moved.max.y <= bounds.max.y
            {
                return moved;
            }
        }

        frame
    }

    let entity = trigger.event().entity;
    let Some((_, _, state)) = ctx.windows.get_tracked(entity) else {
        return;
    };
    if !state.is_floating() {
        return;
    }

    debug!("Entity {entity} is floating.");

    // A window on a virtual row that isn't on screen is off-screen by design;
    // popping it onto the active display would paint it over the row the user
    // is actually looking at.
    let parked_out_of_view = workspaces
        .iter()
        .any(|(strip, active)| !active && strip.contains(entity));

    // Drop the strip membership first, before anything below can bail early —
    // a floating window still reserves column space in the strip otherwise,
    // leaving a gap that never closes on its own.
    for (mut strip, _) in &mut workspaces {
        if strip.contains(entity) {
            strip.remove(entity);
        }
    }

    if pending_defaults.handles_transition(entity) {
        return;
    }

    let Some((display, dock)) = active_display.map(|display| *display) else {
        return;
    };
    let display_bounds = display.actual_display_bounds(dock, &ctx.config);

    let Some((window, _, parent)) = ctx.windows.get_parent(entity) else {
        return;
    };
    // Capability fallback must not run the cosmetic "pop out" resize or a grid
    // write that the window cannot support (including a failed retile request).
    if window.layout_decision(false) != LayoutDecision::Tile {
        return;
    }
    let Some(frame) = ctx.windows.frame(entity) else {
        return;
    };
    let Ok((_, app)) = apps.get(parent) else {
        return;
    };

    let properties = WindowProperties::new(app, window, &ctx.config);

    // Skip the active-display reposition/resize during init; the strip
    // removal below still has to run.
    if parked_out_of_view {
        debug!("Entity {entity} is floating on a hidden virtual row, keeping its frame.");
    } else if let Some((rx, ry, rw, rh)) = properties.grid_ratios() {
        let x = display_bounds.min.x + round_px(f64::from(display_bounds.width()) * rx);
        let y = display_bounds.min.y + round_px(f64::from(display_bounds.height()) * ry);
        let w = round_px(f64::from(display_bounds.width()) * rw);
        let h = round_px(f64::from(display_bounds.height()) * rh);
        ctx.commands.reposition_entity(entity, Origin::new(x, y));
        ctx.commands.resize_entity(entity, Size::new(w, h));
    } else if initializing.is_none() && !properties.floating() {
        let max_width =
            display_bounds.width() * FLOATING_MAX_SCREEN_RATIO_NUM / FLOATING_MAX_SCREEN_RATIO_DEN;
        let max_height =
            display_bounds.height() * FLOATING_MAX_SCREEN_RATIO_NUM / FLOATING_MAX_SCREEN_RATIO_DEN;
        let new_width = frame.width().min(max_width);
        let new_height = frame.height().min(max_height);

        let mut target_frame =
            IRect::from_corners(frame.min, frame.min + Origin::new(new_width, new_height));
        target_frame = clamp_origin_to_bounds(target_frame, target_frame.size(), display_bounds);
        target_frame =
            offset_frame_within_bounds(target_frame, display_bounds, FLOATING_POP_OFFSET);

        if target_frame.size() != frame.size() {
            ctx.commands.resize_entity(
                entity,
                Size::new(target_frame.width(), target_frame.height()),
            );
        }
        if target_frame.min != frame.min {
            ctx.commands.reposition_entity(entity, target_frame.min);
        }
    }
}

fn remember_tiled_strip(entity: Entity, strip: &LayoutStrip, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(PreviousTiledStrip {
            workspace_id: strip.id(),
            index: strip.index_of(entity).unwrap_or(strip.len()),
        });
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn window_visibility_trigger(
    trigger: On<Add, WindowVisibility>,
    windows: Windows,
    workspaces: Query<&mut LayoutStrip>,
    mut focus: ResMut<FocusCoordinator>,
    mut commands: Commands,
) {
    let entity = trigger.event().entity;
    if windows.get_tracked(entity).is_some() {
        debug!("Entity {entity} is minimized or hidden.");
        focus.observe(FocusSignal::Invalidated { entity });

        for mut strip in workspaces {
            if strip.contains(entity) {
                remember_tiled_strip(entity, &strip, &mut commands);
                strip.remove(entity);
            }
        }
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn window_floating_removed_trigger(
    trigger: On<Remove, Floating>,
    mut commands: Commands,
) {
    commands.trigger(RetileWindow(trigger.event().entity));
}

pub(super) fn window_visibility_removed_trigger(
    trigger: On<Remove, WindowVisibility>,
    windows: Query<Has<Floating>, With<Window>>,
    mut commands: Commands,
) {
    if windows
        .get(trigger.event().entity)
        .is_ok_and(|floating| !floating)
    {
        commands.trigger(RetileWindow(trigger.event().entity));
    }
}

#[derive(SystemParam)]
pub(super) struct RetileState<'w, 's> {
    previous_strips: Query<'w, 's, &'static PreviousTiledStrip>,
    pending_reassignments: Query<'w, 's, (), With<WindowSpaceReassignmentPending>>,
    pending_defaults: Query<'w, 's, (), With<WindowDefaultsPending>>,
    displays: Query<'w, 's, (&'static Display, Option<&'static DockPosition>)>,
    topology: Res<'w, super::topology::NativeTopology>,
    window_manager: Res<'w, WindowManager>,
    time: Res<'w, bevy::time::Time>,
}

impl RetileState<'_, '_> {
    fn destination(&self, window_id: WinID, config: &Config) -> Option<(WorkspaceId, IRect)> {
        let workspace_id = self
            .topology
            .observe_memberships(&self.window_manager)
            .ok()?
            .unique_space(window_id)?;
        if self.topology.is_fullscreen(workspace_id) {
            return None;
        }
        let owner = self
            .topology
            .known_displays()
            .find_map(|(display, spaces)| spaces.contains(&workspace_id).then_some(display.id()))?;
        let (display, dock) = self
            .displays
            .iter()
            .find(|(display, _)| display.id() == owner)?;
        Some((workspace_id, display.actual_display_bounds(dock, config)))
    }
}

type PendingRetiles<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static RetilePending),
    (
        With<Window>,
        Without<WindowDefaultsPending>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
        Without<WindowVisibility>,
    ),
>;

type RetileWorkspaces<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut LayoutStrip,
        &'static mut Position,
        Has<ActiveWorkspaceMarker>,
    ),
    (Without<Window>, Without<PendingSpaceDestruction>),
>;

pub(super) fn retry_pending_retiles(
    pending: PendingRetiles,
    time: Res<bevy::time::Time>,
    mut commands: Commands,
) {
    for (entity, retry) in &pending {
        if time.elapsed() >= retry.0 {
            commands.trigger(RetileWindow(entity));
        }
    }
}

fn prepare_retile(
    entity: Entity,
    destination_ready: bool,
    state: &RetileState,
    ctx: &mut WindowCtx,
) -> bool {
    let RetileState {
        previous_strips,
        pending_reassignments,
        pending_defaults,
        time,
        ..
    } = state;
    // Initial placement owns insertion. A window returning from visibility or
    // native fullscreen can already have a remembered route while defaults
    // remain deferred; preserve that route and still check capabilities below.
    if pending_defaults.contains(entity) && previous_strips.get(entity).is_err() {
        return false;
    }
    if pending_reassignments.get(entity).is_ok() {
        debug!(
            ?entity,
            "deferring retile until native Space reassignment settles"
        );
        if ctx
            .windows
            .get_tracked(entity)
            .is_some_and(|(_, _, state)| state.is_floating())
        {
            ctx.commands.entity(entity).insert(RetilePending(
                time.elapsed() + std::time::Duration::from_secs(5),
            ));
        }
        return false;
    }

    let Some(window) = ctx.windows.get(entity) else {
        return false;
    };
    let decision = if !destination_ready
        || window.role().is_err()
        || !matches!(window.try_is_full_screen(), Ok(false))
    {
        LayoutDecision::Defer
    } else {
        window.layout_decision(false)
    };
    match decision {
        LayoutDecision::Defer => {
            ctx.commands.entity(entity).insert((
                Floating,
                RetilePending(time.elapsed() + std::time::Duration::from_secs(5)),
            ));
            return false;
        }
        LayoutDecision::Float(reason) => {
            debug!(?entity, ?reason, "refusing unsupported retile");
            ctx.commands
                .entity(entity)
                .remove::<RetilePending>()
                .insert(Floating);
            return false;
        }
        LayoutDecision::Tile => {}
    }
    ctx.commands.entity(entity).remove::<RetilePending>();
    // Removing Floating invokes this observer too. Let that invocation own the
    // insertion so a deferred request cannot insert twice.
    if ctx
        .windows
        .get_tracked(entity)
        .is_some_and(|(_, _, state)| state.is_floating())
    {
        ctx.commands.entity(entity).remove::<Floating>();
        return false;
    }
    true
}

pub(super) fn retile_window_trigger(
    trigger: On<RetileWindow>,
    apps: Query<(Entity, &Application)>,
    mut workspaces: RetileWorkspaces,
    state: RetileState,
    initializing: Option<Res<Initializing>>,
    mut ctx: WindowCtx,
) {
    let entity = trigger.event().0;
    // finish_setup owns the initial strip assignment.
    if initializing.is_some() {
        return;
    }
    // A retry may outlive a Space switch; only current native membership owns
    // the destination, including when a remembered tiled route is stale.
    let destination = ctx
        .windows
        .get(entity)
        .and_then(|window| state.destination(window.id(), &ctx.config))
        .filter(|(space, _)| {
            workspaces
                .iter()
                .any(|(_, strip, _, _)| strip.id() == *space)
        });
    if !prepare_retile(entity, destination.is_some(), &state, &mut ctx) {
        return;
    }
    let Some((workspace_id, _display_bounds)) = destination else {
        return;
    };
    let previous_strips = &state.previous_strips;

    debug!("Entity {entity} is tiled again.");
    let previous = previous_strips
        .get(entity)
        .ok()
        .filter(|previous| previous.workspace_id == workspace_id);
    let mut insert_at = previous.map(|previous| previous.index);

    if let Some((window, _, parent)) = ctx.windows.get_parent(entity)
        && let Ok((_, app)) = apps.get(parent)
    {
        let properties = WindowProperties::new(app, window, &ctx.config);

        insert_at = properties.insertion().or(insert_at);
    }

    for (_, mut strip, _, _) in &mut workspaces {
        strip.remove(entity);
    }

    // The strip the window ended up in, and whether that strip is the one
    // currently on screen.
    let mut landed_in = None;
    if let Some((strip_entity, mut target_strip, _, active)) = workspaces
        .iter_mut()
        .find(|(_, strip, _, _)| strip.id() == workspace_id)
    {
        landed_in = Some((strip_entity, active));
        if let Some(index) = insert_at {
            target_strip.insert_at(index, entity);
        } else {
            // Insert at the column the floating window visually overlaps so the
            // strip doesn't have to scroll to the end to expose the new column.
            let insertion = ctx.windows.frame(entity).and_then(|frame| {
                let center_x = frame.center().x;
                target_strip.all_columns().into_iter().position(|top| {
                    ctx.windows
                        .frame(top)
                        .is_some_and(|col| col.center().x > center_x)
                })
            });
            let insertion = insertion.unwrap_or(target_strip.len());
            target_strip.insert_at(insertion, entity);
        }
    }

    if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
        entity_commands.try_remove::<PreviousTiledStrip>();
    }

    if let Some((strip_entity, false)) = landed_in {
        // This strip isn't on screen, so the window's current frame (possibly
        // popped onto the active display while floating) must not be kept.
        // Marking the strip's position changed forces the layout to re-derive
        // this window's frame from the strip's off-screen origin, instead of
        // painting it over the active row while it still belongs to the hidden one.
        if let Ok((_, _, mut position, _)) = workspaces.get_mut(strip_entity) {
            position.set_changed();
        }
        return;
    }

    if let Some(origin) = ctx.windows.origin(entity) {
        ctx.commands.reposition_entity(entity, origin);
    }
    if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
        entity_commands.try_insert(VerifyWindowPosition::default());
    }
    ctx.commands.reshuffle_around(entity);
}

/// Handles the event when a window is destroyed. The windows itself is not removed from the layout
/// strip. This happens in the On<Remove, Window> trigger.
///
/// # Arguments
///
/// * `messages` - The event stream carrying the ID of the destroyed window.
/// * `active_display` - The active display and its layout strip.
/// * `apps` - A query for all applications.
/// * `global_state` - Focus-follows-mouse and reshuffle flags.
/// * `focus` - Confirmed, requested, and per-Space navigation focus state.
/// * `windows` - A query for all windows with their parent.
/// * `commands` - Bevy commands to despawn entities and trigger events.
#[instrument(level = Level::DEBUG, skip_all)]
pub(super) fn window_destroyed_trigger(
    mut messages: MessageReader<Event>,
    mut apps: Query<&mut Application>,
    mut focus: ResMut<FocusCoordinator>,
    mut sync: ResMut<WindowStateSync>,
    windows: Windows,
    mut commands: Commands,
) {
    for event in messages.read() {
        let Event::WindowDestroyed {
            window_id,
            source,
            incarnation,
        } = event
        else {
            continue;
        };

        let Some(incarnation) = incarnation else {
            // SLS and WindowServer notifications only carry a recyclable
            // integer ID. The inventory reconciler resolves those against AX
            // and process ownership instead of risking deletion of a new use
            // of the same ID.
            continue;
        };
        let Some((window, entity, parent)) =
            windows.find_parent_incarnation_any(*window_id, *incarnation)
        else {
            debug!("Duplicate event: window {window_id} already destroyed.");
            continue;
        };

        // Only the SLS notification is ambiguous — it also fires when a window merely leaves a
        // space, so it needs confirming. A `kAXUIElementDestroyedNotification` means the AX element
        // itself has been torn down and is taken at face value: confirming it against the app is
        // not just unnecessary but actively wrong, because both signals below lag the teardown.
        // `role()` keeps succeeding on the dead element for apps that outlive their windows, and
        // the app's AX window list is still warm for a moment after the close. Re-checking them
        // raced the window back to life, leaving the entity in the strip and a permanent gap where
        // the window had been.
        if matches!(source, DestroySource::SpaceNotification) && window.role().is_ok() {
            debug!(
                "Window {} still present, this was SLS workspace change.",
                window.id()
            );
            continue;
        }

        let Ok(mut app) = apps.get_mut(parent) else {
            error!("Window {} has no parent!", window.id());
            continue;
        };

        app.unobserve_window(window);
        sync.retire_window(parent, window);
        sync.forget_window(entity);

        focus.observe(FocusSignal::Invalidated { entity });
        focus.forget(entity);

        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_despawn();
        }

        // The window entity will be removed from the layout strip in the On<Remove> trigger.
    }
}

/// Drops a window's cached title when its app reports the title changed.
///
/// Must run before the broadcast handler reads titles in `PostUpdate`, so a
/// subscriber sees the new title in the same frame it changed.
pub(super) fn invalidate_window_title(mut messages: MessageReader<Event>, windows: Windows) {
    for event in messages.read() {
        let Event::WindowTitleChanged {
            window_id,
            incarnation,
        } = event
        else {
            continue;
        };
        let window = match incarnation {
            Some(incarnation) => windows.find_incarnation(*window_id, *incarnation),
            None => windows.find(*window_id),
        };
        if let Some((window, _)) = window {
            window.invalidate_title();
        }
    }
}

fn transfer_window_ownership(
    entity: Entity,
    application: Entity,
    window: Window,
    frame: IRect,
    ctx: &mut SpawnWindowCtx,
) {
    let window_id = window.id();
    let Ok((_, mut app)) = ctx.apps.get_mut(application) else {
        return;
    };
    match app.observe_window(&window) {
        Ok(true) => {}
        Ok(false) => debug!(window_id, "some window observers need retry"),
        Err(error) => warn!(window_id, %error, "unable to register window observers"),
    }
    ctx.sync.forget_window(entity);
    if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
        entity_commands.try_insert((
            window,
            ChildOf(application),
            Position(frame.min),
            Bounds(frame.size()),
            DesiredWindowFrame(frame),
            PresentedWindowFrame(frame),
            ObservedWindowFrame(frame),
            WindowOwnershipChanged,
            WindowDefaultsPending,
        ));
        entity_commands.remove::<(
            PreviousTiledStrip,
            WindowUnavailable,
            RepositionMarker,
            ResizeMarker,
            WindowFrameMotion,
            FullscreenDefaultsDeferred,
            WindowDefaultsApplied,
        )>();
    }
}

fn reconcile_existing_window(
    mut window: Window,
    application: Entity,
    ctx: &mut SpawnWindowCtx,
) -> Option<Window> {
    let window_id = window.id();
    let incarnation = window.incarnation();
    window = adopt_window_representation(window, application, ctx)?;
    let Some((entity, existing, parent)) = ctx.windows.iter().find(|(_, existing, _)| {
        existing.id() == window_id && existing.incarnation() == incarnation
    }) else {
        return Some(window);
    };
    if parent.parent() != application {
        let Ok(frame) = window
            .update_frame()
            .inspect_err(|error| warn!(window_id, %error, "unable to transfer window ownership"))
        else {
            return None;
        };
        if let Ok((_, mut old_app)) = ctx.apps.get_mut(parent.parent()) {
            old_app.unobserve_window(existing);
        }
        transfer_window_ownership(entity, application, window, frame, ctx);
    }
    None
}

/// An application may replace its public AX root without creating another
/// independently presented window. Keep layout identity out of that protocol.
type RepresentationBlocked = bevy::ecs::query::Or<(
    With<WindowSpaceReassignmentPending>,
    With<super::native_space::NativeMoveOwner>,
    With<WindowVisibility>,
)>;

fn replacement_source(
    candidate: &mut Window,
    application: Entity,
    ctx: &mut SpawnWindowCtx,
) -> Option<Entity> {
    let id = candidate.id();
    let existing: Vec<_> = ctx
        .windows
        .iter()
        .filter(|(_, _, owner)| owner.parent() == application)
        .collect();
    if existing.is_empty() || existing.iter().any(|(_, window, _)| window.id() == id) {
        return None;
    }
    let (_, app) = ctx.apps.get(application).ok()?;
    let inventory = app.window_inventory(&ctx.config).ok()?;
    if !inventory.complete
        || !inventory
            .identities
            .contains(&(id, candidate.incarnation()))
    {
        return None;
    }
    let eligible: HashSet<_> = ctx
        .windows
        .iter()
        .filter_map(|(entity, previous, owner)| {
            (owner.parent() == application
                && !ctx.representation_blocked.contains(entity)
                && (previous
                    .represented_window_id()
                    .is_ok_and(|target| target == id)
                    || !inventory
                        .identities
                        .iter()
                        .any(|(published, _)| *published == previous.id())))
            .then_some(entity)
        })
        .collect();
    if eligible.is_empty() || candidate.try_is_full_screen().unwrap_or(true) {
        return None;
    }
    // Geometry cannot disambiguate simultaneous new roots. Only bootstrap a
    // one-to-one publication transition; retained native ownership is stronger.
    let unpublished_targets = inventory
        .identities
        .iter()
        .filter(|(published, _)| {
            !ctx.windows.iter().any(|(_, window, owner)| {
                owner.parent() == application && window.id() == *published
            })
        })
        .count();
    let frame = candidate.update_frame().ok()?;
    let owners = ctx.window_manager.window_owners_in_session();
    let memberships = ctx.topology.observe_memberships(&ctx.window_manager).ok();
    let new_space = memberships
        .as_ref()
        .and_then(|memberships| memberships.unique_space(id));
    let presented = new_space.and_then(|space| {
        ctx.window_manager
            .presentation_windows_in_workspace(space)
            .ok()
    });
    let mut matches = ctx.windows.iter().filter(|(entity, previous, owner)| {
        if !eligible.contains(entity)
            || owner.parent() != application
            || previous.id() == id
            || previous.is_minimized()
            || ctx.representation_blocked.contains(*entity)
            || previous.try_is_full_screen().unwrap_or(true)
        {
            return false;
        }
        let represented = previous.represented_window_id();
        if represented.is_ok_and(|target| target == id) {
            return true;
        }
        // Bootstrap before shared native chrome existed. Require one uniquely
        // withdrawn, still-live public root with the same physical rectangle.
        unpublished_targets == 1
            && !inventory
                .identities
                .iter()
                .any(|(id, _)| *id == previous.id())
            && owners
                .as_ref()
                .is_some_and(|owners| owners.get(&previous.id()) == Some(&app.pid()))
            && new_space.is_some_and(|space| {
                !ctx.topology.is_fullscreen(space)
                    && memberships
                        .as_ref()
                        .and_then(|memberships| memberships.unique_space(previous.id()))
                        == Some(space)
            })
            && presented
                .as_ref()
                .is_some_and(|ids| ids.contains(&id) && !ids.contains(&previous.id()))
            && {
                let padding = bevy::math::IVec2::new(
                    previous.horizontal_padding(),
                    previous.vertical_padding(),
                );
                let observed = previous.frame();
                IRect::from_corners(observed.min + padding, observed.max - padding) == frame
            }
    });
    let (entity, _, _) = matches.next()?;
    matches.next().is_none().then_some(entity)
}

fn adopt_window_representation(
    mut candidate: Window,
    application: Entity,
    ctx: &mut SpawnWindowCtx,
) -> Option<Window> {
    let Some(entity) = replacement_source(&mut candidate, application, ctx) else {
        return Some(candidate);
    };
    let Ok((_, previous, _)) = ctx.windows.get(entity) else {
        return Some(candidate);
    };
    let id = candidate.id();
    let horizontal = previous.horizontal_padding();
    let vertical = previous.vertical_padding();
    candidate.set_padding(crate::manager::WindowPadding::Horizontal(horizontal));
    candidate.set_padding(crate::manager::WindowPadding::Vertical(vertical));
    let _ = candidate.represented_window_id();
    let Ok(frame) = candidate.update_frame() else {
        return Some(candidate);
    };
    if let Ok((_, mut app)) = ctx.apps.get_mut(application) {
        app.unobserve_window(previous);
        _ = app
            .observe_window(&candidate)
            .inspect_err(|error| warn!(id, %error, "window representation observer failed"));
    }
    ctx.sync.forget_window(entity);
    let target = ctx.targets.get(entity).map(|frame| frame.0).ok();
    let mut commands = ctx.commands.entity(entity);
    commands.insert((candidate, ObservedWindowFrame(frame)));
    commands.remove::<(
        WindowUnavailable,
        RepositionMarker,
        ResizeMarker,
        WindowFrameMotion,
        WindowFrameCommitSuspended,
    )>();
    if let Some(target) = target {
        commands.insert(WindowFrameCorrection(target));
    }
    debug!(
        ?entity,
        id, "rebound independent window control target without changing layout"
    );
    None
}

type SpawnCandidateBuckets = HashMap<(Entity, WinID), HashSet<WindowIncarnation>>;

fn collect_spawn_candidates(
    target_application: Option<Entity>,
    new_windows: Vec<Window>,
    ctx: &mut SpawnWindowCtx,
) -> (Vec<(Entity, Window)>, SpawnCandidateBuckets) {
    let mut candidates = Vec::new();
    let mut buckets = SpawnCandidateBuckets::new();
    let mut publications = HashMap::new();

    for window in new_windows {
        let window_id = window.id();
        let Ok(pid) = window.pid() else {
            trace!("Unable to get window pid for {window_id}");
            continue;
        };
        let Some(app_entity) = ctx
            .apps
            .iter_mut()
            .find(|(entity, app)| {
                app.pid() == pid
                    && target_application.is_none_or(|target| *entity == target)
                    && (target_application.is_some()
                        || app.owns_window(&window).is_ok_and(|owned| owned))
            })
            .map(|(entity, _)| entity)
        else {
            trace!("unable to find application with pid {pid}.");
            continue;
        };
        let publication = publications.entry(app_entity).or_insert_with(|| {
            ctx.apps
                .get(app_entity)
                .ok()
                .and_then(|(_, app)| app.window_inventory(&ctx.config).ok())
                .filter(|inventory| inventory.complete)
                .map(|inventory| inventory.identities.into_iter().collect::<HashSet<_>>())
        });
        if publication
            .as_ref()
            .is_some_and(|identities| !identities.contains(&(window_id, window.incarnation())))
            && !window.is_minimized()
            && let Ok(memberships) = ctx.topology.observe_memberships(&ctx.window_manager)
            && let Some(space) = memberships.unique_space(window_id).filter(|space| {
                ctx.topology.visible_display_for_space(*space).is_some()
                    && !ctx.topology.is_fullscreen(*space)
            })
            && ctx
                .window_manager
                .presentation_windows_in_workspace(space)
                .is_ok_and(|ids| !ids.contains(&window_id))
        {
            debug!(
                window_id,
                "not admitting an unpublished, nonpresented native object as a layout window"
            );
            continue;
        }
        // Capture opaque native chrome before the application changes its AX root.
        let _ = window.represented_window_id();
        if ctx.sync.is_window_retired(app_entity, &window) {
            debug!(
                window_id,
                incarnation = window.incarnation(),
                ?app_entity,
                "ignoring retired AX window incarnation"
            );
            continue;
        }
        buckets
            .entry((app_entity, window_id))
            .or_default()
            .insert(window.incarnation());
        candidates.push((app_entity, window));
    }

    (candidates, buckets)
}

fn log_spawned_window(window: &Window) {
    if !tracing::enabled!(Level::DEBUG) {
        return;
    }
    let window_id = window.id();
    let title = window.title().unwrap_or_default();
    let role = window.role().unwrap_or_default();
    let subrole = window.subrole().unwrap_or_default();
    let element = window
        .element()
        .map(|element| format!("{element}"))
        .unwrap_or_default();
    debug!("created {window_id} title: {title} role: {role} subrole: {subrole} element: {element}",);
}

/// Handles the event when a new window is created. It adds the window to the manager and sets focus.
///
/// # Arguments
///
/// * `trigger` - The Bevy event trigger containing the new windows.
/// * `windows` - A query for all windows.
/// * `apps` - A query for all applications.
/// * `active_display` - A query for the active display.
/// * `main_cid` - The main connection ID resource.
/// * `commands` - Bevy commands to manage components and trigger events.
#[instrument(level = Level::DEBUG, skip_all)]
pub(super) fn spawn_window_trigger(mut trigger: On<SpawnWindowTrigger>, mut ctx: SpawnWindowCtx) {
    let target_application = trigger.event().application;
    let new_windows = std::mem::take(&mut trigger.event_mut().windows);
    let (candidates, candidate_buckets) =
        collect_spawn_candidates(target_application, new_windows, &mut ctx);

    let mut processed_buckets = HashSet::new();
    for (app_entity, window) in candidates {
        let window_id = window.id();
        let bucket = (app_entity, window_id);
        let incarnation_count = candidate_buckets.get(&bucket).map_or(0, HashSet::len);
        if incarnation_count > 1 {
            if processed_buckets.insert(bucket) {
                debug!(
                    window_id,
                    ?app_entity,
                    count = incarnation_count,
                    "deferring ambiguous AX window candidates to inventory reconciliation"
                );
            }
            continue;
        }
        if !processed_buckets.insert(bucket) {
            continue;
        }
        let Ok(pid) = window.pid() else {
            trace!("Unable to get window pid for {window_id}");
            continue;
        };
        let Some(mut window) = reconcile_existing_window(window, app_entity, &mut ctx) else {
            continue;
        };

        let Ok((_, mut app)) = ctx.apps.get_mut(app_entity) else {
            continue;
        };

        // Pending capability/rule reads retain identity without reserving a
        // column. The defaults transaction will retry and decide placement.
        let properties = WindowProperties::new(&app, &window, &ctx.config);
        // Native capability and current rules govern initial placement.
        let starts_floating = properties.pending
            || window.layout_decision(window.default_floating() && properties.floating())
                != LayoutDecision::Tile;

        log_spawned_window(&window);

        match app.observe_window(&window) {
            Ok(true) => {}
            Ok(false) => debug!(window_id, "some window observers need retry"),
            Err(error) => warn!(window_id, %error, "unable to register window observers"),
        }

        // Seed the observed projection immediately. `apply_window_defaults`
        // refreshes it again after applying padding or frame rules.
        let Ok(frame) = window.update_frame().inspect_err(|err| error!("{err}")) else {
            continue;
        };
        let position = Position(frame.min);
        let bounds = Bounds(frame.size());
        let launch_snapshot = ctx
            .initializing
            .as_ref()
            .and_then(|_| ctx.launch_capture.capture(&window, &app, frame));
        let layout_position = LayoutPosition::default();

        let title = window.title().unwrap_or_default();
        let app_name = app.name().to_string();
        let bundle_id = app.bundle_id().unwrap_or_default().clone();
        let window_frame = spool_shared_types::state::Frame {
            x: frame.min.x,
            y: frame.min.y,
            width: frame.width(),
            height: frame.height(),
        };

        // Insert the window into the internal Bevy state.
        // This insertion triggers window attributes observer.
        let mut entity_commands = ctx.commands.spawn((
            position,
            bounds,
            DesiredWindowFrame(frame),
            PresentedWindowFrame(frame),
            ObservedWindowFrame(frame),
            window,
            layout_position,
            ChildOf(app_entity),
            WindowDefaultsPending,
        ));
        if starts_floating {
            entity_commands.insert(Floating);
        }
        if ctx.initializing.is_some() {
            entity_commands.insert(InitialWindowMarker);
        }
        if let Some(snapshot) = launch_snapshot {
            entity_commands.insert(snapshot);
        }

        ctx.commands
            .trigger(SendMessageTrigger(Event::WindowSpawned {
                window_id,
                pid,
                app_name,
                bundle_id,
                title,
                frame: window_frame,
                floating: starts_floating,
            }));
    }
}

type DefaultFrameState<'a> = (
    &'a mut Position,
    &'a mut Bounds,
    &'a mut DesiredWindowFrame,
    &'a mut PresentedWindowFrame,
    Option<&'a mut ObservedWindowFrame>,
);

fn commit_default_frame(
    entity: Entity,
    frame: IRect,
    state: DefaultFrameState<'_>,
    commands: &mut Commands,
) {
    let (position, bounds, desired, presented, observed) = state;
    position.0 = frame.min;
    bounds.0 = frame.size();
    desired.0 = frame;
    presented.0 = frame;
    if let Some(observed) = observed {
        observed.0 = frame;
    } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(ObservedWindowFrame(frame));
    }
}

fn invalidate_default_frame(entity: Entity, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<ObservedWindowFrame>();
    }
}

fn mark_window_defaults_applied(entity: Entity, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(WindowDefaultsApplied);
    }
}

#[derive(SystemParam)]
pub(super) struct DefaultGeometry<'w, 's> {
    topology: Res<'w, super::topology::NativeTopology>,
    displays: Query<'w, 's, (&'static Display, Option<&'static DockPosition>)>,
    manager: Res<'w, WindowManager>,
    retries: ResMut<'w, super::defaults::DefaultRetries>,
    time: Res<'w, bevy::time::Time>,
}

impl DefaultGeometry<'_, '_> {
    fn viewport(&self, window_id: WinID, config: &Config) -> Option<IRect> {
        if !self.topology.is_complete() {
            return None;
        }
        let mut owner = None;
        for (display, spaces) in self.topology.known_displays() {
            for space in spaces {
                let members = self.manager.windows_in_workspace(*space).ok()?;
                if members.contains(&window_id) {
                    if owner.is_some() {
                        return None;
                    }
                    owner = Some(display.id());
                }
            }
        }
        let owner = owner?;
        self.displays
            .iter()
            .find(|(display, _)| display.id() == owner)
            .map(|(display, dock)| display.actual_display_bounds(dock, config))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "default application is one retryable transaction across floating and tiled rules"
)]
pub(super) fn apply_window_defaults(
    added: DefaultableWindows,
    apps: Query<(Entity, &Application)>,
    mut geometry: DefaultGeometry,
    config: Res<Config>,
    initializing: Option<Res<Initializing>>,
    mut commands: Commands,
) {
    for (
        entity,
        ref mut window,
        mut position,
        mut bounds,
        mut desired,
        mut presented,
        mut observed,
        child,
    ) in added
    {
        let Ok((_, app)) = apps.get(child.parent()) else {
            continue;
        };

        let now = geometry.time.elapsed();
        if !geometry.retries.admit(entity, window.incarnation(), now) {
            continue;
        }

        // A startup fullscreen frame is physical state owned by macOS, not a
        // sensible tiling size. Keep the defaults transaction pending until
        // AX positively confirms that the window is windowed again.
        match window.try_is_full_screen() {
            Ok(true) => {
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_insert(FullscreenDefaultsDeferred);
                }
                continue;
            }
            Err(_) => continue,
            Ok(false) => {}
        }

        let properties = WindowProperties::new(app, window, &config);
        debug!("Applying window defaults for '{}'", window.id());

        let initializing = initializing.is_some();
        let decision = properties.layout_decision(window);
        if decision == LayoutDecision::Defer {
            commands.entity(entity).insert(Floating);
            continue;
        }

        // Do not add padding to floating windows.
        if let LayoutDecision::Float(reason) = decision {
            debug!(
                window_id = window.id(),
                ?reason,
                "applying floating defaults"
            );
            // Rule placement remains transactional until defaults complete.
            if reason != crate::window_policy::FloatReason::Rule {
                commands.entity(entity).insert(Floating);
            }
            let grid = properties.grid_ratios();
            let grid_capability = if !initializing && grid.is_some() {
                window.layout_decision(false)
            } else {
                decision
            };
            if grid_capability == LayoutDecision::Defer {
                continue;
            }
            // Skip grid_ratios during init: we don't know this window's display.
            let applied = if !initializing
                && grid_capability == LayoutDecision::Tile
                && let Some((rx, ry, rw, rh)) = grid
            {
                let Some(viewport) = geometry.viewport(window.id(), &config) else {
                    continue;
                };
                let x = viewport.min.x + round_px(f64::from(viewport.width()) * rx);
                let y = viewport.min.y + round_px(f64::from(viewport.height()) * ry);
                let w = round_px(f64::from(viewport.width()) * rw);
                let h = round_px(f64::from(viewport.height()) * rh);
                let target = IRect::from_corners(Origin::new(x, y), Origin::new(x + w, y + h));
                commands.entity(entity).insert(DefaultWindowFrame {
                    target,
                    incarnation: window.incarnation(),
                });
                false
            } else if observed.is_none() {
                match window.update_frame() {
                    Ok(frame) => {
                        commit_default_frame(
                            entity,
                            frame,
                            (
                                &mut position,
                                &mut bounds,
                                &mut desired,
                                &mut presented,
                                None,
                            ),
                            &mut commands,
                        );
                        true
                    }
                    Err(error) => {
                        warn!(window_id = window.id(), %error, "unable to refresh floating window defaults");
                        invalidate_default_frame(entity, &mut commands);
                        false
                    }
                }
            } else {
                true
            };
            if applied {
                mark_window_defaults_applied(entity, &mut commands);
            }
            continue;
        }
        let vpadding = properties.vertical_padding();
        let hpadding = properties.horizontal_padding();
        window.set_padding(WindowPadding::Vertical(vpadding.clamp(0, 50)));
        window.set_padding(WindowPadding::Horizontal(hpadding.clamp(0, 50)));
        let Ok(frame) = window.update_frame() else {
            invalidate_default_frame(entity, &mut commands);
            continue;
        };
        commit_default_frame(
            entity,
            frame,
            (
                &mut position,
                &mut bounds,
                &mut desired,
                &mut presented,
                observed.as_deref_mut(),
            ),
            &mut commands,
        );

        // Tiled width rules are inherited column intent. Projection applies
        // them after placement, including on a background Space.
        mark_window_defaults_applied(entity, &mut commands);
    }
}

#[derive(SystemParam)]
pub(super) struct ApplyWindowPositionsCtx<'w, 's> {
    focus: Res<'w, FocusCoordinator>,
    topology: Res<'w, super::topology::NativeTopology>,
    window_manager: Res<'w, WindowManager>,
    window: WindowCtx<'w, 's>,
}

fn finish_pending_window_defaults(entity: Entity, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<(
            WindowDefaultsPending,
            WindowDefaultsApplied,
            FullscreenDefaultsDeferred,
            InitialWindowMarker,
            WindowOwnershipChanged,
        )>();
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
#[allow(
    clippy::too_many_lines,
    reason = "placement is one ordered transaction covering rules, insertion, and focus"
)]
pub(super) fn apply_window_positions(
    added: PositionedWindows,
    mut workspaces: Query<
        (&mut LayoutStrip, Has<ActiveWorkspaceMarker>),
        Without<PendingSpaceDestruction>,
    >,
    apps: Query<&Application>,
    initializing: Option<Res<Initializing>>,
    ctx: ApplyWindowPositionsCtx,
) {
    let ApplyWindowPositionsCtx {
        focus,
        topology,
        window_manager,
        window: mut ctx,
    } = ctx;
    let mut memberships = None;
    for (entity, initial_window) in added {
        if workspaces.iter().any(|(strip, _)| strip.tabbed(entity)) {
            debug!("Ignoring tabbed {entity} attributes.");
            finish_pending_window_defaults(entity, &mut ctx.commands);
            continue;
        }

        let Some((window, _, parent)) = ctx.windows.get_parent(entity) else {
            continue;
        };
        let Ok(app) = apps.get(parent) else {
            continue;
        };

        let in_fullscreen_strip = workspaces
            .iter()
            .any(|(strip, _)| strip.is_fullscreen() && strip.contains(entity));
        if in_fullscreen_strip || window.try_is_full_screen().unwrap_or(true) {
            continue;
        }

        let properties = WindowProperties::new(app, window, &ctx.config);
        let decision = properties.layout_decision(window);
        if decision == LayoutDecision::Defer {
            ctx.commands
                .entity(entity)
                .insert(Floating)
                .remove::<WindowDefaultsApplied>();
            continue;
        }

        let already_inserted = workspaces.iter().any(|(strip, _)| strip.contains(entity));

        if matches!(decision, LayoutDecision::Float(_)) {
            if let Some(mut strip) = workspaces
                .iter_mut()
                .find_map(|(strip, _)| strip.contains(entity).then_some(strip))
            {
                strip.remove(entity);
            }
            if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
                // Avoid managing window if it's floating.
                entity_commands.try_insert(Floating);
            }
            finish_pending_window_defaults(entity, &mut ctx.commands);
            continue;
        }

        if !already_inserted {
            // Capability retries can finish after startup or a Space switch.
            // Place on the observed owner, never on whichever Space is active now.
            let membership =
                memberships.get_or_insert_with(|| topology.observe_memberships(&window_manager));
            let Some(workspace_id) = membership
                .as_ref()
                .ok()
                .and_then(|members| members.unique_space(window.id()))
                .filter(|space| !topology.is_fullscreen(*space))
            else {
                continue;
            };
            let Some(mut strip) = workspaces
                .iter_mut()
                .find_map(|(strip, _)| (strip.id() == workspace_id).then_some(strip))
            else {
                continue;
            };
            // Attempt inserting the window at a pre-defined position.
            let insert_at = properties.insertion().map_or_else(
                || {
                    // Native focus may already belong to the new window before
                    // it has a column. Retain the owning Space's tiled anchor.
                    let eligible = |entity| {
                        strip.contains(entity)
                            && ctx
                                .windows
                                .get_tracked(entity)
                                .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
                    };
                    ctx.windows
                        .focused()
                        .map(|(_, entity)| entity)
                        .filter(|&entity| eligible(entity))
                        .or_else(|| focus.last_tiled_matching(workspace_id, eligible))
                        .and_then(|entity| strip.index_of(entity).ok())
                        .map(|index| index + 1)
                },
                Some,
            );

            debug!("New window {entity} adding at {}", *strip);
            match insert_at {
                Some(after) => {
                    debug!("New window inserted at {after}");
                    strip.insert_at(after, entity);
                }
                None => strip.append(entity),
            }
        }

        if let Ok(mut entity_commands) = ctx.commands.get_entity(entity) {
            entity_commands.try_remove::<Floating>();
        }

        // During init, skip per-window reshuffles. finish_setup does a single
        // reshuffle after all windows are added.
        if !initial_window && initializing.is_none() {
            if properties.dont_focus() {
                let previous = focus
                    .snapshot()
                    .requested_entity()
                    .or_else(|| ctx.windows.focused().map(|(_, entity)| entity));
                if let Some(prev) = previous
                    && let Some(previous_window) = ctx.windows.get(prev)
                {
                    debug!(
                        "Not focusing new window {entity}, keeping focus on '{}'",
                        previous_window.title().unwrap_or_default()
                    );
                    ctx.commands.restore_focus_entity(prev, true);
                }
            } else {
                debug!("Synthesizing WindowFocused for newly spawned window {entity}");
                ctx.commands
                    .trigger(SendMessageTrigger(Event::window_focused(window.id())));
            }
        }
        finish_pending_window_defaults(entity, &mut ctx.commands);
    }
}

pub(super) fn window_removal_trigger(
    trigger: On<Remove, Window>,
    mut workspaces: Query<&mut LayoutStrip>,
    mut focus: ResMut<FocusCoordinator>,
) {
    let entity = trigger.event().entity;

    focus.observe(FocusSignal::Invalidated { entity });
    focus.forget(entity);
    for mut strip in workspaces.iter_mut().filter(|strip| strip.contains(entity)) {
        debug!(
            "Removing despawned entity {entity} from strip {}",
            strip.id()
        );
        strip.remove(entity);
    }
}

pub(super) fn send_message_trigger(
    trigger: On<SendMessageTrigger>,
    mut messages: MessageWriter<Event>,
) {
    let event = &trigger.event().0;
    messages.write(event.clone());
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn cleanup_timeout_trigger(
    trigger: On<Remove, Timeout>,
    all_timeouts: Query<&Timeout>,
    mut commands: Commands,
) {
    if let Ok(timeout) = all_timeouts.get(trigger.entity)
        && let Some(system_id) = timeout.system_id
    {
        commands.unregister_system(system_id);
    }
}

/// Rule defaults are derived configuration, never explicit width edits.
/// Keep one source per column so stack/tab reordering cannot change the rule.
pub(super) fn refresh_column_width_defaults(
    mut strips: Query<&mut LayoutStrip>,
    windows: Windows,
    apps: Query<&Application>,
    config: Res<Config>,
) {
    for mut strip in &mut strips {
        let updates = strip
            .columns()
            .zip(strip.column_states())
            .filter_map(|(column, state)| {
                if state.config_source.is_some() && !config.is_changed() {
                    return None;
                }
                let source = state
                    .config_source
                    .or_else(|| column.window_iter().next())?;
                let (window, _, parent) = windows.get_parent_any(source)?;
                let app = apps.get(parent).ok()?;
                let width = WindowProperties::new(app, window, &config)
                    .width_ratio()
                    .map(WidthIntent::ViewportRatio);
                Some((state.id, source, width))
            })
            .collect::<Vec<_>>();
        for (id, source, width) in updates {
            match strip
                .bypass_change_detection()
                .set_column_rule(id, source, width)
            {
                Ok(true) => strip.set_changed(),
                Ok(false) => {}
                Err(error) => warn!(?id, %error, "invalid inherited column width"),
            }
        }
    }
}

pub(super) fn window_resize_verifier(
    mut removed: RemovedComponents<WindowFrameMotion>,
    mut windows: ResizeVerificationWindows,
    mut commands: Commands,
) {
    use std::cmp::Ordering;
    for entity in removed.read() {
        let Ok((
            mut window,
            mut position,
            mut bounds,
            mut desired,
            mut presented,
            observed,
            floating,
        )) = windows.get_mut(entity)
        else {
            continue;
        };
        if window.try_is_full_screen().unwrap_or(true) {
            continue;
        }
        let Ok(frame) = window.update_frame() else {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<ObservedWindowFrame>();
            }
            continue;
        };
        if let Some(mut observed) = observed {
            if observed.0 != frame {
                observed.0 = frame;
            }
        } else {
            commands.entity(entity).insert(ObservedWindowFrame(frame));
        }

        if floating {
            position.0 = frame.min;
            bounds.0 = frame.size();
            desired.0 = frame;
            presented.0 = frame;
            continue;
        }

        let actual_size = frame.size();
        let expected_size = desired.0.size();

        // note: macOS loves to make window sizes to even numbers, so we treat actual+1 as equal.
        let width_ord = fuzzy_equal(actual_size.x, expected_size.x);
        let height_ord = fuzzy_equal(actual_size.y, expected_size.y);

        if width_ord == Ordering::Equal && height_ord == Ordering::Equal {
            continue;
        }
        debug!(
            "window '{}'({}) did not fully resized to {}, was {} instead",
            window.title().unwrap_or_default(),
            window.id(),
            expected_size,
            actual_size,
        );
        // Bounds is the tiled layout's desired size. Keep that intent intact
        // and ask the central reconciler to retry from the observed frame.
        commands.trigger(SendMessageTrigger(Event::WindowResized {
            window_id: window.id(),
            incarnation: window.incarnation(),
        }));
    }
}

fn fuzzy_equal<N>(actual_size: N, expected_size: N) -> Ordering
where
    N: std::ops::Sub<Output = N> + Ord + From<i8>,
{
    let diff = expected_size - actual_size;

    if diff < N::from(-1) {
        Ordering::Less
    } else if diff > N::from(1) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

#[cfg(test)]
mod focus_query_logging_tests {
    use accessibility_sys::{kAXErrorCannotComplete, kAXErrorNoValue};

    use super::*;
    use crate::util::MacResult as _;

    #[test]
    fn no_value_is_debug_but_other_ax_errors_still_warn() {
        let no_value = kAXErrorNoValue
            .to_result("focused window")
            .expect_err("NoValue must remain an error so focus is retried");
        let cannot_complete = kAXErrorCannotComplete
            .to_result("focused window")
            .expect_err("CannotComplete must remain an error so focus is retried");

        assert_eq!(focus_query_failure_level(&no_value), Level::DEBUG);
        assert_eq!(focus_query_failure_level(&cannot_complete), Level::WARN);
    }
}

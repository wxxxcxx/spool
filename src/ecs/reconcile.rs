use std::collections::{HashMap, HashSet};
use std::time::Duration;

use accessibility_sys::kAXErrorNoValue;
use bevy::ecs::change_detection::DetectChangesMut as _;
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, Without};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut, SystemParam};
use bevy::math::IRect;
use bevy::time::{Time, Timer, TimerMode};
use tracing::{debug, warn};

use crate::config::Config;
use crate::ecs::focus::{FocusCoordinator, FocusSignal, FocusSnapshot};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::window_geometry::WindowGeometrySettling;
use crate::ecs::workspace::WindowSpaceReassignmentPending;
use crate::ecs::{
    Bounds, DesiredWindowFrame, Floating, Initializing, MissionControlActive, ObservedWindowFrame,
    Position, PresentedWindowFrame, RepositionMarker, ResizeMarker, SendMessageTrigger,
    SpawnWindowTrigger, WindowDefaultsPending, WindowFrameCommitSuspended, WindowFrameMotion,
    WindowVisibility,
};
use crate::errors::Error;
use crate::events::{Event, FocusSource, ReconcileScope};
use crate::manager::{Application, Window, WindowManager};
use crate::platform::{Pid, WinID, WindowIncarnation};

const CONFIRMATION_DELAY: Duration = Duration::from_millis(250);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const FRAME_RETRY_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_FRAME_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug)]
struct FrameConvergence {
    target: IRect,
    attempts: u8,
    retry_after: Option<Duration>,
}

/// Coalesces unreliable macOS notifications with a low-frequency inventory
/// audit. Notifications keep the common path responsive; the heartbeat closes
/// gaps when AX, SLS, or `WindowServer` drops an event.
#[derive(Resource, Debug, Default)]
pub(crate) struct WindowStateSync {
    since_heartbeat: Duration,
    frame_convergence: HashMap<Entity, FrameConvergence>,
    application_observers: HashSet<Entity>,
    window_observers: HashSet<Entity>,
    focus_absent: HashMap<Entity, FocusSnapshot>,
    retired_window_incarnations: HashSet<(Entity, WindowKey)>,
    // Keep AX-known IDs while their native surfaces survive so presentation
    // fallback cannot resurrect an ignored or retired window as "undiscovered".
    ax_observed_windows: HashMap<Entity, HashSet<WinID>>,
}

impl WindowStateSync {
    pub(crate) fn has_observed_ax_window(&self, application: Entity, id: WinID) -> bool {
        self.ax_observed_windows
            .get(&application)
            .is_some_and(|ids| ids.contains(&id))
    }

    fn remember_ax_windows(
        &mut self,
        application: Entity,
        pid: Pid,
        identities: &[(WinID, WindowIncarnation)],
        owners: &HashMap<WinID, Pid>,
    ) {
        let observed = self.ax_observed_windows.entry(application).or_default();
        observed.retain(|id| owners.get(id) == Some(&pid));
        observed.extend(identities.iter().map(|(id, _)| *id));
    }

    pub(crate) fn forget_window(&mut self, entity: Entity) {
        self.frame_convergence.remove(&entity);
        self.window_observers.remove(&entity);
    }

    pub(crate) fn retire_window(&mut self, application: Entity, window: &Window) {
        self.retired_window_incarnations
            .insert((application, WindowKey::new(window)));
    }

    pub(crate) fn is_window_retired(&self, application: Entity, window: &Window) -> bool {
        self.retired_window_incarnations
            .contains(&(application, WindowKey::new(window)))
    }

    fn is_key_retired(&self, application: Entity, key: WindowKey) -> bool {
        self.retired_window_incarnations
            .contains(&(application, key))
    }

    fn forget_application(&mut self, application: Entity) {
        self.ax_observed_windows.remove(&application);
        self.application_observers.remove(&application);
        self.focus_absent.remove(&application);
        self.retired_window_incarnations
            .retain(|(owner, _)| *owner != application);
    }

    fn tick(&mut self, delta: Duration) -> bool {
        self.since_heartbeat = self.since_heartbeat.saturating_add(delta);
        if self.since_heartbeat < HEARTBEAT_INTERVAL {
            return false;
        }
        self.since_heartbeat = Duration::ZERO;
        true
    }

    fn converge_tiled_frame(
        &mut self,
        entity: Entity,
        window_id: WinID,
        window: &mut Window,
        mut frame: IRect,
        target: IRect,
        now: Duration,
    ) -> Option<IRect> {
        if frames_equivalent(frame, target) {
            self.frame_convergence.remove(&entity);
            return Some(frame);
        }

        let convergence = self
            .frame_convergence
            .entry(entity)
            .or_insert(FrameConvergence {
                target,
                attempts: 0,
                retry_after: None,
            });
        if convergence.target != target {
            *convergence = FrameConvergence {
                target,
                attempts: 0,
                retry_after: None,
            };
        }

        if convergence.attempts >= MAX_FRAME_ATTEMPTS {
            let retry_after = convergence
                .retry_after
                .get_or_insert(now.saturating_add(FRAME_RETRY_COOLDOWN));
            if now >= *retry_after {
                convergence.attempts = 0;
                convergence.retry_after = None;
            } else {
                debug!(
                    window_id,
                    ?target,
                    ?frame,
                    ?retry_after,
                    "window frame remains constrained during retry cooldown"
                );
                return Some(frame);
            }
        }

        convergence.attempts += 1;
        match window.set_frame(target) {
            Ok(confirmed_frame) => {
                frame = confirmed_frame;
                if frames_equivalent(frame, target) {
                    self.frame_convergence.remove(&entity);
                } else if let Some(convergence) = self.frame_convergence.get_mut(&entity)
                    && convergence.attempts >= MAX_FRAME_ATTEMPTS
                {
                    convergence.retry_after = Some(now.saturating_add(FRAME_RETRY_COOLDOWN));
                }
            }
            Err(error) => {
                warn!(
                    window_id,
                    attempt = convergence.attempts,
                    %error,
                    "unable to converge tiled window frame"
                );
                return None;
            }
        }
        Some(frame)
    }
}

#[derive(Debug, Default)]
struct SyncRequest {
    all: bool,
    pids: HashSet<Pid>,
    frame_ids: HashSet<WinID>,
}

impl SyncRequest {
    fn collect(messages: &mut MessageReader<Event>, heartbeat: bool) -> Self {
        let mut request = Self {
            all: heartbeat,
            ..Self::default()
        };
        for event in messages.read() {
            match event {
                Event::ReconcileWindows { scope } => match scope {
                    ReconcileScope::Application(pid) => {
                        request.pids.insert(*pid);
                    }
                    ReconcileScope::All => request.all = true,
                },
                Event::WindowMoved { window_id, .. } | Event::WindowResized { window_id, .. } => {
                    request.frame_ids.insert(*window_id);
                }
                Event::ApplicationActivated { pid }
                | Event::ApplicationDeactivated { pid }
                | Event::ApplicationVisible { pid }
                | Event::ApplicationHidden { pid } => {
                    request.pids.insert(*pid);
                }
                Event::WindowDestroyed {
                    incarnation: None, ..
                }
                | Event::MouseUp { .. }
                | Event::SpaceChanged
                | Event::SpaceDestroyed { .. }
                | Event::MissionControlExit
                | Event::DisplayChanged
                | Event::SystemWoke { .. } => request.all = true,
                _ => {}
            }
        }
        request
    }

    fn is_empty(&self) -> bool {
        !self.all && self.pids.is_empty() && self.frame_ids.is_empty()
    }

    fn lifecycle_requested(&self) -> bool {
        self.all || !self.pids.is_empty()
    }

    fn includes_application(&self, pid: Pid) -> bool {
        self.all || self.pids.contains(&pid)
    }
}

#[derive(Default)]
struct LifecycleAudit {
    window_server: Option<HashMap<WinID, Pid>>,
    confirmed: HashSet<Entity>,
    retiring: HashSet<Entity>,
    actions: Vec<LifecycleAction>,
}

enum LifecycleAction {
    Resume(Entity, bool),
    Suspend(Entity, bool),
    Destroy(Entity),
    Spawn(Entity, Vec<Window>),
    RetireApplication(Entity),
}

impl LifecycleAction {
    fn priority(&self) -> u8 {
        match self {
            Self::Resume(..) | Self::Suspend(..) => 0,
            Self::Destroy(_) => 1,
            Self::Spawn(..) => 2,
            Self::RetireApplication(_) => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WindowKey {
    id: WinID,
    incarnation: WindowIncarnation,
}

struct ApplicationInventory<'a> {
    entity: Entity,
    pid: Pid,
    buckets: &'a HashMap<WinID, HashSet<WindowKey>>,
    complete: bool,
}

impl WindowKey {
    fn new(window: &Window) -> Self {
        Self {
            id: window.id(),
            incarnation: window.incarnation(),
        }
    }
}

fn inventory_buckets(
    identities: Vec<(WinID, WindowIncarnation)>,
    application: Entity,
    sync: &WindowStateSync,
) -> HashMap<WinID, HashSet<WindowKey>> {
    let mut buckets = HashMap::<WinID, HashSet<WindowKey>>::new();
    for (id, incarnation) in identities {
        let key = WindowKey { id, incarnation };
        if !sync.is_key_retired(application, key) {
            buckets.entry(id).or_default().insert(key);
        }
    }
    buckets
}

impl LifecycleAudit {
    fn contains_window(&self, window_id: WinID, pid: Pid) -> bool {
        self.window_server
            .as_ref()
            .is_some_and(|windows| windows.get(&window_id) == Some(&pid))
    }

    fn record_discovered(
        &mut self,
        application: Entity,
        pid: Pid,
        candidates: Vec<Window>,
        buckets: &HashMap<WinID, HashSet<WindowKey>>,
        tracked: &HashMap<WindowKey, Entity>,
        sync: &mut WindowStateSync,
    ) {
        let mut discovered_ids = HashSet::new();
        let discovered = candidates
            .into_iter()
            .filter(|window| {
                let key = WindowKey::new(window);
                let unique_identity = buckets
                    .get(&key.id)
                    .is_some_and(|keys| keys.len() == 1 && keys.contains(&key));
                let conflicting_tracked = tracked.iter().any(|(tracked_key, entity)| {
                    tracked_key.id == key.id
                        && *tracked_key != key
                        && !self.retiring.contains(entity)
                });
                self.contains_window(window.id(), pid)
                    && unique_identity
                    && !conflicting_tracked
                    && !tracked.contains_key(&key)
                    && !sync.is_key_retired(application, key)
                    && discovered_ids.insert(key.id)
            })
            .collect::<Vec<_>>();
        if !discovered.is_empty() {
            debug!(
                pid,
                count = discovered.len(),
                "window-state audit found windows"
            );
            for replacement in &discovered {
                let replacement_key = WindowKey::new(replacement);
                for (tracked_key, entity) in tracked {
                    if tracked_key.id != replacement_key.id
                        || tracked_key.incarnation == replacement_key.incarnation
                    {
                        continue;
                    }
                    retire_tracked_window(application, *tracked_key, *entity, sync, self);
                }
            }
            self.actions
                .push(LifecycleAction::Spawn(application, discovered));
        }
    }
}

/// The AX window is no longer usable, but `CoreGraphics` has not yet confirmed
/// that its surface left the screen. Such windows remain in Spool's declarative
/// layout while being excluded from macOS reads, writes, and focus operations.
#[derive(Component, Debug)]
pub(crate) struct WindowUnavailable {
    confirmation: Timer,
    layout_projection_invalidated: bool,
}

impl WindowUnavailable {
    fn new(layout_projection_invalidated: bool) -> Self {
        Self {
            confirmation: Timer::new(CONFIRMATION_DELAY, TimerMode::Once),
            layout_projection_invalidated,
        }
    }

    pub(crate) fn excludes_from_layout_projection(&self) -> bool {
        self.layout_projection_invalidated
    }
}

type ReconcileWindowData = (
    Entity,
    &'static mut Window,
    &'static ChildOf,
    Option<&'static mut WindowUnavailable>,
    &'static mut Position,
    &'static mut Bounds,
    &'static mut DesiredWindowFrame,
    &'static mut PresentedWindowFrame,
    Option<&'static mut ObservedWindowFrame>,
    Has<Floating>,
    Option<&'static WindowVisibility>,
    (
        Has<WindowDefaultsPending>,
        Has<RepositionMarker>,
        Has<ResizeMarker>,
        Has<WindowFrameMotion>,
        Has<WindowSpaceReassignmentPending>,
    ),
);

#[derive(SystemParam)]
pub(super) struct ReconcileState<'w, 's> {
    applications: Query<'w, 's, (Entity, &'static mut Application)>,
    windows: Query<'w, 's, ReconcileWindowData>,
    workspaces: Query<'w, 's, &'static mut LayoutStrip, Without<Window>>,
    focus: ResMut<'w, FocusCoordinator>,
    mission_control: Res<'w, MissionControlActive>,
    initializing: Option<Res<'w, Initializing>>,
    settling: Res<'w, WindowGeometrySettling>,
}

impl ReconcileState<'_, '_> {
    fn audit_lifecycle(
        &mut self,
        request: &SyncRequest,
        window_manager: &WindowManager,
        config: &Config,
        sync: &mut WindowStateSync,
        commands: &mut Commands,
    ) -> LifecycleAudit {
        let window_server = if request.lifecycle_requested() {
            window_manager.window_owners_in_session()
        } else {
            None
        };
        if request.lifecycle_requested() && window_server.is_none() {
            warn!("window lifecycle audit skipped: unable to read the WindowServer inventory");
        }
        let mut audit = LifecycleAudit {
            window_server,
            ..LifecycleAudit::default()
        };

        for (app_entity, mut app) in &mut self.applications {
            let pid = app.pid();
            if !request.includes_application(pid) {
                continue;
            }
            match app.is_running() {
                Ok(false) => {
                    retire_application(app_entity, &mut self.windows, sync, &mut audit);
                    continue;
                }
                Ok(true) => {}
                Err(error) => {
                    warn!(pid, %error, "application liveness audit failed open");
                }
            }
            refresh_application_observer(app_entity, &mut app, sync);
            let Some(owners) = &audit.window_server else {
                continue;
            };
            let Ok(inventory) = app.window_inventory(config).inspect_err(|error| {
                warn!(pid, %error, "window reconciliation skipped application");
            }) else {
                suspend_windows_missing_from_window_server(
                    app_entity,
                    pid,
                    &mut self.windows,
                    sync,
                    &mut audit,
                );
                continue;
            };
            sync.remember_ax_windows(app_entity, pid, &inventory.identities, owners);
            let buckets = inventory_buckets(inventory.identities, app_entity, sync);
            if !inventory.complete {
                debug!(
                    pid,
                    "AX window inventory incomplete; destructive absence decisions deferred"
                );
            }
            let tracked = audit_application_windows(
                ApplicationInventory {
                    entity: app_entity,
                    pid,
                    buckets: &buckets,
                    complete: inventory.complete,
                },
                &mut app,
                &mut self.windows,
                sync,
                &mut audit,
            );
            audit.record_discovered(
                app_entity,
                pid,
                inventory.candidates,
                &buckets,
                &tracked,
                sync,
            );
        }

        if request.all {
            let live_applications = self
                .applications
                .iter()
                .map(|(entity, _)| entity)
                .collect::<HashSet<_>>();
            sync.application_observers
                .retain(|entity| live_applications.contains(entity));
            sync.focus_absent
                .retain(|entity, _| live_applications.contains(entity));
            sync.retired_window_incarnations
                .retain(|(entity, _)| live_applications.contains(entity));
            sync.ax_observed_windows
                .retain(|entity, _| live_applications.contains(entity));
            let live_windows = self
                .windows
                .iter()
                .map(|(entity, ..)| entity)
                .collect::<HashSet<_>>();
            sync.window_observers
                .retain(|entity| live_windows.contains(entity));
            sync.frame_convergence
                .retain(|entity, _| live_windows.contains(entity));
        }
        self.apply_lifecycle_actions(&mut audit, commands);
        audit
    }

    fn apply_lifecycle_actions(&mut self, audit: &mut LifecycleAudit, commands: &mut Commands) {
        audit.actions.sort_by_key(LifecycleAction::priority);
        for action in audit.actions.drain(..) {
            match action {
                LifecycleAction::Resume(entity, layout_projection_invalidated) => resume_window(
                    entity,
                    layout_projection_invalidated,
                    &mut self.workspaces,
                    commands,
                ),
                LifecycleAction::Suspend(entity, invalidate_layout_projection) => {
                    suspend_window(
                        entity,
                        invalidate_layout_projection,
                        &mut self.workspaces,
                        &mut self.focus,
                        commands,
                    );
                }
                LifecycleAction::Destroy(entity) => {
                    let Ok((_, window, parent, ..)) = self.windows.get_mut(entity) else {
                        continue;
                    };
                    let window_id = window.id();
                    let incarnation = window.incarnation();
                    if let Ok((_, mut app)) = self.applications.get_mut(parent.parent()) {
                        app.unobserve_window(&window);
                    }
                    self.focus.observe(FocusSignal::Invalidated { entity });
                    self.focus.forget(entity);
                    if let Ok(mut entity_commands) = commands.get_entity(entity) {
                        entity_commands.try_despawn();
                    }
                    // Preserve the public lifecycle event after the exact
                    // Entity has been retired. The incarnation token makes
                    // this delayed notification harmless if the ID is reused.
                    commands.trigger(SendMessageTrigger(Event::WindowDestroyed {
                        window_id,
                        source: crate::events::DestroySource::Reconciliation,
                        incarnation: Some(incarnation),
                    }));
                }
                LifecycleAction::Spawn(application, windows) => {
                    commands.trigger(SpawnWindowTrigger::for_application(application, windows));
                }
                LifecycleAction::RetireApplication(entity) => {
                    if let Ok(mut entity_commands) = commands.get_entity(entity) {
                        entity_commands.try_despawn();
                    }
                }
            }
        }
    }

    fn audit_frontmost_focus(
        &mut self,
        request: &SyncRequest,
        sync: &mut WindowStateSync,
        commands: &mut Commands,
    ) {
        let snapshot = self.focus.snapshot();
        let preferred_window = snapshot
            .requested_entity()
            .or_else(|| snapshot.confirmed_entity());
        let preferred_application = preferred_window.and_then(|entity| {
            self.windows
                .get_mut(entity)
                .ok()
                .map(|(_, _, parent, ..)| parent.parent())
        });
        let mut selected = None;
        for (app_entity, app) in &mut self.applications {
            let pid = app.pid();
            if !request.includes_application(pid) || !app.is_frontmost() {
                continue;
            }
            let candidate = (app_entity, pid, app.focused_window_id());
            if Some(app_entity) == preferred_application {
                selected = Some(candidate);
                break;
            }
            selected.get_or_insert(candidate);
        }
        let Some((app_entity, pid, focused_window)) = selected else {
            if request.all {
                sync.focus_absent.clear();
            }
            return;
        };
        if request.all {
            sync.focus_absent.retain(|entity, _| *entity == app_entity);
        }
        let window_id = match focused_window {
            Ok(window_id) => {
                sync.focus_absent.remove(&app_entity);
                window_id
            }
            Err(error)
                if matches!(error, Error::InvalidWindow)
                    || error.macos_code() == Some(kAXErrorNoValue) =>
            {
                let previous = sync.focus_absent.insert(app_entity, snapshot);
                if previous != Some(snapshot)
                    && snapshot.confirmed_entity().is_some()
                    && snapshot.needs_resolution(pid)
                {
                    debug!(pid, "window-state audit found no focused window");
                    commands.trigger(SendMessageTrigger(Event::FocusRevalidationRequested {
                        pid,
                        source: FocusSource::StateSync,
                    }));
                }
                return;
            }
            Err(error) => {
                debug!(pid, %error, "window-state audit could not observe focus");
                return;
            }
        };

        let tracked_entity = self
            .windows
            .iter_mut()
            .find_map(|(entity, window, parent, ..)| {
                (parent.parent() == app_entity && window.id() == window_id).then_some(entity)
            });
        if !self
            .focus
            .snapshot()
            .needs_revalidation(pid, window_id, tracked_entity)
        {
            return;
        }

        debug!(pid, window_id, "window-state audit found focus drift");
        commands.trigger(SendMessageTrigger(Event::FocusRevalidationRequested {
            pid,
            source: FocusSource::StateSync,
        }));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one audit pass keeps each window's observe, converge, and publish transaction contiguous"
    )]
    fn reconcile_frames(
        &mut self,
        request: &SyncRequest,
        audit: &LifecycleAudit,
        sync: &mut WindowStateSync,
        commands: &mut Commands,
        now: Duration,
    ) {
        let native_fullscreen_windows = self
            .workspaces
            .iter()
            .filter(|strip| strip.is_fullscreen())
            .flat_map(LayoutStrip::all_windows)
            .collect::<HashSet<_>>();
        // Notifications only invalidate the projection. Read the AX frame for
        // explicit signals and for every live window on the heartbeat.
        for (
            entity,
            mut window,
            _,
            unavailable,
            mut position,
            mut bounds,
            mut desired,
            mut presented,
            observed,
            floating,
            visibility,
            (defaults_pending, repositioning, resizing, presenting, space_reassignment_pending),
        ) in &mut self.windows
        {
            let window_id = window.id();
            if defaults_pending {
                sync.frame_convergence.remove(&entity);
                continue;
            }
            let unavailable = unavailable.is_some() && !audit.confirmed.contains(&entity);
            if should_skip_frame(request, audit, entity, window_id, unavailable) {
                if unavailable || audit.retiring.contains(&entity) {
                    sync.frame_convergence.remove(&entity);
                }
                if unavailable {
                    remove_observed_frame(entity, commands);
                }
                continue;
            }

            if space_reassignment_pending {
                sync.frame_convergence.remove(&entity);
                let Ok(frame) = window.update_frame().inspect_err(|error| {
                    warn!(window_id, %error, "unable to observe transitioning window frame");
                }) else {
                    remove_observed_frame(entity, commands);
                    continue;
                };
                update_floating_intent(
                    &mut position,
                    &mut bounds,
                    &mut desired,
                    &mut presented,
                    frame,
                );
                if let Some(mut observed) = observed {
                    if observed.0 != frame {
                        observed.0 = frame;
                    }
                } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_insert(ObservedWindowFrame(frame));
                }
                continue;
            }

            let Ok(mut frame) = window.update_frame().inspect_err(|error| {
                warn!(window_id, %error, "unable to observe window frame");
            }) else {
                remove_observed_frame(entity, commands);
                continue;
            };

            let native_fullscreen = native_fullscreen_windows.contains(&entity)
                || window.try_is_full_screen().unwrap_or(true);
            if native_fullscreen {
                sync.frame_convergence.remove(&entity);
            }
            let can_adopt_frame = visibility.is_none()
                && !repositioning
                && !resizing
                && !presenting
                && !native_fullscreen
                && !self.settling.contains(entity);
            if !floating && can_adopt_frame {
                let target = desired.0;
                let Some(confirmed) =
                    sync.converge_tiled_frame(entity, window_id, &mut window, frame, target, now)
                else {
                    // The AX setter may have partially succeeded even though
                    // it returned an error. Keep a fresh physical readback so
                    // overlays and diagnostics never fall back to stale or
                    // missing geometry while the bounded retry is pending.
                    match window.update_frame() {
                        Ok(readback) => {
                            if let Some(mut observed) = observed {
                                if observed.0 != readback {
                                    observed.0 = readback;
                                }
                            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                                entity_commands.try_insert(ObservedWindowFrame(readback));
                            }
                        }
                        Err(error) => {
                            warn!(window_id, %error, "unable to read back constrained tiled window frame");
                            remove_observed_frame(entity, commands);
                        }
                    }
                    continue;
                };
                frame = confirmed;
                if presented.0 != frame {
                    presented.0 = frame;
                }
                if frames_equivalent(frame, target)
                    && let Ok(mut entity_commands) = commands.get_entity(entity)
                {
                    entity_commands.try_remove::<WindowFrameCommitSuspended>();
                }
            } else if floating {
                sync.frame_convergence.remove(&entity);
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_remove::<WindowFrameCommitSuspended>();
                }
            }

            if let Some(mut observed) = observed {
                if observed.0 != frame {
                    observed.0 = frame;
                }
            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(ObservedWindowFrame(frame));
            }

            if floating && can_adopt_frame {
                update_floating_intent(
                    &mut position,
                    &mut bounds,
                    &mut desired,
                    &mut presented,
                    frame,
                );
            }
        }
    }
}

/// Reconciles the macOS window inventory with ECS. Query failures are
/// deliberately fail-open: absence is actionable only when both AX and the
/// full `WindowServer` session inventory succeeded.
pub(super) fn reconcile_windows(
    mut messages: MessageReader<Event>,
    mut state: ReconcileState,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    time: Res<Time>,
    mut sync: ResMut<WindowStateSync>,
    mut commands: Commands,
) {
    let heartbeat = sync.tick(time.delta());
    let request = SyncRequest::collect(&mut messages, heartbeat);
    if request.is_empty() {
        return;
    }

    let focus_audit_enabled = state.initializing.is_none() && !state.mission_control.0;
    let audit = state.audit_lifecycle(&request, &window_manager, &config, &mut sync, &mut commands);
    // Mouse-up and Space signals request a full lifecycle pass immediately,
    // while AX focus may still lag the direct interaction evidence. Audit
    // focus only after the heartbeat debounce or an application-scoped signal.
    if (heartbeat || !request.pids.is_empty()) && focus_audit_enabled {
        state.audit_frontmost_focus(&request, &mut sync, &mut commands);
    }
    state.reconcile_frames(&request, &audit, &mut sync, &mut commands, time.elapsed());
}

fn refresh_application_observer(entity: Entity, app: &mut Application, sync: &mut WindowStateSync) {
    let pid = app.pid();
    if sync.application_observers.contains(&entity) {
        return;
    }
    match app.observe() {
        Ok(true) => {
            sync.application_observers.insert(entity);
        }
        Ok(false) => {
            debug!(pid, "application observer registration remains incomplete");
        }
        Err(error) => {
            warn!(pid, %error, "unable to refresh application observers");
        }
    }
}

fn retire_application(
    application: Entity,
    windows: &mut Query<ReconcileWindowData>,
    sync: &mut WindowStateSync,
    audit: &mut LifecycleAudit,
) {
    debug!(
        ?application,
        "window-state audit found terminated application"
    );
    sync.forget_application(application);
    for (entity, _, parent, ..) in windows.iter_mut() {
        if parent.parent() != application || !audit.retiring.insert(entity) {
            continue;
        }
        sync.frame_convergence.remove(&entity);
        sync.window_observers.remove(&entity);
    }
    audit
        .actions
        .push(LifecycleAction::RetireApplication(application));
}

fn retire_tracked_window(
    application: Entity,
    key: WindowKey,
    entity: Entity,
    sync: &mut WindowStateSync,
    audit: &mut LifecycleAudit,
) {
    sync.retired_window_incarnations.insert((application, key));
    if audit.retiring.insert(entity) {
        sync.frame_convergence.remove(&entity);
        sync.window_observers.remove(&entity);
        audit.actions.push(LifecycleAction::Destroy(entity));
    }
}

fn audit_application_windows(
    inventory: ApplicationInventory<'_>,
    app: &mut Application,
    windows: &mut Query<ReconcileWindowData>,
    sync: &mut WindowStateSync,
    audit: &mut LifecycleAudit,
) -> HashMap<WindowKey, Entity> {
    let mut tracked = HashMap::new();
    for (entity, window, parent, unavailable, ..) in windows.iter_mut() {
        if parent.parent() != inventory.entity {
            continue;
        }
        let window_id = window.id();
        let key = WindowKey::new(&window);
        let surface_present = audit.contains_window(window_id, inventory.pid);
        tracked.insert(key, entity);
        if sync.is_key_retired(inventory.entity, key) {
            retire_tracked_window(inventory.entity, key, entity, sync, audit);
            continue;
        }
        let identities = inventory.buckets.get(&window_id);
        if identities.is_some_and(|keys| keys.len() > 1) {
            debug!(
                window_id,
                pid = inventory.pid,
                ?entity,
                "multiple AX incarnations claim one WindowServer ID; deferring selection"
            );
            request_suspension(audit, entity, unavailable.as_deref(), !surface_present);
            continue;
        }
        let observed_key = identities.and_then(|keys| keys.iter().next()).copied();
        let replaced = observed_key.is_some_and(|observed| observed != key);
        if replaced && inventory.complete {
            debug!(
                window_id,
                pid = inventory.pid,
                ?entity,
                "window ID now belongs to a new AX incarnation"
            );
            retire_tracked_window(inventory.entity, key, entity, sync, audit);
            continue;
        }
        if replaced {
            request_suspension(audit, entity, unavailable.as_deref(), !surface_present);
            continue;
        }
        match (observed_key == Some(key), surface_present) {
            (true, true) => {
                audit.confirmed.insert(entity);
                refresh_window_observer(entity, app, &window, sync);
                if let Some(unavailable) = unavailable.as_deref() {
                    audit.actions.push(LifecycleAction::Resume(
                        entity,
                        unavailable.layout_projection_invalidated,
                    ));
                }
            }
            (false, false) if inventory.complete => {
                debug!(
                    window_id,
                    pid = inventory.pid,
                    "AX and WindowServer inventories confirmed missing window"
                );
                retire_tracked_window(inventory.entity, key, entity, sync, audit);
            }
            (false, true) if !inventory.complete => {
                debug!(
                    window_id,
                    pid = inventory.pid,
                    "incomplete AX inventory omitted a live WindowServer surface"
                );
            }
            _ => {
                request_suspension(audit, entity, unavailable.as_deref(), !surface_present);
            }
        }
    }
    tracked
}

fn suspend_windows_missing_from_window_server(
    app_entity: Entity,
    pid: Pid,
    windows: &mut Query<ReconcileWindowData>,
    sync: &mut WindowStateSync,
    audit: &mut LifecycleAudit,
) {
    for (entity, window, parent, unavailable, ..) in windows.iter_mut() {
        if parent.parent() != app_entity
            || audit.contains_window(window.id(), pid)
            || unavailable.is_some()
        {
            continue;
        }
        sync.frame_convergence.remove(&entity);
        request_suspension(audit, entity, unavailable.as_deref(), true);
    }
}

fn request_suspension(
    audit: &mut LifecycleAudit,
    entity: Entity,
    unavailable: Option<&WindowUnavailable>,
    invalidate_layout_projection: bool,
) {
    let needs_transition = unavailable.is_none()
        || invalidate_layout_projection
            && unavailable.is_some_and(|state| !state.layout_projection_invalidated);
    if needs_transition {
        audit.actions.push(LifecycleAction::Suspend(
            entity,
            invalidate_layout_projection,
        ));
    }
}

fn refresh_window_observer(
    entity: Entity,
    app: &mut Application,
    window: &Window,
    sync: &mut WindowStateSync,
) {
    let window_id = window.id();
    if sync.window_observers.contains(&entity) {
        return;
    }
    match app.observe_window(window) {
        Ok(true) => {
            sync.window_observers.insert(entity);
        }
        Ok(false) => {
            debug!(window_id, "window observer registration remains incomplete");
        }
        Err(error) => {
            warn!(window_id, %error, "unable to refresh window observers");
        }
    }
}

fn should_skip_frame(
    request: &SyncRequest,
    audit: &LifecycleAudit,
    entity: Entity,
    window_id: WinID,
    unavailable: bool,
) -> bool {
    audit.retiring.contains(&entity)
        || unavailable
        || (!request.all
            && !request.frame_ids.contains(&window_id)
            && !audit.confirmed.contains(&entity))
        || (request.lifecycle_requested()
            && audit.window_server.is_some()
            && !audit.confirmed.contains(&entity))
}

fn remove_observed_frame(entity: Entity, commands: &mut Commands) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<ObservedWindowFrame>();
    }
}

fn update_floating_intent(
    position: &mut Position,
    bounds: &mut Bounds,
    desired: &mut DesiredWindowFrame,
    presented: &mut PresentedWindowFrame,
    frame: IRect,
) {
    if position.0 != frame.min {
        position.0 = frame.min;
    }
    if bounds.0 != frame.size() {
        bounds.0 = frame.size();
    }
    if desired.0 != frame {
        desired.0 = frame;
    }
    if presented.0 != frame {
        presented.0 = frame;
    }
}

pub(crate) fn frames_equivalent(left: IRect, right: IRect) -> bool {
    let origin_delta = (left.min - right.min).abs();
    let size_delta = (left.size() - right.size()).abs();
    origin_delta.max_element() <= 1 && size_delta.max_element() <= 1
}

fn suspend_window(
    entity: Entity,
    invalidate_layout_projection: bool,
    workspaces: &mut Query<&mut LayoutStrip, Without<Window>>,
    focus: &mut FocusCoordinator,
    commands: &mut Commands,
) {
    focus.observe(FocusSignal::Invalidated { entity });
    if invalidate_layout_projection {
        for mut strip in workspaces.iter_mut().filter(|strip| strip.contains(entity)) {
            strip.set_changed();
        }
    }
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(WindowUnavailable::new(invalidate_layout_projection));
        entity_commands.remove::<(
            RepositionMarker,
            ResizeMarker,
            WindowFrameMotion,
            ObservedWindowFrame,
        )>();
    }
    debug!(?entity, "suspended unavailable window operations and focus");
}

fn resume_window(
    entity: Entity,
    layout_projection_invalidated: bool,
    workspaces: &mut Query<&mut LayoutStrip, Without<Window>>,
    commands: &mut Commands,
) {
    if layout_projection_invalidated {
        for mut strip in workspaces.iter_mut().filter(|strip| strip.contains(entity)) {
            strip.set_changed();
        }
    }
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<WindowUnavailable>();
    }
    debug!(?entity, "resumed available window operations");
}

/// Rechecks only the owning application after the public CG surface has had a
/// short opportunity to disappear. This avoids a permanent global poll.
pub(super) fn confirm_unavailable_windows(
    time: Res<Time>,
    mut windows: Query<(&mut WindowUnavailable, &ChildOf)>,
    applications: Query<&Application>,
    mut commands: Commands,
) {
    let mut pids = HashSet::new();
    for (mut unavailable, parent) in &mut windows {
        unavailable.confirmation.tick(time.delta());
        if unavailable.confirmation.just_finished()
            && let Ok(app) = applications.get(parent.parent())
        {
            pids.insert(app.pid());
        }
    }
    for pid in pids {
        commands.trigger(SendMessageTrigger(Event::ReconcileWindows {
            scope: ReconcileScope::Application(pid),
        }));
    }
}

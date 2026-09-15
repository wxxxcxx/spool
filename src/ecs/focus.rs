use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::change_detection::DetectChanges as _;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::{Add, Remove};
use bevy::ecs::observer::On;
use bevy::ecs::query::{Added, Has, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, Populated, Query, Res, ResMut, Single, SystemParam};
use bevy::prelude::Event as BevyEvent;
use bevy::prelude::Time;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{Level, debug, instrument, trace, warn};

use super::{FocusedMarker, MouseHeldMarker, SystemTheme};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{ActiveDisplay, GlobalState, WindowCtx, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, RaiseWindow, Scrolling, SendMessageTrigger, SpawnCommandsExt,
    StrayFocusEvent,
};
use crate::events::{Event, FocusObservation};
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, WinID, WindowIncarnation, WorkspaceId};
use spool_shared_types::commands::FocusRole;

pub(crate) mod activation;
mod stacking;

#[derive(Default)]
struct FocusOrder(Vec<Entity>);

impl FocusOrder {
    fn record(&mut self, entity: Entity) {
        self.0.retain(|candidate| *candidate != entity);
        self.0.push(entity);
    }

    fn last(&self) -> Option<Entity> {
        self.0.last().copied()
    }

    fn newest_matching(&self, mut eligible: impl FnMut(Entity) -> bool) -> Option<Entity> {
        self.0
            .iter()
            .rev()
            .copied()
            .find(|entity| eligible(*entity))
    }

    fn forget(&mut self, entity: Entity) {
        self.0.retain(|candidate| *candidate != entity);
    }
}

#[derive(Default)]
struct TierMemory {
    preference: Option<Entity>,
    selection: Option<Entity>,
    any: FocusOrder,
    tiled: FocusOrder,
    floating: FocusOrder,
}

/// One Space's focus memory as the live entities it names. Entities never leave
/// the process, so a saver resolves them to cached identity hints and an import
/// resolves cached hints back to entities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusMemory {
    pub space_id: WorkspaceId,
    pub preference: Option<Entity>,
    pub selection: Option<Entity>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ObservedFocus {
    #[default]
    Unresolved,
    Tracked {
        entity: Entity,
        window_id: WinID,
    },
    Untracked {
        pid: Option<Pid>,
        window_id: Option<WinID>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FocusResolution {
    generation: u64,
    activation_version: u64,
    pid: Option<Pid>,
    candidate: Option<WinID>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusRequest {
    pub entity: Entity,
    pub window_id: WinID,
    pub pid: Pid,
    pub incarnation: WindowIncarnation,
    pub workspace: WorkspaceId,
    pub version: u64,
    pub kind: FocusRequestKind,
    pub raise: bool,
    pub allow_native_activation: bool,
    pub execution_grant: Option<u64>,
    pub qualification_generation: Option<u64>,
    pub blocker: Option<&'static str>,
    pub submitted: Option<Duration>,
    pub probes: usize,
    pub conflict: Option<(Pid, WinID, WindowIncarnation, Duration)>,
    pub submission_failed: bool,
    pub outcome: ActivationOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivationOutcome {
    Accepted,
    Blocked,
    Unconfirmed,
    Confirmed,
    Yielded,
    Invalidated,
}

impl FocusRequest {
    fn pending(self) -> bool {
        matches!(
            self.outcome,
            ActivationOutcome::Accepted
                | ActivationOutcome::Blocked
                | ActivationOutcome::Unconfirmed
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusSnapshot {
    generation: u64,
    observed: ObservedFocus,
    resolving: Option<FocusResolution>,
    requested: Option<FocusRequest>,
}

impl FocusSnapshot {
    pub(crate) fn confirmed_entity(self) -> Option<Entity> {
        match self.observed {
            ObservedFocus::Tracked { entity, .. } => Some(entity),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn confirmed_window_id(self) -> Option<WinID> {
        match self.observed {
            ObservedFocus::Tracked { window_id, .. } => Some(window_id),
            _ => None,
        }
    }

    pub(crate) fn requested_entity(self) -> Option<Entity> {
        self.requested
            .filter(|request| request.pending())
            .map(|request| request.entity)
    }

    pub(crate) fn needs_revalidation(
        self,
        pid: Pid,
        window_id: WinID,
        tracked_entity: Option<Entity>,
    ) -> bool {
        if tracked_entity.is_some() && self.requested_entity() == tracked_entity {
            return true;
        }

        if self.resolving.is_some_and(|resolution| {
            resolution.pid == Some(pid)
                && resolution
                    .candidate
                    .is_none_or(|candidate_id| candidate_id == window_id)
        }) {
            return false;
        }

        !match self.observed {
            ObservedFocus::Tracked {
                entity,
                window_id: observed_id,
            } => tracked_entity == Some(entity) && observed_id == window_id,
            ObservedFocus::Untracked {
                pid: known_pid,
                window_id: Some(known_window_id),
            } => tracked_entity.is_none() && known_pid == Some(pid) && known_window_id == window_id,
            ObservedFocus::Unresolved | ObservedFocus::Untracked { .. } => false,
        }
    }

    pub(crate) fn needs_resolution(self, pid: Pid) -> bool {
        self.resolving
            .is_none_or(|resolution| resolution.pid != Some(pid))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusSignal {
    Resolve {
        pid: Option<Pid>,
        candidate: Option<WinID>,
    },
    Candidate {
        generation: Option<u64>,
        pid: Option<Pid>,
        window_id: WinID,
    },
    Tracked {
        generation: u64,
        entity: Entity,
        window_id: WinID,
    },
    Untracked {
        generation: Option<u64>,
        pid: Option<Pid>,
        window_id: Option<WinID>,
    },
    Unresolved {
        generation: u64,
    },
    Invalidated {
        entity: Entity,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusUpdate {
    Started(u64),
    Accepted(u64),
    Stale,
}

impl FocusUpdate {
    pub fn generation(self) -> Option<u64> {
        match self {
            Self::Started(generation) | Self::Accepted(generation) => Some(generation),
            Self::Stale => None,
        }
    }

    pub fn accepted(self) -> bool {
        !matches!(self, Self::Stale)
    }
}

/// Owns the distinction between macOS-confirmed focus, an explicit in-flight
/// request, and the last tracked window used for keyboard navigation.
#[derive(Default, Resource)]
pub struct FocusCoordinator {
    generation: u64,
    activation_version: u64,
    uncertain_effects: HashSet<(Pid, WinID, WindowIncarnation)>,
    observed: ObservedFocus,
    resolving: Option<FocusResolution>,
    requested: Option<FocusRequest>,
    by_workspace: HashMap<WorkspaceId, TierMemory>,
}

impl FocusCoordinator {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    fn begin_resolution(&mut self, pid: Option<Pid>, candidate: Option<WinID>) -> u64 {
        let generation = self.next_generation();
        self.resolving = Some(FocusResolution {
            generation,
            activation_version: self.activation_version,
            pid,
            candidate,
        });
        self.generation
    }

    fn accepts(&self, generation: u64, pid: Option<Pid>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.resolving.is_some_and(|resolution| {
            resolution.generation == generation
                && (resolution.pid.is_none() || pid.is_none() || resolution.pid == pid)
        })
    }

    pub(super) fn observe(&mut self, signal: FocusSignal) -> FocusUpdate {
        let before = self.snapshot();
        let result = self.apply_signal(signal);
        if before != self.snapshot() {
            debug!(target: "spool::focus_diagnostics", ?signal, ?result, ?before,
                after = ?self.snapshot(), "focus_observation");
        }
        result
    }

    fn apply_signal(&mut self, signal: FocusSignal) -> FocusUpdate {
        match signal {
            FocusSignal::Resolve { pid, candidate } => {
                FocusUpdate::Started(self.begin_resolution(pid, candidate))
            }
            FocusSignal::Candidate {
                generation,
                pid,
                window_id,
            } => {
                let generation =
                    generation.unwrap_or_else(|| self.begin_resolution(pid, Some(window_id)));
                if !self.accepts(generation, pid) {
                    return FocusUpdate::Stale;
                }
                self.resolving = Some(FocusResolution {
                    generation,
                    activation_version: self
                        .resolving
                        .map_or(self.activation_version, |resolution| {
                            resolution.activation_version
                        }),
                    pid,
                    candidate: Some(window_id),
                });
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Tracked {
                generation,
                entity,
                window_id,
            } => {
                if !self.accepts(generation, None) {
                    return FocusUpdate::Stale;
                }
                self.observed = ObservedFocus::Tracked { entity, window_id };
                if let Some(request) = &mut self.requested
                    && request
                        .conflict
                        .is_some_and(|(_, old_id, _, _)| old_id != window_id)
                {
                    request.conflict = None;
                }
                if let Some(request) = &mut self.requested
                    && request.pending()
                    && request.entity == entity
                    && request.window_id == window_id
                    && self
                        .resolving
                        .is_some_and(|resolution| resolution.activation_version == request.version)
                {
                    request.outcome = ActivationOutcome::Confirmed;
                }
                self.resolving = None;
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Untracked {
                generation,
                pid,
                window_id,
            } => {
                let generation =
                    generation.unwrap_or_else(|| self.begin_resolution(pid, window_id));
                if !self.accepts(generation, pid) {
                    return FocusUpdate::Stale;
                }
                self.observed = ObservedFocus::Untracked { pid, window_id };
                if let Some(request) = &mut self.requested
                    && request.conflict.is_some_and(|(old_pid, old_id, _, _)| {
                        pid != Some(old_pid) || window_id != Some(old_id)
                    })
                {
                    request.conflict = None;
                }
                self.resolving = None;
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Unresolved { generation } => {
                if !self.accepts(generation, None) {
                    return FocusUpdate::Stale;
                }
                self.observed = ObservedFocus::Unresolved;
                self.resolving = None;
                if let Some(request) = &mut self.requested {
                    request.conflict = None;
                }
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Invalidated { entity } => self.invalidate(entity),
        }
    }

    fn invalidate(&mut self, entity: Entity) -> FocusUpdate {
        let observed_matches = matches!(
            self.observed,
            ObservedFocus::Tracked {
                entity: focused, ..
            } if focused == entity
        );
        let requested_matches = self
            .requested
            .is_some_and(|request| request.entity == entity);
        if !observed_matches && !requested_matches {
            return FocusUpdate::Stale;
        }
        let generation = self.next_generation();
        self.resolving = None;
        if observed_matches {
            self.observed = ObservedFocus::Unresolved;
        }
        if requested_matches && let Some(request) = &mut self.requested {
            request.outcome = ActivationOutcome::Invalidated;
        }
        FocusUpdate::Accepted(generation)
    }

    #[cfg(test)]
    fn request(&mut self, entity: Entity) -> u64 {
        self.admit(FocusRequest {
            entity,
            window_id: 0,
            pid: 0,
            incarnation: 0,
            workspace: 1,
            version: 0,
            kind: FocusRequestKind::Explicit,
            raise: true,
            allow_native_activation: false,
            execution_grant: None,
            qualification_generation: None,
            blocker: None,
            submitted: None,
            probes: 0,
            conflict: None,
            submission_failed: false,
            outcome: ActivationOutcome::Accepted,
        })
    }

    fn admit(&mut self, mut request: FocusRequest) -> u64 {
        if let Some(previous) = self
            .requested
            .filter(|previous| previous.submitted.is_some() && previous.pending())
        {
            self.uncertain_effects
                .insert((previous.pid, previous.window_id, previous.incarnation));
        }
        self.activation_version = self.activation_version.wrapping_add(1).max(1);
        request.version = self.activation_version;
        let memory = self.by_workspace.entry(request.workspace).or_default();
        memory.preference = Some(request.entity);
        memory.selection = Some(request.entity);
        self.requested = Some(request);
        self.activation_version
    }

    /// Losing access is not evidence of destruction and does not cancel intent.
    pub(super) fn suspend(&mut self, entity: Entity) {
        if self.snapshot().confirmed_entity() == Some(entity) {
            self.next_generation();
            self.observed = ObservedFocus::Unresolved;
            self.resolving = None;
        }
    }

    pub(crate) fn snapshot(&self) -> FocusSnapshot {
        FocusSnapshot {
            generation: self.generation,
            observed: self.observed,
            resolving: self.resolving,
            requested: self.requested,
        }
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    pub(super) fn record_navigation(
        &mut self,
        workspace: WorkspaceId,
        entity: Entity,
        floating: bool,
        visible: bool,
    ) {
        if !visible {
            return;
        }
        let pending_in_space = self
            .requested
            .is_some_and(|request| request.pending() && request.workspace == workspace);
        let memory = self.by_workspace.entry(workspace).or_default();
        if !pending_in_space {
            memory.selection = Some(entity);
        }
        memory.any.record(entity);
        if floating {
            memory.floating.record(entity);
        } else {
            memory.tiled.record(entity);
        }
    }

    pub(crate) fn last_tiled(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)
            .and_then(|memory| memory.tiled.last())
    }

    pub(crate) fn last_tiled_matching(
        &self,
        workspace: WorkspaceId,
        eligible: impl FnMut(Entity) -> bool,
    ) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)?
            .tiled
            .newest_matching(eligible)
    }

    pub(crate) fn last_floating(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)
            .and_then(|memory| memory.floating.last())
    }

    pub(crate) fn space_selection(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace.get(&workspace)?.selection
    }

    pub(crate) fn navigation_entity(&self, workspace: WorkspaceId) -> Option<Entity> {
        let memory = self.by_workspace.get(&workspace)?;
        memory.selection.or_else(|| memory.any.last())
    }

    pub(crate) fn restoration_entity(
        &self,
        workspace: WorkspaceId,
        eligible: impl FnMut(Entity) -> bool,
    ) -> Option<Entity> {
        let memory = self.by_workspace.get(&workspace)?;
        let mut eligible = eligible;
        memory
            .preference
            .filter(|entity| eligible(*entity))
            .or_else(|| memory.any.newest_matching(eligible))
    }

    pub(super) fn forget(&mut self, entity: Entity) {
        for memory in self.by_workspace.values_mut() {
            if memory.preference == Some(entity) {
                memory.preference = None;
            }
            if memory.selection == Some(entity) {
                memory.selection = None;
            }
            memory.tiled.forget(entity);
            memory.floating.forget(entity);
            memory.any.forget(entity);
        }
    }

    pub(super) fn forget_workspace(&mut self, workspace: WorkspaceId) {
        self.by_workspace.remove(&workspace);
    }

    /// Every Space's recorded focus memory, for a saver to resolve into cached
    /// identity hints. A Space with neither a preference nor a selection carries
    /// no intent to save.
    pub(crate) fn focus_memory(&self) -> impl Iterator<Item = FocusMemory> + '_ {
        self.by_workspace.iter().filter_map(|(space, memory)| {
            (memory.preference.is_some() || memory.selection.is_some()).then_some(FocusMemory {
                space_id: *space,
                preference: memory.preference,
                selection: memory.selection,
            })
        })
    }

    /// Records the focus memory a trusted import names.
    ///
    /// This is the authored transition, like [`set_space_preference`]: it
    /// creates no activation request, advances no observation, leaves confirmed
    /// history alone, and writes nothing to the platform.
    pub(crate) fn import_focus_memory(
        &mut self,
        workspace: WorkspaceId,
        role: FocusRole,
        entity: Entity,
    ) {
        let memory = self.by_workspace.entry(workspace).or_default();
        match role {
            FocusRole::Preference => memory.preference = Some(entity),
            FocusRole::Selection => memory.selection = Some(entity),
        }
    }
}

pub struct FocusEventsPlugin;

impl Plugin for FocusEventsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FocusCoordinator>();
        app.init_resource::<stacking::TiledStackingState>();
        app.add_systems(
            PostUpdate,
            (
                reconcile_activation,
                activation::verify_activation,
                project_confirmed_focus,
                reconcile_space_selections
                    .before(super::systems::update_overlays)
                    .before(crate::bar::update_bar),
                autocenter_window_on_focus.after(super::systems::animate_resize_entities),
                mouse_follows_focus.after(super::systems::animate_resize_entities),
                stacking::reconcile_tiled_stacking.after(super::systems::commit_window_frame),
            )
                .chain(),
        );
        app.add_observer(dim_remove_window_trigger)
            .add_observer(dim_window_trigger)
            .add_observer(maintain_focus_singleton)
            .add_observer(virtual_strip_activated)
            .add_observer(stray_focus_observer)
            .add_observer(focus_window_trigger)
            .add_observer(raise_window_trigger);
    }
}

#[derive(BevyEvent)]
pub(super) struct FocusWindow {
    pub entity: Entity,
    pub raise: bool,
    pub kind: FocusRequestKind,
    pub allow_native_activation: bool,
    pub native_space: Option<WorkspaceId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusRequestKind {
    Explicit,
    Automatic,
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn maintain_focus_singleton(_trigger: On<Add, FocusedMarker>, mut config: GlobalState) {
    // Check if the reshuffle was caused by a keyboard switch or mouse move.
    // Skip reshuffle if caused by mouse - because then it won't center.
    if config.ffm_flag().is_none() {
        config.set_skip_reshuffle(false);
    }
    config.set_ffm_flag(None);
}

type SelectionWindows<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Window,
        Option<&'static super::WindowVisibility>,
        Option<&'static super::reconcile::WindowUnavailable>,
        Has<super::Floating>,
    ),
>;

/// Lifecycle edits only the local selection; no activation or history writes.
fn reconcile_space_selections(
    mut focus: ResMut<FocusCoordinator>,
    windows: SelectionWindows,
    strips: Query<&LayoutStrip>,
    manager: Res<WindowManager>,
) {
    let mut updates = Vec::new();
    for (&space, memory) in &focus.by_workspace {
        if memory.selection.is_some_and(|entity| {
            windows
                .get(entity)
                .is_ok_and(|(_, _, visibility, _, _)| visibility.is_none())
        }) {
            // A suspended AX endpoint is not proof the retained choice vanished.
            continue;
        }
        let strip = strips.iter().find(|strip| strip.id() == space);
        let mut native = None;
        let next = memory.any.newest_matching(|entity| {
            let Ok((_, window, visibility, unavailable, floating)) = windows.get(entity) else {
                return false;
            };
            if visibility.is_some() || unavailable.is_some() {
                return false;
            }
            if strip.is_some_and(|strip| strip.contains(entity)) {
                return true;
            }
            if !floating {
                return false;
            }
            native
                .get_or_insert_with(|| manager.windows_in_workspace(space).ok())
                .as_ref()
                .is_some_and(|ids| ids.contains(&window.id()))
        });
        if next != memory.selection {
            updates.push((space, next));
        }
    }
    if !updates.is_empty() {
        for (space, next) in updates {
            if let Some(memory) = focus.by_workspace.get_mut(&space) {
                memory.selection = next;
            }
        }
    }
}

pub(crate) fn project_confirmed_focus(
    coordinator: Res<FocusCoordinator>,
    windows: Query<(Entity, Has<FocusedMarker>), With<Window>>,
    mut commands: Commands,
) {
    if !coordinator.is_changed() {
        return;
    }

    let target = coordinator.snapshot().confirmed_entity();
    let mut target_has_marker = false;
    for (entity, focused) in &windows {
        if Some(entity) == target {
            target_has_marker = focused;
        } else if focused && let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<FocusedMarker>();
        }
    }
    if let Some(entity) = target
        && !target_has_marker
        && let Ok(mut entity_commands) = commands.get_entity(entity)
    {
        entity_commands.try_insert(FocusedMarker);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn autocenter_window_on_focus(
    focused: Single<Entity, Added<FocusedMarker>>,
    mouse_held: Query<&MouseHeldMarker>,
    global_state: GlobalState,
    active_display: ActiveDisplay,
    mut ctx: WindowCtx,
) {
    let entity = *focused;

    if global_state.skip_reshuffle() || global_state.initializing() || !mouse_held.is_empty() {
        return;
    }
    if active_display.active_strip().tabbed(entity) {
        return;
    }
    if ctx.config.auto_center()
        && ctx
            .windows
            .get_tracked(entity)
            .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
        && let Some(size) = ctx.windows.size(entity)
        && let Some(mut origin) = ctx.windows.origin(entity)
    {
        let center = active_display.bounds().center();
        origin.x = center.x - size.x / 2;
        ctx.commands.reposition_entity(entity, origin);
    }
    ctx.commands.reshuffle_around(entity);
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn mouse_follows_focus(
    focused: Single<Entity, Added<FocusedMarker>>,
    windows: Windows,
    global_state: GlobalState,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
    displays: Query<&Display>,
    workspaces: Query<(
        &LayoutStrip,
        &ChildOf,
        Option<&Scrolling>,
        Has<ActiveWorkspaceMarker>,
    )>,
) {
    let entity = *focused;
    let Some(window) = windows.get(entity) else {
        return;
    };
    if workspaces
        .iter()
        .find_map(|(_, _, scrolling, active)| if active { scrolling } else { None })
        .is_some_and(|scrolling| scrolling.is_user_swiping)
    {
        debug!("Suppressing center mouse due to a swipe");
        return;
    }

    trace!(
        "window {}, skip_reshuffle {}, ffm flag {:?}.",
        window.id(),
        global_state.skip_reshuffle(),
        global_state.ffm_flag()
    );
    if config.mouse_follows_focus()
        && !global_state.skip_reshuffle()
        && global_state.ffm_flag().is_none_or(|id| id != window.id())
        && let Some(frame) = windows.moving_frame(entity)
        && let Some(display_bounds) = workspaces
            .into_iter()
            .find_map(|(strip, child, _, _)| strip.contains(entity).then_some(child))
            .and_then(|child| displays.get(child.parent()).ok())
            .map(Display::bounds)
    {
        let visible = display_bounds.intersect(frame);
        // If the overlap is smaller than 50x50, the window is probably hidden
        // off screen, so do not move the mouse.
        if visible.size().length_squared() > 5000 {
            let origin = visible.center();
            debug!("centering on {} {origin}", window.id());
            window_manager.warp_mouse(origin);
        }
    }
}

fn dim_window_trigger(
    trigger: On<Add, FocusedMarker>,
    windows: Windows,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    theme: Option<Res<SystemTheme>>,
) {
    let Some(window) = windows.get(trigger.event().entity) else {
        return;
    };

    let dark = theme.is_some_and(|theme| theme.is_dark);
    if config.window_dim_ratio(dark).is_some() {
        window_manager.dim_windows(&[window.id()], 0.0);
    }
}

fn dim_remove_window_trigger(
    trigger: On<Remove, FocusedMarker>,
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    theme: Option<Res<SystemTheme>>,
) {
    let Some((window, _, state)) = windows.get_tracked(trigger.event().entity) else {
        return;
    };
    if !state.is_visible() {
        return;
    }

    let active_strip = active_display.active_strip();
    let same_space = if state.is_floating() {
        window_manager
            .windows_in_workspace(active_strip.id())
            .is_ok_and(|ids| ids.contains(&window.id()))
    } else {
        active_strip.contains(trigger.event().entity)
    };
    if !same_space {
        // Do not dim the window losing focus on another display or Space.
        return;
    }

    let dark = theme.is_some_and(|theme| theme.is_dark);
    if let Some(dim_ratio) = config.window_dim_ratio(dark) {
        window_manager.dim_windows(&[window.id()], dim_ratio);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn virtual_strip_activated(
    trigger: On<Add, FocusedMarker>,
    workspaces: Query<(Entity, &LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    mut commands: Commands,
) {
    let owner_strip = workspaces.into_iter().find_map(|(entity, strip, active)| {
        (strip.contains(trigger.entity) && !active).then_some(entity)
    });
    if let Some(entity) = owner_strip
        && let Ok(mut entity_commands) = commands.get_entity(entity)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }
}

#[derive(SystemParam)]
struct FocusAdmission<'w, 's> {
    windows: Windows<'w, 's>,
    workspaces: Query<'w, 's, &'static LayoutStrip>,
    apps: Query<'w, 's, &'static Application>,
    manager: Res<'w, WindowManager>,
    topology: ResMut<'w, super::topology::NativeTopology>,
    transactions: ResMut<'w, super::native_space::NativeSpaceTransactions>,
}

fn focus_window_trigger(
    trigger: On<FocusWindow>,
    admission: FocusAdmission,
    mut focus: ResMut<FocusCoordinator>,
    mut commands: Commands,
) {
    let FocusAdmission {
        windows,
        workspaces,
        apps,
        manager,
        mut topology,
        mut transactions,
    } = admission;
    let FocusWindow {
        entity,
        raise,
        kind,
        allow_native_activation,
        native_space,
    } = *trigger.event();
    let Some((window, _, parent)) = windows.get_parent_any(entity) else {
        return;
    };
    let Ok(app) = apps.get(parent) else {
        return;
    };
    let workspace = native_space
        .or_else(|| {
            workspaces
                .iter()
                .find(|strip| strip.contains(entity))
                .map(LayoutStrip::id)
        })
        .or_else(|| topology.observe_visible_window_space(&manager, window.id()));
    let Some(workspace) = workspace else {
        return;
    };
    // Repeated automatic restoration is not new explicit input. In particular,
    // it cannot revive an attempted or terminal activation of this instance.
    if kind == FocusRequestKind::Automatic
        && focus.requested.is_some_and(|request| {
            request.entity == entity
                && request.incarnation == window.incarnation()
                && request.outcome != ActivationOutcome::Confirmed
        })
    {
        return;
    }
    if kind == FocusRequestKind::Explicit {
        transactions.cancel_pending_follows();
    }
    focus.admit(FocusRequest {
        entity,
        window_id: window.id(),
        pid: app.pid(),
        incarnation: window.incarnation(),
        workspace,
        version: 0,
        kind,
        raise,
        allow_native_activation,
        execution_grant: native_space.map(|_| topology.generation()),
        qualification_generation: None,
        blocker: None,
        submitted: None,
        probes: 0,
        conflict: None,
        submission_failed: false,
        outcome: ActivationOutcome::Accepted,
    });
    // The same coordinator serves immediate commands and deferred eligibility.
    commands.run_system_cached(reconcile_activation);
}

#[derive(SystemParam)]
pub(crate) struct ActivationEffects<'w, 's> {
    windows: Windows<'w, 's>,
    apps: Query<'w, 's, &'static Application>,
    manager: Res<'w, WindowManager>,
    topology: ResMut<'w, super::topology::NativeTopology>,
    mission_control: Res<'w, super::MissionControlActive>,
    transactions: Res<'w, super::native_space::NativeSpaceTransactions>,
}

pub(crate) fn reconcile_activation(
    mut focus: ResMut<FocusCoordinator>,
    mut effects: ActivationEffects,
    time: Res<Time>,
) {
    let Some(request) = focus.requested.filter(|request| request.pending()) else {
        return;
    };
    let Some((window, _, parent)) = effects.windows.get_parent_any(request.entity) else {
        focus.observe(FocusSignal::Invalidated {
            entity: request.entity,
        });
        return;
    };
    if window.id() != request.window_id || window.incarnation() != request.incarnation {
        focus.observe(FocusSignal::Invalidated {
            entity: request.entity,
        });
        return;
    }
    if request.submitted.is_some() {
        return;
    }
    let eligible = !effects.mission_control.0 && effects.windows.get(request.entity).is_some();
    if !eligible {
        if let Some(current) = &mut focus.requested {
            current.outcome = ActivationOutcome::Blocked;
            current.execution_grant = None;
            current.qualification_generation = None;
            current.blocker = Some(if effects.mission_control.0 {
                "mission_control"
            } else {
                "window_unavailable"
            });
        }
        return;
    }
    // Retry qualification on new topology evidence, not once per render frame.
    if request.qualification_generation == Some(effects.topology.generation()) {
        return;
    }
    if !request.allow_native_activation
        && request.execution_grant != Some(effects.topology.generation())
        && !matches!(
            effects.topology.confirm_visible_window_space(
                &effects.manager,
                request.window_id,
                request.workspace
            ),
            super::topology::SpaceClaim::Confirmed
        )
    {
        if let Some(current) = &mut focus.requested {
            current.outcome = ActivationOutcome::Blocked;
            current.execution_grant = None;
            current.qualification_generation = Some(effects.topology.generation());
            current.blocker = Some("visible_membership_unconfirmed");
        }
        return;
    }
    let Ok(app) = effects.apps.get(parent) else {
        if let Some(current) = &mut focus.requested {
            current.outcome = ActivationOutcome::Blocked;
            current.blocker = Some("application_unavailable");
        }
        return;
    };
    // Reserve before the first native effect, including partial failures.
    if let Some(current) = &mut focus.requested {
        current.submitted = Some(time.elapsed());
        current.blocker = None;
        current.outcome = ActivationOutcome::Unconfirmed;
    }
    let psn = app.psn();
    let result = if !request.raise
        && let Some((focused_window, focused_entity)) = effects.windows.focused()
        && let Some((_, _, focused_parent)) = effects.windows.get_parent(focused_entity)
        && let Ok(focused_app) = effects.apps.get(focused_parent)
    {
        window.focus_without_raise(psn, focused_window, focused_app.psn())
    } else {
        window.focus_with_raise(psn)
    };
    if let Err(error) = result {
        warn!(window_id = request.window_id, %error, "activation submission failed; only readback remains");
        if let Some(current) = &mut focus.requested {
            current.submission_failed = true;
        }
    }
}

fn raise_window_trigger(
    trigger: On<RaiseWindow>,
    stacking: stacking::TiledStacking,
    mut state: ResMut<stacking::TiledStackingState>,
) {
    let RaiseWindow { entity, with_strip } = *trigger.event();
    if with_strip {
        stacking.raise_strip(entity, &mut state, true);
    } else {
        stacking.raise_one(entity);
    }
}

pub(super) fn stray_focus_observer(
    trigger: On<Add, Window>,
    focus_events: Populated<(Entity, &StrayFocusEvent)>,
    windows: Windows,
    applications: Query<&Application>,
    mut commands: Commands,
) {
    let entity = trigger.event().entity;
    let Some((window, _, parent)) = windows.get_parent(entity) else {
        return;
    };
    let Ok(app) = applications.get(parent) else {
        return;
    };
    let window_id = window.id();
    let incarnation = window.incarnation();
    let pid = app.pid();

    focus_events
        .iter()
        .filter(|(_, stray_focus)| {
            stray_focus.0.window_id == window_id
                && stray_focus.0.pid.is_none_or(|expected| expected == pid)
                && stray_focus
                    .0
                    .incarnation
                    .is_none_or(|expected| expected == incarnation)
        })
        .for_each(|(timeout_entity, stray_focus)| {
            debug!("Re-queueing lost focus event for window id {window_id}.");
            commands.trigger(SendMessageTrigger(Event::WindowFocused(FocusObservation {
                incarnation: Some(incarnation),
                ..stray_focus.0
            })));
            if let Ok(mut entity_commands) = commands.get_entity(timeout_entity) {
                entity_commands.try_despawn();
            }
        });
}

/// State-only admission: background preference never creates an activation.
pub(crate) fn set_space_preference(
    bevy::prelude::In((workspace, window_id)): bevy::prelude::In<(WorkspaceId, WinID)>,
    workspaces: Query<&LayoutStrip>,
    windows: Query<(
        Entity,
        &Window,
        Has<super::Floating>,
        Option<&super::PreviousTiledStrip>,
    )>,
    manager: Res<WindowManager>,
    mut focus: ResMut<FocusCoordinator>,
) -> crate::errors::Result<()> {
    let strip = workspaces
        .iter()
        .find(|strip| strip.id() == workspace)
        .ok_or_else(|| crate::errors::Error::rejected("space_not_found"))?;
    let mut matches = windows
        .iter()
        .filter(|(_, window, _, _)| window.id() == window_id);
    let (entity, _, floating, previous) = matches
        .next()
        .ok_or_else(|| crate::errors::Error::rejected("window_not_found"))?;
    if matches.next().is_some() {
        return Err(crate::errors::Error::rejected("ambiguous_window_identity"));
    }
    let retained = strip.contains(entity)
        || previous.is_some_and(|previous| previous.workspace_id == workspace);
    if !(retained
        || (floating
            && manager
                .windows_in_workspace(workspace)?
                .contains(&window_id)))
    {
        return Err(crate::errors::Error::rejected("window_not_in_space"));
    }
    let memory = focus.by_workspace.entry(workspace).or_default();
    memory.preference = Some(entity);
    memory.selection = Some(entity);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::World;

    #[test]
    fn unresolved_observation_preserves_the_activation_request() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        focus.request(target);
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: None,
                candidate: None,
            })
            .generation()
            .unwrap();
        focus.observe(FocusSignal::Unresolved { generation });
        assert_eq!(focus.snapshot().requested_entity(), Some(target));
    }

    #[test]
    fn newly_tracked_identity_revalidates_previously_untracked_focus() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        focus.observe(FocusSignal::Untracked {
            generation: None,
            pid: Some(836),
            window_id: Some(325),
        });
        assert!(!focus.snapshot().needs_revalidation(836, 325, None));
        assert!(focus.snapshot().needs_revalidation(836, 325, Some(entity)));
    }

    #[test]
    fn record_and_read_per_tier() {
        let mut world = World::new();
        let tiled = world.spawn(()).id();
        let floating = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();

        focus.record_navigation(1, tiled, false, true);
        focus.record_navigation(1, floating, true, true);

        assert_eq!(focus.last_tiled(1), Some(tiled));
        assert_eq!(focus.last_floating(1), Some(floating));
    }

    #[test]
    fn record_ignores_minimized_and_hidden() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();

        focus.record_navigation(1, entity, false, false);
        focus.record_navigation(1, entity, true, false);

        assert_eq!(focus.last_tiled(1), None);
        assert_eq!(focus.last_floating(1), None);
    }

    #[test]
    fn per_workspace_isolation() {
        let mut world = World::new();
        let a = world.spawn(()).id();
        let b = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();

        focus.record_navigation(1, a, false, true);
        focus.record_navigation(2, b, false, true);

        assert_eq!(focus.last_tiled(1), Some(a));
        assert_eq!(focus.last_tiled(2), Some(b));
    }

    #[test]
    fn forget_clears_entity_across_workspaces() {
        let mut world = World::new();
        let target = world.spawn(()).id();
        let other = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();

        focus.record_navigation(1, target, false, true);
        focus.record_navigation(2, target, true, true);
        focus.record_navigation(2, other, false, true);

        focus.forget(target);

        assert_eq!(focus.last_tiled(1), None);
        assert_eq!(focus.last_floating(2), None);
        assert_eq!(focus.last_tiled(2), Some(other));
    }

    #[test]
    fn forget_workspace_drops_entry() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();

        focus.record_navigation(1, entity, false, true);
        focus.forget_workspace(1);

        assert_eq!(focus.last_tiled(1), None);
    }

    #[test]
    fn invalidating_requested_target_preserves_confirmed_focus() {
        let mut world = World::new();
        let confirmed = world.spawn(()).id();
        let requested = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: None,
                candidate: Some(10),
            })
            .generation()
            .unwrap();
        focus.observe(FocusSignal::Tracked {
            generation,
            entity: confirmed,
            window_id: 10,
        });
        focus.request(requested);

        focus.observe(FocusSignal::Invalidated { entity: requested });

        assert_eq!(focus.snapshot().confirmed_entity(), Some(confirmed));
        assert_eq!(focus.snapshot().requested_entity(), None);
    }

    #[test]
    fn resolving_a_new_focus_preserves_the_confirmed_window_until_observed() {
        let mut world = World::new();
        let confirmed = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: Some(100),
                candidate: Some(10),
            })
            .generation()
            .unwrap();
        focus.observe(FocusSignal::Tracked {
            generation,
            entity: confirmed,
            window_id: 10,
        });

        focus.observe(FocusSignal::Resolve {
            pid: Some(100),
            candidate: Some(11),
        });

        assert_eq!(
            focus.snapshot().confirmed_entity(),
            Some(confirmed),
            "an in-flight AX query is not evidence that the old confirmed focus disappeared"
        );
        assert_eq!(focus.snapshot().confirmed_window_id(), Some(10));
    }

    #[test]
    fn invalidating_confirmed_focus_preserves_another_request() {
        let mut world = World::new();
        let confirmed = world.spawn(()).id();
        let requested = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: None,
                candidate: Some(10),
            })
            .generation()
            .unwrap();
        focus.observe(FocusSignal::Tracked {
            generation,
            entity: confirmed,
            window_id: 10,
        });
        focus.request(requested);

        focus.observe(FocusSignal::Invalidated { entity: confirmed });

        assert_eq!(focus.snapshot().confirmed_entity(), None);
        assert_eq!(focus.snapshot().requested_entity(), Some(requested));
    }
}

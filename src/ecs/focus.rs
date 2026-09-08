use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::change_detection::DetectChanges as _;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::{Add, Remove};
use bevy::ecs::observer::On;
use bevy::ecs::query::{Added, Has, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, Populated, Query, Res, ResMut, Single};
use bevy::prelude::Event as BevyEvent;
use std::collections::HashMap;
use tracing::{Level, debug, instrument, trace, warn};

use super::{FocusedMarker, MouseHeldMarker, SystemTheme};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{ActiveDisplay, GlobalState, WindowCtx, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, ObservedWindowFrame, PresentedWindowFrame, RaiseWindow, Scrolling,
    SendMessageTrigger, SpawnCommandsExt, StrayFocusEvent,
};
use crate::events::{Event, FocusObservation};
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, WinID, WorkspaceId};

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
    any: FocusOrder,
    tiled: FocusOrder,
    floating: FocusOrder,
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
    pid: Option<Pid>,
    candidate: Option<WinID>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusRequest {
    pub entity: Entity,
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
        self.requested.map(|request| request.entity)
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
            } => known_pid == Some(pid) && known_window_id == window_id,
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
                self.resolving = None;
                self.requested = None;
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
                self.resolving = None;
                self.requested = None;
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Unresolved { generation } => {
                if !self.accepts(generation, None) {
                    return FocusUpdate::Stale;
                }
                self.observed = ObservedFocus::Unresolved;
                self.resolving = None;
                self.requested = None;
                FocusUpdate::Accepted(generation)
            }
            FocusSignal::Invalidated { entity } => {
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
                if requested_matches {
                    self.requested = None;
                }
                FocusUpdate::Accepted(generation)
            }
        }
    }

    pub(super) fn request(&mut self, entity: Entity) -> u64 {
        let generation = self.next_generation();
        self.resolving = None;
        self.requested = Some(FocusRequest { entity });
        debug!(target: "spool::focus_diagnostics", generation, ?entity,
            observed = ?self.observed, "focus_request");
        generation
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
        let memory = self.by_workspace.entry(workspace).or_default();
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

    pub(crate) fn last_floating(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)
            .and_then(|memory| memory.floating.last())
    }

    pub(crate) fn navigation_entity(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.requested
            .map(|request| request.entity)
            .or_else(|| self.by_workspace.get(&workspace)?.any.last())
    }

    pub(crate) fn restoration_entity(
        &self,
        workspace: WorkspaceId,
        eligible: impl FnMut(Entity) -> bool,
    ) -> Option<Entity> {
        self.by_workspace
            .get(&workspace)?
            .any
            .newest_matching(eligible)
    }

    pub(super) fn forget(&mut self, entity: Entity) {
        for memory in self.by_workspace.values_mut() {
            memory.tiled.forget(entity);
            memory.floating.forget(entity);
            memory.any.forget(entity);
        }
    }

    pub(super) fn forget_workspace(&mut self, workspace: WorkspaceId) {
        self.by_workspace.remove(&workspace);
    }
}

pub struct FocusEventsPlugin;

impl Plugin for FocusEventsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FocusCoordinator>();
        app.add_systems(
            PostUpdate,
            (
                project_confirmed_focus,
                autocenter_window_on_focus.after(super::systems::animate_resize_entities),
                mouse_follows_focus.after(super::systems::animate_resize_entities),
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

fn project_confirmed_focus(
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

fn focus_window_trigger(
    trigger: On<FocusWindow>,
    windows: Windows,
    apps: Query<&Application>,
    mut focus: ResMut<FocusCoordinator>,
    mut transactions: ResMut<super::native_space::NativeSpaceTransactions>,
) {
    let FocusWindow {
        entity,
        raise,
        kind,
    } = *trigger.event();
    let Some((window, _, app_entity)) = windows.get_parent(entity) else {
        return;
    };
    let Ok(app) = apps.get(app_entity) else {
        return;
    };
    if kind == FocusRequestKind::Explicit {
        transactions.cancel_pending_follows();
    }
    let psn = app.psn();
    focus.request(entity);
    if !raise
        && let Some((focused_window, focused_entity)) = windows.focused()
        && let Some((_, _, focused_app_entity)) = windows.get_parent(focused_entity)
        && let Ok(focused_app) = apps.get(focused_app_entity)
    {
        window.focus_without_raise(psn, focused_window, focused_app.psn());
    } else {
        window.focus_with_raise(psn);
    }
}

fn raise_window_trigger(
    trigger: On<RaiseWindow>,
    windows: Query<(
        Entity,
        &Window,
        Option<&ObservedWindowFrame>,
        Option<&PresentedWindowFrame>,
    )>,
    active_display: ActiveDisplay,
    config: Res<Config>,
) {
    let RaiseWindow { entity, with_strip } = *trigger.event();

    let Ok((focus, window, _, _)) = windows.get(entity) else {
        return;
    };

    if with_strip {
        let viewport = active_display.actual_bounds(&config);
        let strip = active_display.active_strip();
        strip
            .all_windows()
            .into_iter()
            .filter_map(|entity| {
                if entity == focus {
                    None
                } else {
                    windows.get(entity).ok()
                }
            })
            .filter(|(_, _, observed, presented)| {
                observed
                    .map(|frame| frame.0)
                    .or_else(|| presented.map(|frame| frame.0))
                    .is_some_and(|frame| viewport.intersect(frame).width() > 50)
            })
            .for_each(|(_, window, _, _)| {
                window.raise_without_focus();
            });
    }

    // Raise the focused window last, because raised windows get OS focus events.
    window.raise_without_focus();
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::World;

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

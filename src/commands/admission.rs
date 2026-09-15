//! Ordered execution returns admission, never an enqueue acknowledgement.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use spool_shared_types::commands::SpaceLayoutOperation;

use super::{Action, DisplayTarget, Operation};
use crate::config::Config;
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::Windows;
use crate::ecs::topology::{NativeTopology, WindowMemberships};
use crate::ecs::workspace::PendingSpaceDestruction;
use crate::ecs::{ActiveWorkspaceMarker, DockPosition, NativeFullscreenMarker};
use crate::manager::{Display, Window, WindowManager};
use crate::platform::{WinID, WorkspaceId};

pub(super) fn execute(world: &mut World, action: Action) -> crate::errors::Result<()> {
    execute_action(world, action, LifecycleEffect::Deferred)
}

/// The fire-and-forget dispatch path. Unlike a checked command, it has no
/// receipt to deliver first, so a lifecycle action performs its native effect
/// immediately instead of leaving it to the reply adapter.
pub(super) fn execute_dispatched(world: &mut World, action: Action) -> crate::errors::Result<()> {
    execute_action(world, action, LifecycleEffect::Immediate)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LifecycleEffect {
    /// The checked-command adapter attempts the receipt before shutdown.
    Deferred,
    /// The dispatch path runs the native effect inline.
    Immediate,
}

/// Which gates one action passes before its effect may run.
///
/// The compiler demands an answer for every `Action` variant, so a new action
/// cannot be added without stating its reach. A hand-written list of the gated
/// variants would instead let a new one bypass the session gate by omission,
/// which is how the same action came to be admitted differently on different
/// intakes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionReach {
    /// The session must be writable as well: not initializing, not showing
    /// Mission Control, not exiting.
    Writable,
    /// The lifecycle gate alone decides.
    Running,
}

fn session_reach(action: &Action) -> SessionReach {
    match action {
        // Layout State edits. `Layout(plan)` is one admitted batch whose inner
        // operations still take their own admission path during replay.
        Action::Window(_)
        | Action::TargetedWindow { .. }
        | Action::SpaceLayout { .. }
        | Action::Layout(_) => SessionReach::Writable,
        // Everything else keeps the reach it has today. Several of these do
        // edit state — a Space focus preference, a native Space command — and
        // the native command system revalidates the session itself; promoting
        // one to `Writable` is its own change, with its own tests.
        Action::SetSpaceFocusPreference { .. }
        | Action::Mouse(_)
        | Action::FocusWindow { .. }
        | Action::FocusWindowInSpace { .. }
        | Action::FocusSpace { .. }
        | Action::MoveWindowToSpace { .. }
        | Action::MoveColumnToSpace { .. }
        | Action::MoveFocusedWindowToSpace { .. }
        | Action::CreateSpace { .. }
        | Action::DeleteSpace { .. }
        | Action::ToggleBarCollapse
        | Action::ReorderColumn { .. }
        | Action::PrintState
        | Action::ReconcileWindows
        | Action::MissionControl
        | Action::ShowDesktop
        | Action::Lua(_)
        // Answered before this classification is consulted.
        | Action::Quit
        | Action::Restart => SessionReach::Running,
    }
}

fn session_is_writable(world: &World) -> bool {
    !world.contains_resource::<crate::ecs::Initializing>()
        && !world
            .get_resource::<crate::ecs::MissionControlActive>()
            .is_some_and(|overview| overview.0)
        && !world.contains_resource::<crate::ecs::exit_restore::ExitInProgress>()
}

/// One translation for every effect arm: a cached system that could not run at
/// all is an execution failure, while whatever the system itself concluded
/// passes through untouched.
fn invoked<T, E: std::fmt::Display>(outcome: Result<T, E>) -> crate::errors::Result<T> {
    outcome
        .map_err(|error| crate::errors::Error::rejection_with_cause("execution_unavailable", error))
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
fn execute_action(
    world: &mut World,
    action: Action,
    lifecycle_effect: LifecycleEffect,
) -> crate::errors::Result<()> {
    let lifecycle = world.resource::<crate::lifecycle::Lifecycle>().clone();
    if lifecycle_effect == LifecycleEffect::Immediate
        && matches!(action, Action::Quit | Action::Restart)
    {
        return match action {
            Action::Quit => invoked(world.run_system_cached(super::command_quit_handler)),
            Action::Restart => invoked(world.run_system_cached(super::command_restart_handler)),
            _ => unreachable!("lifecycle action"),
        };
    }
    if let Some(reason) = lifecycle.rejection() {
        return Err(crate::errors::Error::rejected(reason));
    }
    if matches!(action, Action::Quit | Action::Restart) {
        lifecycle.set(crate::lifecycle::Phase::Stopping);
        return Ok(());
    }
    // Effect-only actions carry no state to validate; they run before the
    // arrangement recipes so a session that cannot answer a layout query still
    // reaches its native effect. `session_reach` declares the same reach.
    match &action {
        Action::MissionControl => {
            return invoked(world.run_system_cached_with(
                super::command_system_overview,
                crate::platform::mission_control::SystemOverview::MissionControl,
            ));
        }
        Action::ShowDesktop => {
            return invoked(world.run_system_cached_with(
                super::command_system_overview,
                crate::platform::mission_control::SystemOverview::ShowDesktop,
            ));
        }
        Action::PrintState => {
            return invoked(world.run_system_cached(super::print_internal_state_handler));
        }
        Action::ReconcileWindows => {
            return invoked(world.run_system_cached(super::reconcile_windows_handler));
        }
        Action::ToggleBarCollapse => {
            return invoked(world.run_system_cached(super::command_toggle_bar_collapse));
        }
        _ => {}
    }
    let action = invoked(world.run_system_cached_with(resolve_default, action))??;
    if let Some(result) =
        invoked(world.run_system_cached_with(super::column_width::execute, action.clone()))?
    {
        return result;
    }
    if let Some(result) =
        invoked(world.run_system_cached_with(super::layout_edit::execute, action.clone()))?
    {
        return result;
    }
    if session_reach(&action) == SessionReach::Writable && !session_is_writable(world) {
        return Err(crate::errors::Error::rejected("session_not_writable"));
    }
    if let Action::Window(
        operation @ (Operation::FocusFloating | Operation::FocusTiled | Operation::FocusOtherLayer),
    ) = &action
    {
        invoked(world.run_system_cached_with(validate_layer_target, operation.clone()))??;
    }
    match action {
        Action::Window(operation @ (Operation::Focus(_) | Operation::FocusStep(_))) => {
            invoked(world.run_system_cached_with(super::command_move_focus, operation))?
        }
        Action::Window(Operation::FocusFloating) => {
            invoked(world.run_system_cached(super::command_focus_floating))
        }
        Action::Window(Operation::FocusTiled) => {
            invoked(world.run_system_cached(super::command_focus_tiled))
        }
        Action::Window(Operation::FocusOtherLayer) => {
            invoked(world.run_system_cached(super::command_focus_other_layer))
        }
        Action::Mouse(super::MouseMove::ToNextDisplay) => invoked(world.run_system_cached_with(
            super::checked_focus_other_display,
            (None, DisplayTarget::Next),
        ))?,
        Action::SetSpaceFocusPreference {
            space_id,
            window_id,
        } => invoked(world.run_system_cached_with(
            crate::ecs::focus::set_space_preference,
            (space_id, window_id),
        ))?,
        action @ (Action::FocusWindow { .. }
        | Action::FocusWindowInSpace { .. }
        | Action::FocusSpace { .. }
        | Action::CreateSpace { .. }
        | Action::DeleteSpace { .. }
        | Action::MoveWindowToSpace { .. }
        | Action::MoveColumnToSpace { .. }) => invoked(world.run_system_cached_with(
            crate::ecs::native_space::execute_native_space_command,
            crate::events::Event::action_requested(action),
        ))?,
        Action::TargetedWindow {
            window_id,
            operation: Operation::ToNextDisplay(move_focus),
        } => invoked(world.run_system_cached_with(
            super::transfer::execute,
            (window_id, move_focus, DisplayTarget::Next),
        ))?,
        Action::TargetedWindow {
            window_id,
            operation: Operation::Move(direction),
        } => {
            let destination = invoked(
                world.run_system_cached_with(move_destination, (window_id, direction.clone())),
            )?;
            if let Some(destination) = destination {
                invoked(world.run_system_cached_with(
                    super::transfer::execute,
                    (window_id, super::MoveFocus::Follow, destination),
                ))?
            } else {
                invoked(world.run_system_cached_with(
                    super::targeted::execute,
                    Action::TargetedWindow {
                        window_id,
                        operation: Operation::Move(direction),
                    },
                ))?
            }
        }
        action @ Action::TargetedWindow { .. } => {
            invoked(world.run_system_cached_with(super::targeted::execute, action))?
        }
        Action::SpaceLayout {
            space_id,
            operation: SpaceLayoutOperation::ToggleTiledVisibility,
        } => invoked(world.run_system_cached_with(crate::ecs::tiled_visibility::toggle, space_id))?,
        action @ Action::ReorderColumn { .. } => {
            invoked(world.run_system_cached_with(super::command_reorder_column, action))
        }
        #[cfg(feature = "lua")]
        Action::Layout(plan) => {
            invoked(world.run_system_cached_with(crate::ecs::layout_ops::apply_layout_plan, plan))
        }
        #[cfg(not(feature = "lua"))]
        Action::Layout(_) => Err(crate::errors::Error::rejected("unsupported_operation")),
        _ => Err(crate::errors::Error::rejected("unsupported_operation")),
    }
}

fn move_destination(
    In((window_id, direction)): In<(i32, super::Direction)>,
    windows: crate::ecs::params::Windows,
    strips: Query<&crate::ecs::layout::LayoutStrip>,
) -> Option<DisplayTarget> {
    let (_, entity) = windows.find(window_id)?;
    let (_, _, state) = windows.get_tracked(entity)?;
    if !state.is_tiled() {
        return None;
    }
    let strip = strips.iter().find(|strip| strip.contains(entity))?;
    if super::get_window_in_direction(&direction, entity, strip).is_some() {
        return None;
    }
    DisplayTarget::from_direction(&direction)
}

fn resolve_default(
    In(action): In<Action>,
    windows: crate::ecs::params::Windows,
    focus: Res<crate::ecs::focus::FocusCoordinator>,
) -> crate::errors::Result<Action> {
    let focused_id = || {
        super::command_entity(&windows, &focus)
            .and_then(|entity| windows.get(entity))
            .map(|window| window.id())
            .ok_or_else(|| crate::errors::Error::rejected("no_focused_window"))
    };
    Ok(match action {
        Action::MoveFocusedWindowToSpace {
            space_id,
            move_focus,
        } => Action::MoveWindowToSpace {
            window_id: focused_id()?,
            space_id,
            move_focus,
        },
        Action::Window(Operation::Equalize) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::Equalize { column: None },
        },
        Action::Window(Operation::Balance) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::Balance {
                reference_column: None,
            },
        },
        Action::Window(Operation::ToggleTiledVisibility) => Action::SpaceLayout {
            space_id: None,
            operation: SpaceLayoutOperation::ToggleTiledVisibility,
        },
        Action::Window(
            operation @ (Operation::Center
            | Operation::Move(_)
            | Operation::Resize { .. }
            | Operation::SetWidth(_)
            | Operation::Maximize
            | Operation::Snap
            | Operation::ToggleFloating
            | Operation::ToggleStack
            | Operation::ToNextDisplay(_)),
        ) => Action::TargetedWindow {
            window_id: focused_id()?,
            operation,
        },
        action => action,
    })
}

fn validate_layer_target(
    In(operation): In<Operation>,
    windows: crate::ecs::params::Windows,
    active: crate::ecs::params::ActiveDisplay,
    focus: Res<crate::ecs::focus::FocusCoordinator>,
    manager: Res<crate::manager::WindowManager>,
    mut topology: ResMut<crate::ecs::topology::NativeTopology>,
) -> crate::errors::Result<()> {
    let floating = match operation {
        Operation::FocusFloating => true,
        Operation::FocusTiled => false,
        _ => !super::command_entity(&windows, &focus)
            .and_then(|entity| windows.get_tracked(entity))
            .is_some_and(|(_, _, state)| state.is_floating()),
    };
    let target = if floating {
        let candidates = super::visible_floating_entities(
            &windows,
            &manager,
            active.active_strip().id(),
            active.bounds(),
        );
        focus
            .last_floating(active.active_strip().id())
            .filter(|entity| candidates.contains(entity))
            .or_else(|| candidates.into_iter().next())
    } else {
        let strip = windows.navigable_strip(active.active_strip());
        focus
            .last_tiled(strip.id())
            .filter(|entity| strip.contains(*entity))
            .or_else(|| strip.all_columns().into_iter().next())
    }
    .ok_or_else(|| crate::errors::Error::rejected("focus_target_not_found"))?;
    let window = windows
        .get(target)
        .ok_or_else(|| crate::errors::Error::rejected("window_unavailable"))?;
    if !windows.layout_is_writable(target)
        || topology.observe_visible_window_space(&manager, window.id())
            != Some(active.active_strip().id())
    {
        return Err(crate::errors::Error::rejected("focus_target_unavailable"));
    }
    Ok(())
}

/// Bounded reasons a Command Admission recipe refuses a command. [`Self::code`]
/// is the IPC contract: each string is byte-for-byte the `Error::rejected` code
/// the domain executors publish today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    WindowNotFound,
    WindowUnavailable,
    NoFocusedWindow,
    FocusedWindowOutsideSpace,
    SpaceNotVisible,
    FullscreenSpace,
    LayoutNotFound,
    AmbiguousLayout,
    LayoutTransitionPending,
    ColumnOutOfRange,
    ColumnUnavailable,
    IneligibleLayoutEntry,
    TopologyUnresolved,
    DisplayNotFound,
    AmbiguousDisplay,
    DisplayGeometryStale,
    InvalidDisplayFrame,
    LayoutOwnershipUnresolved,
    TargetSpaceUnavailable,
}

impl Rejection {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::WindowNotFound => "window_not_found",
            Self::WindowUnavailable => "window_unavailable",
            Self::NoFocusedWindow => "no_focused_window",
            Self::FocusedWindowOutsideSpace => "focused_window_outside_space",
            Self::SpaceNotVisible => "space_not_visible",
            Self::FullscreenSpace => "fullscreen_space",
            Self::LayoutNotFound => "layout_not_found",
            Self::AmbiguousLayout => "ambiguous_layout",
            Self::LayoutTransitionPending => "layout_transition_pending",
            Self::ColumnOutOfRange => "column_out_of_range",
            Self::ColumnUnavailable => "column_unavailable",
            Self::IneligibleLayoutEntry => "ineligible_layout_entry",
            Self::TopologyUnresolved => "topology_unresolved",
            Self::DisplayNotFound => "display_not_found",
            Self::AmbiguousDisplay => "ambiguous_display",
            Self::DisplayGeometryStale => "display_geometry_stale",
            Self::InvalidDisplayFrame => "invalid_display_frame",
            Self::LayoutOwnershipUnresolved => "layout_ownership_unresolved",
            Self::TargetSpaceUnavailable => "target_space_unavailable",
        }
    }
}

impl From<Rejection> for crate::errors::Error {
    fn from(rejection: Rejection) -> Self {
        crate::errors::Error::rejected(rejection.code())
    }
}

type AdmissionStrips<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LayoutStrip,
        Option<&'static ChildOf>,
        Has<ActiveWorkspaceMarker>,
    ),
>;

type WritableTransferStrips<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static LayoutStrip, Option<&'static ChildOf>),
    (
        Without<PendingSpaceDestruction>,
        Without<NativeFullscreenMarker>,
    ),
>;

type FullscreenInclusiveStrips<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static LayoutStrip, Option<&'static ChildOf>),
    Without<PendingSpaceDestruction>,
>;

/// A strip's layout, its entity, the display showing it, and that display's
/// usable viewport after topology and geometry freshness are confirmed.
pub(crate) struct SpaceView<'a> {
    pub(crate) strip: &'a LayoutStrip,
    pub(crate) strip_entity: Entity,
    pub(crate) display: &'a Display,
    pub(crate) viewport: IRect,
}

/// Shared Command Admission recipes: resolving a target window, its Native
/// Space and strip, and gating layout edits. Each returns a typed [`Rejection`]
/// so the domain executors can keep their own failure strategy.
#[derive(SystemParam)]
pub(crate) struct Admission<'w, 's> {
    windows: Windows<'w, 's>,
    #[cfg(test)]
    focus: Res<'w, FocusCoordinator>,
    topology: ResMut<'w, NativeTopology>,
    manager: Res<'w, WindowManager>,
    config: Res<'w, Config>,
    displays: Query<'w, 's, (Entity, &'static Display, Option<&'static DockPosition>)>,
    strips: AdmissionStrips<'w, 's>,
    transfer_strips: WritableTransferStrips<'w, 's>,
    fullscreen_inclusive_strips: FullscreenInclusiveStrips<'w, 's>,
}

impl Admission<'_, '_> {
    /// The tracked window addressed by id, regardless of visibility.
    pub(crate) fn window(&self, window_id: WinID) -> Result<(&Window, Entity), Rejection> {
        self.windows
            .find(window_id)
            .ok_or(Rejection::WindowNotFound)
    }

    /// The addressed identity even when it is currently unavailable for
    /// operations, mirroring the identity-only lookup the layout state edits
    /// use against their own window query.
    pub(crate) fn window_any(windows: &Windows, window_id: WinID) -> Result<Entity, Rejection> {
        windows
            .find_any(window_id)
            .map(|(_, entity)| entity)
            .ok_or(Rejection::WindowNotFound)
    }

    /// The addressed window when it is visible and its layout is writable.
    pub(crate) fn writable_window(&self, window_id: WinID) -> Result<Entity, Rejection> {
        let (_, entity) = self.window(window_id)?;
        let (_, _, state) = self
            .windows
            .get_tracked(entity)
            .ok_or(Rejection::WindowUnavailable)?;
        (state.is_visible() && self.windows.layout_is_writable(entity))
            .then_some(entity)
            .ok_or(Rejection::WindowUnavailable)
    }

    /// Pending focus wins over confirmed focus; an invisible target is refused
    /// rather than redirected back to the previously confirmed window.
    #[cfg(test)]
    pub(crate) fn focus_target(&self) -> Result<Entity, Rejection> {
        Self::focused_target_in(&self.windows, &self.focus)
    }

    /// Resolves pending or confirmed focus against explicit query and focus
    /// references.
    pub(crate) fn focused_target_in(
        windows: &Windows,
        focus: &FocusCoordinator,
    ) -> Result<Entity, Rejection> {
        let entity = focus
            .snapshot()
            .requested_entity()
            .or_else(|| windows.focused().map(|(_, entity)| entity))
            .ok_or(Rejection::NoFocusedWindow)?;
        windows
            .get_tracked(entity)
            .filter(|(_, _, state)| state.is_visible())
            .map(|(_, entity, _)| entity)
            .ok_or(Rejection::WindowUnavailable)
    }

    /// The window's one visible, non-fullscreen Native Space.
    pub(crate) fn visible_space(&mut self, window_id: WinID) -> Result<WorkspaceId, Rejection> {
        let space = self
            .topology
            .observe_visible_window_space(&self.manager, window_id)
            .ok_or(Rejection::SpaceNotVisible)?;
        if self.topology.is_fullscreen(space) {
            return Err(Rejection::FullscreenSpace);
        }
        Ok(space)
    }

    /// The display currently showing `space`, when its inventory is complete.
    pub(crate) fn visible_display_for_space(&self, space: WorkspaceId) -> Result<u32, Rejection> {
        self.topology
            .visible_display_for_space(space)
            .ok_or(Rejection::TopologyUnresolved)
    }

    /// Every known physical display and the Space ids it owns.
    pub(crate) fn known_displays(&self) -> impl Iterator<Item = (&Display, &[WorkspaceId])> + '_ {
        self.topology.known_displays()
    }

    /// A window's native memberships, refusing when no complete scan is available.
    pub(crate) fn memberships(&self) -> crate::errors::Result<WindowMemberships> {
        self.topology
            .observe_memberships(&self.manager)
            .map_err(|error| {
                crate::errors::Error::rejection_with_cause("native_operation_rejected", error)
            })
    }

    /// A candidate remaining in `strip` whose native Space is still `space` and
    /// whose native reassignment is not in flight.
    pub(crate) fn remaining_member(
        &self,
        strip: &LayoutStrip,
        entity: Entity,
        blocked: &[Entity],
        memberships: &WindowMemberships,
        space: WorkspaceId,
    ) -> bool {
        strip.contains(entity)
            && !blocked.contains(&entity)
            && self
                .windows
                .get_tracked(entity)
                .is_some_and(|(window, _, state)| {
                    state.is_visible() && memberships.unique_space(window.id()) == Some(space)
                })
    }

    /// The display inventory entity for `display_id`, refusing a duplicate id,
    /// an unresolved native display, or stale geometry.
    pub(crate) fn display_entity(&self, display_id: u32) -> Result<Entity, Rejection> {
        let mut candidates = self
            .displays
            .iter()
            .filter(|(_, display, _)| display.id() == display_id);
        let (entity, display, _) = candidates.next().ok_or(Rejection::DisplayNotFound)?;
        if candidates.next().is_some() {
            return Err(Rejection::AmbiguousDisplay);
        }
        let (native, _) = self
            .topology
            .known_displays()
            .find(|(native, _)| native.id() == display_id)
            .ok_or(Rejection::TopologyUnresolved)?;
        if display.clone().update_geometry(native) {
            return Err(Rejection::DisplayGeometryStale);
        }
        Ok(entity)
    }

    /// The display inventory entry for `display_id`, refusing a duplicate id.
    pub(crate) fn display_context(&self, display_id: u32) -> Result<(Entity, IRect), Rejection> {
        let entity = self.display_entity(display_id)?;
        let (_, display, dock) = self
            .displays
            .get(entity)
            .map_err(|_| Rejection::DisplayNotFound)?;
        let viewport = display
            .checked_actual_display_bounds(dock, &self.config)
            .ok_or(Rejection::InvalidDisplayFrame)?;
        Ok((entity, viewport))
    }

    /// The Space a display uniquely shows, without excluding native fullscreen.
    pub(crate) fn visible_space_on_display(
        &self,
        display_id: u32,
    ) -> Result<WorkspaceId, Rejection> {
        let space = self
            .topology
            .visible_space(display_id)
            .ok_or(Rejection::SpaceNotVisible)?;
        (self.topology.visible_display_for_space(space) == Some(display_id))
            .then_some(space)
            .ok_or(Rejection::SpaceNotVisible)
    }

    /// The Space a display currently shows, when it is uniquely visible and not
    /// native fullscreen.
    pub(crate) fn target_space(&self, display_id: u32) -> Result<WorkspaceId, Rejection> {
        let space = self
            .visible_space_on_display(display_id)
            .map_err(|_| Rejection::TargetSpaceUnavailable)?;
        if self.topology.is_fullscreen(space) {
            return Err(Rejection::TargetSpaceUnavailable);
        }
        Ok(space)
    }

    /// The globally active display, when topology has resolved one.
    pub(crate) fn active_display(&self) -> Option<u32> {
        self.topology.active_display()
    }

    /// Every display's Space inventory is currently known.
    pub(crate) fn require_topology(&mut self) -> Result<(), Rejection> {
        self.topology
            .refresh_for_command(&self.manager)
            .then_some(())
            .ok_or(Rejection::TopologyUnresolved)
    }

    /// The one strip owning `space`, its entity, and its parent display entity.
    ///
    /// An ordinary window command refuses an absent or duplicated layout rather
    /// than picking the first match.
    pub(crate) fn space_view(&self, space: WorkspaceId) -> Result<SpaceView<'_>, Rejection> {
        let mut matches = self
            .strips
            .iter()
            .filter(|(_, strip, _, _)| strip.id() == space);
        let (strip_entity, strip, parent, _) = matches.next().ok_or(Rejection::LayoutNotFound)?;
        if matches.next().is_some() {
            return Err(Rejection::AmbiguousLayout);
        }
        let display_entity = parent
            .map(ChildOf::parent)
            .ok_or(Rejection::DisplayNotFound)?;
        let (_, display, dock) = self
            .displays
            .get(display_entity)
            .map_err(|_| Rejection::DisplayNotFound)?;
        if self.topology.visible_display_for_space(space) != Some(display.id()) {
            return Err(Rejection::TopologyUnresolved);
        }
        let (native, _) = self
            .topology
            .known_displays()
            .find(|(native, _)| native.id() == display.id())
            .ok_or(Rejection::TopologyUnresolved)?;
        if display.clone().update_geometry(native) {
            return Err(Rejection::DisplayGeometryStale);
        }
        let viewport = display
            .checked_actual_display_bounds(dock, &self.config)
            .ok_or(Rejection::InvalidDisplayFrame)?;
        Ok(SpaceView {
            strip,
            strip_entity,
            display,
            viewport,
        })
    }

    /// The one strip owning `space`, refusing an absent or duplicated layout.
    ///
    /// Shares [`Self::space_scope_strip_in`]'s uniqueness rule rather than
    /// restating it.
    #[cfg(test)]
    pub(crate) fn unique_strip(&self, space: WorkspaceId) -> Result<&LayoutStrip, Rejection> {
        Self::space_scope_strip_in(
            self.strips
                .iter()
                .map(|(_, strip, _, active)| (active, strip)),
            Some(space),
        )
    }

    /// The one strip owning the active Space, or `space` when named, refusing an
    /// absent or duplicated layout. State-only edits scope their plan this way,
    /// against a caller-supplied strip view because the sharing caller needs a
    /// mutable query for its own edits and so cannot also hold [`Admission`]'s
    /// read-only strip query.
    pub(crate) fn space_scope_strip_in<'a>(
        strips: impl Iterator<Item = (bool, &'a LayoutStrip)>,
        space: Option<WorkspaceId>,
    ) -> Result<&'a LayoutStrip, Rejection> {
        let mut matches =
            strips.filter(|(active, strip)| space.map_or(*active, |id| strip.id() == id));
        let (_, strip) = matches.next().ok_or(Rejection::LayoutNotFound)?;
        (matches.next().is_none())
            .then_some(strip)
            .ok_or(Rejection::AmbiguousLayout)
    }

    /// The first strip holding `entity`, without requiring a unique owner,
    /// against a caller-supplied strip view.
    pub(crate) fn strip_containing_in<'a>(
        strips: impl Iterator<Item = &'a LayoutStrip>,
        entity: Entity,
    ) -> Result<&'a LayoutStrip, Rejection> {
        strips
            .into_iter()
            .find(|strip| strip.contains(entity))
            .ok_or(Rejection::LayoutNotFound)
    }

    /// The one non-fullscreen, non-destroying strip on `display` owning `space`.
    pub(crate) fn owned_strip(
        &self,
        space: WorkspaceId,
        display: Entity,
    ) -> Result<&LayoutStrip, Rejection> {
        Self::owned_display_strip(self.transfer_strips.iter(), space, display)
    }

    /// [`Self::owned_strip`] without excluding native fullscreen strips, so the
    /// mouse display path keeps its own filter.
    pub(crate) fn owned_strip_including_fullscreen(
        &self,
        space: WorkspaceId,
        display: Entity,
    ) -> Result<&LayoutStrip, Rejection> {
        Self::owned_display_strip(self.fullscreen_inclusive_strips.iter(), space, display)
    }

    fn owned_display_strip<'a>(
        strips: impl Iterator<Item = (Entity, &'a LayoutStrip, Option<&'a ChildOf>)>,
        space: WorkspaceId,
        display: Entity,
    ) -> Result<&'a LayoutStrip, Rejection> {
        let mut matches = strips.filter(|(_, strip, _)| strip.id() == space);
        let (_, strip, parent) = matches.next().ok_or(Rejection::LayoutNotFound)?;
        (matches.next().is_none() && parent.map(ChildOf::parent) == Some(display))
            .then_some(strip)
            .ok_or(Rejection::LayoutOwnershipUnresolved)
    }

    /// Resolves a one-based column ordinal to a zero-based index.
    pub(crate) fn column_index(strip: &LayoutStrip, ordinal: usize) -> Result<usize, Rejection> {
        let index = Self::ordinal_index(ordinal)?;
        strip
            .get(index)
            .map(|_| index)
            .map_err(|_| Rejection::ColumnOutOfRange)
    }

    /// Resolves a one-based ordinal without requiring the column to exist.
    pub(crate) fn ordinal_index(ordinal: usize) -> Result<usize, Rejection> {
        ordinal.checked_sub(1).ok_or(Rejection::ColumnOutOfRange)
    }

    /// The addressed column's representative entity, refusing a missing column
    /// as out of range and an empty one as unavailable.
    pub(crate) fn column_leader(strip: &LayoutStrip, index: usize) -> Result<Entity, Rejection> {
        let column = strip.get(index).map_err(|_| Rejection::ColumnOutOfRange)?;
        column.top().ok_or(Rejection::ColumnUnavailable)
    }

    /// A column addressed for editing must exist and not be native fullscreen.
    pub(crate) fn ineligible_column(strip: &LayoutStrip, index: usize) -> Result<(), Rejection> {
        match strip.get(index) {
            Ok(column) if !matches!(column, Column::Fullscren(_)) => Ok(()),
            _ => Err(Rejection::IneligibleLayoutEntry),
        }
    }

    /// Whether the addressed column holds a member whose native reassignment is
    /// in flight, without gating on availability or fullscreen, against an
    /// explicit window query.
    pub(crate) fn column_transition_pending_in(
        windows: &Windows,
        strip: &LayoutStrip,
        index: usize,
    ) -> bool {
        strip.get(index).is_ok_and(|column| {
            column
                .window_iter()
                .any(|member| windows.layout_transition_pending(member))
        })
    }

    /// A state-edit column must exist, not be native fullscreen, and hold no
    /// member whose native reassignment is in flight.
    #[cfg(test)]
    pub(crate) fn eligible_column(
        &self,
        strip: &LayoutStrip,
        index: usize,
    ) -> Result<(), Rejection> {
        Self::eligible_column_in(&self.windows, strip, index)
    }

    /// Runs the state-edit column check against an explicit window query.
    pub(crate) fn eligible_column_in(
        windows: &Windows,
        strip: &LayoutStrip,
        index: usize,
    ) -> Result<(), Rejection> {
        let column = strip.get(index).map_err(|_| Rejection::ColumnUnavailable)?;
        if matches!(column, Column::Fullscren(_)) {
            return Err(Rejection::IneligibleLayoutEntry);
        }
        if column.window_iter().any(|member| {
            windows.get_any(member).is_none() || windows.layout_transition_pending(member)
        }) {
            return Err(Rejection::LayoutTransitionPending);
        }
        Ok(())
    }

    /// A tiled window's containing column must exist and not be fullscreen.
    pub(crate) fn tiled_column(strip: &LayoutStrip, entity: Entity) -> Result<(), Rejection> {
        match strip.column_containing(entity) {
            Some(column) if !matches!(column, Column::Fullscren(_)) => Ok(()),
            _ => Err(Rejection::IneligibleLayoutEntry),
        }
    }

    /// Every member of the window's containing column must be writable.
    pub(crate) fn writable_column(
        &self,
        strip: &LayoutStrip,
        entity: Entity,
    ) -> Result<(), Rejection> {
        self.windows
            .layout_column_is_writable(strip, entity)
            .then_some(())
            .ok_or(Rejection::ColumnUnavailable)
    }
}

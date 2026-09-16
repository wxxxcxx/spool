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
    execute_action(world, action)
}

/// The common caller: decide, then conclude whatever is not waiting on a reply.
/// A checked request uses [`execute`] instead, so its requester owns that step.
pub(super) fn admit(world: &mut World, action: Action) -> crate::errors::Result<()> {
    let aftermath = aftermath(&action);
    execute(world, action)?;
    aftermath.conclude_in_world(world);
    Ok(())
}

/// The deferred continuation of a script's plan.
///
/// It is not an action — it carries a snapshot binding no `Action` variant can
/// hold — but it takes the same gate as the action it continues, so a plan
/// cannot reach the native command system at a moment when the same command
/// would have been refused.
pub(super) fn admit_deferred(
    world: &mut World,
    event: crate::events::Event,
) -> crate::errors::Result<()> {
    session_is_ready(world)?;
    invoked(
        world.run_system_cached_with(crate::ecs::native_space::apply_native_space_command, event),
    )
}

/// The lifecycle decision: `Ok` when the session may act at all.
///
/// One question with one answer, asked by the ordered pipeline and by a plan's
/// deferred continuation, so the two cannot drift into different gates.
pub(super) fn session_is_ready(world: &World) -> crate::errors::Result<()> {
    world
        .resource::<crate::lifecycle::Lifecycle>()
        .rejection()
        .map_or(Ok(()), |reason| Err(crate::errors::Error::rejected(reason)))
}

/// What an admitted action owes the process once its effect has run.
///
/// Not a status: a refused action has no aftermath, and a refusal travels as
/// `Err` with a stable code. `Stop` and `Restart` stay distinct — a restart is
/// an external stop, so raising `Event::Exit` for it would stop the process
/// twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Aftermath {
    /// Nothing follows. Every action but two.
    Nothing,
    /// The daemon stops gracefully: `Event::Exit` on the internal channel.
    Stop,
    /// A detached service restart takes this process down from outside.
    Restart,
}

/// Which actions end the session. Pure and total, so the transport can ask it
/// before the World exists and the table asks the same thing later.
pub(crate) fn aftermath(action: &Action) -> Aftermath {
    match action {
        Action::Quit => Aftermath::Stop,
        Action::Restart => Aftermath::Restart,
        _ => Aftermath::Nothing,
    }
}

impl Aftermath {
    /// From inside the world: nobody is waiting to be answered, so the effect
    /// may follow the decision immediately.
    fn conclude_in_world(self, world: &mut World) {
        match self {
            Self::Nothing => {}
            Self::Stop => {
                _ = world.resource::<crate::manager::WindowManager>().quit();
            }
            Self::Restart => hand_over(),
        }
    }

    /// From whoever owns the transport, once the receipt has been written.
    pub(crate) fn conclude_from_wire(self, events: &crate::events::EventSender) {
        match self {
            Self::Nothing => {}
            Self::Stop => {
                _ = events.send(crate::events::Event::Exit);
            }
            Self::Restart => hand_over(),
        }
    }
}

fn hand_over() {
    if let Err(error) = crate::platform::service::Service::request_restart() {
        tracing::error!(%error, "unable to start admitted service restart");
    }
}

/// Which gates one action passes before its effect may run.
///
/// The compiler demands an answer for every `Action` variant, so a new action
/// cannot be added without stating whether it must still work while the session
/// is handing the desktop back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionReach {
    /// Refused while the session is handing the desktop back (ADR 0011). The exit
    /// path is putting the windows Spool managed back where it found them, so an
    /// edit accepted now would describe a screen that no longer exists, and there
    /// is no later realization to wait for.
    HandingOver,
    /// The lifecycle gate alone decides. An accepted edit is realized when the
    /// desktop allows it: Mission Control and initialization are states to wait
    /// out through the realization outcome, not states to refuse in (ADR 0011).
    Running,
}

fn session_reach(action: &Action) -> SessionReach {
    match action {
        // Ending the session has to stay possible while it is ending: a repeated
        // quit is idempotent, and a restart is the handover itself.
        Action::Quit | Action::Restart => SessionReach::Running,
        // Everything else edits retained state or asks for an effect on a desktop
        // that is already being handed back. The variants are listed rather than
        // wildcarded on purpose: a new action must state which side it is on.
        Action::Window(_)
        | Action::TargetedWindow { .. }
        | Action::SpaceLayout { .. }
        | Action::Layout(_)
        | Action::SetSpaceFocusPreference { .. }
        | Action::FocusWindow { .. }
        | Action::FocusWindowInSpace { .. }
        | Action::FocusSpace { .. }
        | Action::MoveWindowToSpace { .. }
        | Action::MoveColumnToSpace { .. }
        | Action::MoveFocusedWindowToSpace { .. }
        | Action::CreateSpace { .. }
        | Action::DeleteSpace { .. }
        | Action::Mouse(_)
        | Action::ToggleBarCollapse
        | Action::ReorderColumn { .. }
        | Action::PrintState
        | Action::ReconcileWindows
        | Action::MissionControl
        | Action::ShowDesktop
        | Action::RestoreIntents(_)
        | Action::Lua(_) => SessionReach::HandingOver,
    }
}

/// Whether the desktop is still Spool's to change.
///
/// It is not while the exit path is restoring the windows Spool managed. Mission
/// Control and initialization are deliberately not part of this question: their
/// answer is "not yet", which the realization outcome already says (ADR 0011).
fn session_owns_desktop(world: &World) -> bool {
    !world.contains_resource::<crate::ecs::exit_restore::ExitInProgress>()
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
fn execute_action(world: &mut World, action: Action) -> crate::errors::Result<()> {
    session_is_ready(world)?;
    let lifecycle = world.resource::<crate::lifecycle::Lifecycle>().clone();
    if aftermath(&action) != Aftermath::Nothing {
        // The effect waits for whoever owns the transport, so a checked request
        // has its receipt delivered first; this only refuses everything else
        // while the requester still waits.
        lifecycle.set(crate::lifecycle::Phase::Stopping);
        return Ok(());
    }
    // The handover gate comes before every effect, including the effect-only
    // ones: while the windows Spool managed are being put back where it found
    // them, nothing is accepted — an overview command or an audit would write
    // geometry into a desktop that is already leaving. Ending the session stays
    // possible through this same gate (`session_reach`).
    if session_reach(&action) == SessionReach::HandingOver && !session_owns_desktop(world) {
        // The wire code keeps its string: scripts match on it, and what changed
        // is the situation it answers, not the situation's name (ADR 0009's
        // unchanged-wire rule).
        return Err(Rejection::SessionHandingOver.into());
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
        | Action::MoveColumnToSpace { .. }) => {
            // A refused membership edit leaves no transaction behind, so the
            // record is the only place its reason survives for diagnosis.
            let attempt = membership_attempt(&action);
            let outcome = invoked(world.run_system_cached_with(
                crate::ecs::native_space::execute_native_space_command,
                crate::events::Event::action_requested(action),
            ))?;
            if let (Some((window_id, space_id)), Err(error)) = (attempt, &outcome) {
                crate::ecs::native_space::note_space_move_attempt(
                    world,
                    window_id,
                    space_id,
                    crate::ecs::native_space::SpaceMoveResult::Refused(
                        error.admission_code().to_owned(),
                    ),
                );
            }
            outcome
        }
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
        Action::RestoreIntents(bindings) => {
            invoked(world.run_system_cached_with(crate::ecs::restore::restore_intents, bindings))?
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

/// The window and target Space a membership edit addresses, if any.
///
/// Focus and Space-lifecycle actions share their arm with the membership moves
/// but do not move a window's membership, so they have nothing to record.
fn membership_attempt(
    action: &Action,
) -> Option<(crate::platform::WinID, crate::platform::WorkspaceId)> {
    match action {
        Action::MoveWindowToSpace {
            window_id,
            space_id,
            ..
        }
        | Action::MoveColumnToSpace {
            window_id,
            space_id,
            ..
        } => Some((*window_id, *space_id)),
        _ => None,
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
    /// The session is putting the desktop back the way it found it, so no edit is
    /// accepted. The serialized code deliberately stays `session_not_writable`
    /// (ADR 0011 narrowed the reach; ADR 0009 keeps the wire string).
    SessionHandingOver,
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
            Self::SessionHandingOver => "session_not_writable",
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
    ///
    /// An unreachable target and an unusable one are different answers: a display
    /// that is not showing a single Space is "not now" (`SpaceNotVisible`,
    /// usually a transient topology fact), while a fullscreen Space is not a user
    /// Space at all (`FullscreenSpace`).
    pub(crate) fn target_space(&self, display_id: u32) -> Result<WorkspaceId, Rejection> {
        let space = self.visible_space_on_display(display_id)?;
        if self.topology.is_fullscreen(space) {
            return Err(Rejection::FullscreenSpace);
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
            Ok(column) if !matches!(column, Column::Fullscreen(_)) => Ok(()),
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

    /// Runs the state-edit column check against an explicit window query.
    pub(crate) fn eligible_column_in(
        windows: &Windows,
        strip: &LayoutStrip,
        index: usize,
    ) -> Result<(), Rejection> {
        let column = strip.get(index).map_err(|_| Rejection::ColumnUnavailable)?;
        if matches!(column, Column::Fullscreen(_)) {
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
            Some(column) if !matches!(column, Column::Fullscreen(_)) => Ok(()),
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

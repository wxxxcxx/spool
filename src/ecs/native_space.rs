use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::{Has, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, In, Local, Query, Res, ResMut, SystemParam};
use bevy::math::IRect;
use bevy::time::Time;
use tracing::{debug, error, instrument, warn};

use crate::commands::{Action, MoveFocus};
use crate::config::Config;
use crate::ecs::display::FloatingLayer;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::layout_snapshot::{LayoutSession, matches_window};
use crate::ecs::params::Windows;
use crate::ecs::topology::{NativeTopology, SpaceClaim};
use crate::ecs::window_frame::{DisplayTransferFrame, DisplayTransferReadback};
use crate::ecs::workspace::{
    PendingSpaceDestruction, WindowSpaceReassignmentPending, freeze_window_for_space_reassignment,
};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, Bounds, DesiredWindowFrame, Position,
    PresentedWindowFrame, PreviousTiledStrip, RefreshWindowSizes, SpawnCommandsExt,
    WindowFrameCommitSuspended, WindowFrameMotion, WindowVisibility,
};
use crate::events::Event;
use crate::manager::{Display, NativeSpaceIntent, Origin, WindowManager};
use crate::platform::{WinID, WindowIncarnation, WorkspaceId};
pub use spool_shared_types::state::SpaceKind;

/// The macOS-managed Space represented by a layout strip.
///
/// Every Space owns exactly one `LayoutStrip`; the private platform
/// adapter is the sole source of this session-scoped identity and ordering.
#[derive(Clone, Component, Copy, Debug, Eq, PartialEq)]
pub struct NativeSpace {
    pub id: WorkspaceId,
    pub ordinal: u32,
    pub kind: SpaceKind,
}

impl NativeSpace {
    pub fn new(id: WorkspaceId, ordinal: usize, fullscreen: bool) -> Self {
        Self {
            id,
            ordinal: ordinal.try_into().unwrap_or(u32::MAX),
            kind: if fullscreen {
                SpaceKind::Fullscreen
            } else {
                SpaceKind::User
            },
        }
    }
}

/// Marks the Space currently visible on its physical display.
///
/// Unlike `ActiveWorkspaceMarker`, this is deliberately not a global
/// singleton: with separate Spaces enabled, every connected display has one
/// visible Space at the same time.
#[derive(Component, Debug)]
pub struct VisibleNativeSpaceMarker;

/// Retains a Space projection while its physical display is disconnected.
/// Only observed topology and membership, never elapsed time, end this state.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct DetachedSpace {
    pub(crate) source_display_id: u32,
}

/// The explicit move transaction owns reconciliation until confirmation or
/// timeout. The shared reassignment marker independently suspends geometry.
#[derive(Component)]
pub(crate) struct NativeMoveOwner;

#[derive(Debug)]
struct PendingMove {
    window_ids: Vec<i32>,
    members: Vec<MoveWindowIdentity>,
    target_space_id: WorkspaceId,
    follow: Option<MoveWindowIdentity>,
    layout: PendingMoveLayout,
    submitted: Duration,
    display: Option<DisplayMoveCompletion>,
}

pub(crate) struct DisplayMovePlan {
    pub(crate) members: Vec<Entity>,
    pub(crate) target: IRect,
    pub(crate) viewport: IRect,
    pub(crate) target_space_id: WorkspaceId,
    pub(crate) source_display_id: u32,
    pub(crate) target_display_id: u32,
    pub(crate) follow: Option<Entity>,
    pub(crate) source_neighbour: Option<Entity>,
    pub(crate) tiled: bool,
}

#[derive(Debug)]
struct DisplayMoveCompletion {
    target_display_id: u32,
    source_follow: Option<PendingFollow>,
}

#[derive(Clone, Copy, Debug)]
struct MoveWindowIdentity {
    window_id: WinID,
    entity: Entity,
    incarnation: WindowIncarnation,
}

impl MoveWindowIdentity {
    fn is_current(self, windows: &Windows) -> bool {
        windows
            .find_parent_incarnation_any(self.window_id, self.incarnation)
            .is_some_and(|(_, entity, _)| entity == self.entity)
    }

    fn is_available(self, windows: &Windows) -> bool {
        windows
            .get_tracked(self.entity)
            .is_some_and(|(window, _, _)| {
                window.id() == self.window_id && window.incarnation() == self.incarnation
            })
    }
}

#[derive(Debug)]
enum PendingMoveLayout {
    AssociatedWindows,
    Columns(LayoutStrip),
}

impl PendingMoveLayout {
    fn capture_column<'a>(
        column: Column,
        space_id: WorkspaceId,
        sources: impl Iterator<Item = &'a LayoutStrip>,
        members: &[MoveWindowIdentity],
    ) -> Self {
        let mut remaining = members
            .iter()
            .map(|member| member.entity)
            .collect::<HashSet<_>>();
        for entity in column.window_iter() {
            remaining.remove(&entity);
        }
        let sources = sources.collect::<Vec<_>>();
        let state = column.window_iter().find_map(|entity| {
            sources.iter().find_map(|strip| {
                strip
                    .index_of(entity)
                    .ok()
                    .and_then(|index| strip.column_state(index))
                    .cloned()
            })
        });
        let mut captured = LayoutStrip::new(space_id);
        captured.append_column_with_state(column, state.unwrap_or_default());
        if !remaining.is_empty() {
            let mut sources = sources;
            sources.sort_unstable_by_key(|strip| strip.id());
            for source in sources {
                if remaining.is_empty() {
                    break;
                }
                if !source
                    .columns()
                    .flat_map(Column::window_iter)
                    .any(|entity| remaining.contains(&entity))
                {
                    continue;
                }
                let mut source = source.clone();
                let mut associated = source.take_windows_preserving_layout(&remaining);
                for entity in associated.all_windows() {
                    remaining.remove(&entity);
                }
                captured.append_strip(&mut associated);
            }
            // Floats and not-yet-placed members have no source column to retain.
            for member in members {
                if remaining.remove(&member.entity) {
                    captured.append(member.entity);
                }
            }
        }
        Self::Columns(captured)
    }

    fn tiled_projection(&self, space_id: WorkspaceId, tiled: &[Entity]) -> LayoutStrip {
        let mut strip = match self {
            Self::AssociatedWindows => {
                let mut strip = LayoutStrip::new(space_id);
                strip.append_tab_group(tiled);
                strip
            }
            Self::Columns(captured) => captured.clone(),
        };
        for entity in strip.all_windows() {
            if !tiled.contains(&entity) {
                strip.remove(entity);
            }
        }
        strip
    }
}

#[derive(Debug)]
struct PendingFollow {
    member: MoveWindowIdentity,
    target_space_id: WorkspaceId,
    submitted: Duration,
    visible_display: Option<u32>,
    warp_pointer: bool,
}

impl PendingFollow {
    fn submit(
        member: MoveWindowIdentity,
        target_space_id: WorkspaceId,
        manager: &WindowManager,
        config: &Config,
        submitted: Duration,
    ) -> Option<Self> {
        if !config.space_control_enabled() {
            return None;
        }
        let intent = NativeSpaceIntent::Focus {
            space_id: target_space_id,
            animate: config.space_switch_animation(),
        };
        match manager.perform_native_space_intent(&intent) {
            Ok(()) => Some(Self {
                member,
                target_space_id,
                submitted,
                visible_display: None,
                warp_pointer: false,
            }),
            Err(error) => {
                warn!(window_id = member.window_id, space_id = target_space_id, %error, "unable to follow moved window to Space");
                None
            }
        }
    }
}

#[derive(Default, Resource)]
pub(crate) struct NativeSpaceTransactions {
    moves: Vec<PendingMove>,
    follows: Vec<PendingFollow>,
}

impl NativeSpaceTransactions {
    pub(super) fn focus_transition_pending(&self) -> bool {
        !self.moves.is_empty() || !self.follows.is_empty()
    }

    pub(super) fn cancel_pending_follows(&mut self) {
        for pending in &mut self.moves {
            pending.follow = None;
            if let Some(display) = &mut pending.display {
                display.source_follow = None;
            }
        }
        self.follows.clear();
    }

    fn cancel_follows_for(&mut self, window_ids: &[WinID]) {
        self.follows
            .retain(|pending| !window_ids.contains(&pending.member.window_id));
    }

    pub(crate) fn submit_display_move(
        &mut self,
        plan: DisplayMovePlan,
        windows: &Windows,
        source: &LayoutStrip,
        commands: &mut Commands,
        submitted: Duration,
    ) -> crate::errors::Result<()> {
        let identity = |entity| {
            windows
                .get_tracked(entity)
                .map(|(window, _, _)| MoveWindowIdentity {
                    window_id: window.id(),
                    entity,
                    incarnation: window.incarnation(),
                })
        };
        let Some(members) = plan
            .members
            .iter()
            .copied()
            .map(identity)
            .collect::<Option<Vec<_>>>()
        else {
            return Err(crate::errors::Error::rejected("window_unavailable"));
        };
        let window_ids = members
            .iter()
            .map(|member| member.window_id)
            .collect::<Vec<_>>();
        if self
            .moves
            .iter()
            .any(|pending| pending.window_ids.iter().any(|id| window_ids.contains(id)))
        {
            return Err(crate::errors::Error::rejected("native_move_pending"));
        }
        let follow = plan.follow.and_then(identity);
        let source_follow = plan
            .source_neighbour
            .and_then(identity)
            .filter(|_| follow.is_none())
            .map(|member| PendingFollow {
                member,
                target_space_id: source.id(),
                submitted,
                visible_display: Some(plan.source_display_id),
                warp_pointer: false,
            });
        self.cancel_pending_follows();
        for member in &members {
            freeze_window_for_space_reassignment(
                member.entity,
                source.index_of(member.entity).unwrap_or_default(),
                commands,
            );
            commands.entity(member.entity).insert((
                NativeMoveOwner,
                DisplayTransferFrame {
                    target: plan.target,
                    viewport: plan.viewport,
                    source_space_id: source.id(),
                    target_space_id: plan.target_space_id,
                    display_id: plan.target_display_id,
                    incarnation: member.incarnation,
                    tiled: plan.tiled,
                },
            ));
        }
        self.moves.push(PendingMove {
            window_ids,
            members,
            target_space_id: plan.target_space_id,
            follow,
            layout: PendingMoveLayout::AssociatedWindows,
            submitted,
            display: Some(DisplayMoveCompletion {
                target_display_id: plan.target_display_id,
                source_follow,
            }),
        });
        Ok(())
    }
}

enum FocusSpacePolicy {
    VisibleOnly,
    AllowNativeActivation,
}

fn focus_window_command(
    window_id: WinID,
    policy: FocusSpacePolicy,
    known_space: Option<WorkspaceId>,
    windows: &Windows,
    window_manager: &WindowManager,
    topology: &mut NativeTopology,
    commands: &mut Commands,
) -> crate::errors::Result<()> {
    let Some((_, entity)) = windows.find(window_id) else {
        warn!(window_id, "window is not tracked");
        return Err(crate::errors::Error::rejected("window_not_found"));
    };
    let mut native_space = None;
    if matches!(policy, FocusSpacePolicy::VisibleOnly) {
        // A caller that drew this window in a Space it can name hands that
        // Space over here, so confirming visibility can skip the scan of every
        // other Space. Only a confirmation takes that shortcut; a refusal asks
        // the question the strict path always asked, and an unreadable Space
        // refuses on evidence that path would refuse on.
        let claim = known_space.map(|space_id| {
            topology.confirm_visible_window_space(window_manager, window_id, space_id)
        });
        let confirmed = match claim {
            Some(SpaceClaim::Confirmed) => known_space,
            // An unreadable Space is not evidence that it is visible.
            Some(SpaceClaim::Unavailable) => None,
            Some(SpaceClaim::Refused) | None => {
                topology.observe_visible_window_space(window_manager, window_id)
            }
        };
        native_space = confirmed;
        if confirmed.is_none() {
            warn!(window_id, "cannot confirm a visible Space for window focus");
            return Err(crate::errors::Error::rejected("space_not_visible"));
        }
    }
    commands.trigger(super::focus::FocusWindow {
        entity,
        raise: true,
        kind: super::focus::FocusRequestKind::Explicit,
        allow_native_activation: matches!(policy, FocusSpacePolicy::AllowNativeActivation),
        native_space,
    });
    Ok(())
}

#[derive(SystemParam)]
pub(crate) struct NativeSpaceCommandCtx<'w, 's> {
    windows: Windows<'w, 's>,
    spaces: Query<'w, 's, &'static LayoutStrip>,
    previous_strips: Query<'w, 's, &'static PreviousTiledStrip>,
    config: Res<'w, Config>,
    window_manager: Res<'w, WindowManager>,
    transactions: bevy::ecs::system::ResMut<'w, NativeSpaceTransactions>,
    time: Res<'w, Time>,
    session: Res<'w, LayoutSession>,
    topology: ResMut<'w, NativeTopology>,
    commands: Commands<'w, 's>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn apply_native_space_command(In(event): In<Event>, ctx: NativeSpaceCommandCtx) {
    if let Err(reason) = execute_native_space_command(In(event), ctx) {
        debug!(%reason, "native command rejected");
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn execute_native_space_command(
    In(event): In<Event>,
    ctx: NativeSpaceCommandCtx,
) -> crate::errors::Result<()> {
    let NativeSpaceCommandCtx {
        windows,
        spaces,
        previous_strips,
        config,
        window_manager,
        mut transactions,
        time,
        session,
        mut topology,
        mut commands,
    } = ctx;
    let (action, snapshot) = match &event {
        Event::ActionRequested { action } => (Cow::Borrowed(action), None),
        Event::LayoutSpaceRequested { op, snapshot } => {
            use spool_shared_types::windowset::LayoutOp;
            if !session.accepts(snapshot, *op, &windows) {
                debug!(
                    ?op,
                    "skipping deferred Space command with a stale snapshot identity"
                );
                return Err(crate::errors::Error::rejected("native_precondition_failed"));
            }
            let action = match *op {
                LayoutOp::MoveToWorkspace {
                    window,
                    space_id,
                    follow,
                } => Action::MoveWindowToSpace {
                    window_id: window,
                    space_id,
                    move_focus: if follow {
                        MoveFocus::Follow
                    } else {
                        MoveFocus::Stay
                    },
                },
                LayoutOp::View { space_id } => Action::FocusSpace { space_id },
                LayoutOp::Focus(window_id) => Action::FocusWindow { window_id },
                _ => return Err(crate::errors::Error::rejected("unsupported_operation")),
            };
            (Cow::Owned(action), Some(snapshot.as_ref()))
        }
        _ => return Err(crate::errors::Error::rejected("unsupported_operation")),
    };
    let action = action.as_ref();
    if let Action::FocusWindow { window_id } = action {
        let policy = if snapshot.is_some() {
            FocusSpacePolicy::AllowNativeActivation
        } else {
            FocusSpacePolicy::VisibleOnly
        };
        // The retained layout already places this window in a Space, and that
        // placement is what a Bar icon was drawn from or what the caller meant
        // by an id. Handing it over lets the command confirm visibility from
        // one Space's membership instead of a full topology sample; a claim
        // the native reads contradict is still refused there.
        let known_space = windows.find(*window_id).and_then(|(_, entity)| {
            spaces
                .iter()
                .find(|strip| strip.contains(entity))
                .map(LayoutStrip::id)
        });
        return focus_window_command(
            *window_id,
            policy,
            known_space,
            &windows,
            &window_manager,
            &mut topology,
            &mut commands,
        );
    }
    if let Action::FocusWindowInSpace {
        window_id,
        space_id,
    } = action
    {
        // A window in a Space macOS is not showing has its AX surface withdrawn,
        // so it is suspended rather than gone: `find` only answers for live
        // surfaces, and refusing here would refuse exactly the case this action
        // exists for. The identity lookup still knows it, which is all the Space
        // switch needs; the follow waits for the surface to come back.
        let Some((window, entity)) = windows.find_any(*window_id) else {
            warn!(window_id, "window is not tracked");
            return Err(crate::errors::Error::rejected("window_not_found"));
        };
        // The Bar drew this icon from a snapshot, so the window may have moved
        // since. Refuse rather than switch to a Space the window is no longer
        // in: an unasked-for Space switch is worse than nothing happening.
        if let Ok(memberships) = topology.observe_memberships(&window_manager)
            && let Some(observed) = memberships.unique_space(*window_id)
            && observed != *space_id
        {
            warn!(
                window_id,
                space_id, observed, "window is no longer in the Space it was drawn in"
            );
            return Err(crate::errors::Error::rejected("native_precondition_failed"));
        }
        let member = MoveWindowIdentity {
            window_id: *window_id,
            entity,
            incarnation: window.incarnation(),
        };
        if let Some(pending) =
            PendingFollow::submit(member, *space_id, &window_manager, &config, time.elapsed())
        {
            // Only an accepted selection supersedes every older focus intent,
            // including follows still attached to unconfirmed moves.
            transactions.cancel_pending_follows();
            transactions.follows.push(pending);
        } else {
            warn!(
                window_id,
                space_id, "unable to switch to that Space to focus the window"
            );
            return Err(crate::errors::Error::rejected("space_focus_rejected"));
        }
        return Ok(());
    }
    let move_request = match action {
        Action::MoveWindowToSpace {
            window_id,
            space_id,
            move_focus,
        } => Some((*window_id, *space_id, *move_focus, None)),
        Action::MoveColumnToSpace {
            window_id,
            space_id,
            move_focus,
        } => {
            let Some((_, entity)) = windows.find(*window_id) else {
                warn!(window_id, "window is not tracked");
                return Err(crate::errors::Error::rejected("native_precondition_failed"));
            };
            let Some((_, _, state)) = windows.get_tracked(entity) else {
                return Err(crate::errors::Error::rejected("native_precondition_failed"));
            };
            if state.is_floating() {
                warn!(
                    window_id,
                    "a floating window does not belong to a tiled column"
                );
                return Err(crate::errors::Error::rejected("native_precondition_failed"));
            }
            let Some(column) = spaces
                .iter()
                .find_map(|strip| strip.column_containing(entity))
            else {
                warn!(window_id, "window is not in a layout column");
                return Err(crate::errors::Error::rejected("native_precondition_failed"));
            };
            Some((*window_id, *space_id, *move_focus, Some(column)))
        }
        _ => None,
    };
    let Some((window_id, space_id, move_focus, column)) = move_request else {
        let intent = match action {
            Action::FocusSpace { space_id } => Some(NativeSpaceIntent::Focus {
                space_id: *space_id,
                animate: config.space_switch_animation(),
            }),
            Action::CreateSpace { display_id } => Some(NativeSpaceIntent::Create {
                display_id: *display_id,
            }),
            Action::DeleteSpace { space_id } => Some(NativeSpaceIntent::Delete {
                space_id: *space_id,
            }),
            _ => None,
        };
        if let Some(intent) = intent {
            if config.space_control_enabled() {
                match window_manager.perform_native_space_intent(&intent) {
                    Ok(()) => {
                        if matches!(intent, NativeSpaceIntent::Focus { .. }) {
                            transactions.cancel_pending_follows();
                        }
                    }
                    Err(error) => {
                        warn!(?action, %error, "Space capability unavailable");
                        return Err(crate::errors::Error::rejected("native_operation_rejected"));
                    }
                }
            } else {
                warn!(?action, "Space control is disabled");
                return Err(crate::errors::Error::rejected("capability_unavailable"));
            }
            return Ok(());
        }
        return Err(crate::errors::Error::rejected("unsupported_operation"));
    };
    if !config.space_control_enabled() {
        warn!("Space control is disabled; enable experimental_space_control");
        return Err(crate::errors::Error::rejected("capability_unavailable"));
    }
    let observations = window_manager.observe_displays().map_err(|error| {
        warn!(space_id, %error, "unable to read the display inventory");
        crate::errors::Error::rejected("native_precondition_failed")
    })?;
    // A display whose Space list could not be read cannot confirm the target,
    // but its failure is not evidence that the target Space does not exist.
    let target_is_known = observations.iter().any(|observation| {
        observation
            .spaces
            .as_ref()
            .is_ok_and(|spaces| spaces.contains(&space_id))
    });
    if !target_is_known || window_manager.workspace_is_fullscreen(space_id) {
        warn!(space_id, "target is not a known user Space");
        return Err(crate::errors::Error::rejected("native_precondition_failed"));
    }
    if windows.find(window_id).is_none() {
        warn!(window_id, "window is not tracked");
        return Err(crate::errors::Error::rejected("window_not_found"));
    }
    let members = column.as_ref().map_or_else(
        || {
            windows
                .find(window_id)
                .map(|(_, entity)| vec![entity])
                .unwrap_or_default()
        },
        |column| column.window_iter().collect(),
    );
    let mut window_ids = Vec::new();
    if members
        .iter()
        .any(|member| windows.get_tracked(*member).is_none())
    {
        warn!(window_id, "column has unavailable members");
        return Err(crate::errors::Error::rejected("native_precondition_failed"));
    }
    for member in members {
        let Some((window, _, _)) = windows.get_tracked(member) else {
            continue;
        };
        let member_id = window.id();
        window_ids.extend(window_manager.get_associated_windows(member_id));
        window_ids.push(member_id);
    }
    window_ids.sort_unstable();
    window_ids.dedup();
    if window_ids.iter().any(|id| {
        windows
            .find_any(*id)
            .is_some_and(|(_, entity)| !windows.is_available(entity))
    }) {
        warn!(window_id, "associated window is temporarily unavailable");
        return Err(crate::errors::Error::rejected("native_precondition_failed"));
    }
    if snapshot.is_some_and(|snapshot| {
        window_ids
            .iter()
            .any(|id| !matches_window(snapshot, *id, &windows))
    }) {
        debug!(
            window_id,
            "skipping script move with unbound or replaced associated windows"
        );
        return Err(crate::errors::Error::rejected("native_precondition_failed"));
    }
    if transactions
        .moves
        .iter()
        .any(|pending| pending.window_ids.iter().any(|id| window_ids.contains(id)))
    {
        warn!(window_id, "window already has a pending native move");
        return Err(crate::errors::Error::rejected("native_move_pending"));
    }
    let members = window_ids
        .iter()
        .filter_map(|id| {
            windows
                .find(*id)
                .map(|(window, entity)| MoveWindowIdentity {
                    window_id: *id,
                    entity,
                    incarnation: window.incarnation(),
                })
        })
        .collect::<Vec<_>>();
    let follow = members
        .iter()
        .find(|member| member.window_id == window_id)
        .copied()
        .filter(|member| {
            move_focus == MoveFocus::Follow
                && windows
                    .get_tracked(member.entity)
                    .is_some_and(|(_, _, state)| state.is_visible())
        });
    let already_at_target = topology
        .observe_memberships(&window_manager)
        .is_ok_and(|membership| {
            window_ids
                .iter()
                .all(|id| membership.unique_space(*id) == Some(space_id))
        });
    let fully_reconciled = already_at_target
        && members.iter().all(|member| {
            windows
                .get_tracked(member.entity)
                .is_some_and(|(_, _, state)| {
                    let mut owners = spaces.iter().filter(|strip| strip.contains(member.entity));
                    if state.is_floating() {
                        owners.next().is_none()
                    } else if !state.is_visible() {
                        owners.next().is_none()
                            && previous_strips
                                .get(member.entity)
                                .is_ok_and(|previous| previous.workspace_id == space_id)
                    } else {
                        owners.next().is_some_and(|strip| strip.id() == space_id)
                            && owners.next().is_none()
                    }
                })
        });
    if fully_reconciled {
        if let Some(member) = follow {
            if let Some(pending) =
                PendingFollow::submit(member, space_id, &window_manager, &config, time.elapsed())
            {
                transactions.cancel_pending_follows();
                transactions.follows.push(pending);
            } else {
                return Err(crate::errors::Error::rejected("space_focus_rejected"));
            }
        } else {
            transactions.cancel_follows_for(&window_ids);
        }
        return Ok(());
    }
    let intent = NativeSpaceIntent::MoveWindows {
        window_ids: window_ids.clone(),
        space_id,
    };
    let submitted = if already_at_target {
        Ok(())
    } else {
        window_manager.perform_native_space_intent(&intent)
    };
    match submitted {
        Ok(()) => {
            if follow.is_some() {
                transactions.cancel_pending_follows();
            } else {
                transactions.cancel_follows_for(&window_ids);
            }
            for member in &members {
                let entity = member.entity;
                let index = spaces
                    .iter()
                    .find_map(|strip| strip.index_of(entity).ok())
                    .or_else(|| {
                        previous_strips
                            .get(entity)
                            .ok()
                            .map(|previous| previous.index)
                    })
                    .unwrap_or_default();
                freeze_window_for_space_reassignment(entity, index, &mut commands);
                commands.entity(entity).insert(NativeMoveOwner);
            }
            let layout = column.map_or(PendingMoveLayout::AssociatedWindows, |column| {
                PendingMoveLayout::capture_column(column, space_id, spaces.iter(), &members)
            });
            transactions.moves.push(PendingMove {
                window_ids,
                members,
                target_space_id: space_id,
                follow,
                layout,
                submitted: time.elapsed(),
                display: None,
            });
        }
        Err(error) => {
            warn!(%error, "Space operation rejected");
            return Err(crate::errors::Error::rejected("native_operation_rejected"));
        }
    }
    Ok(())
}

#[derive(SystemParam)]
pub(crate) struct NativeSpaceReconciliationCtx<'w, 's> {
    window_manager: Res<'w, WindowManager>,
    windows: Windows<'w, 's>,
    config: Res<'w, Config>,
    spaces: Query<'w, 's, (&'static mut LayoutStrip, Has<PendingSpaceDestruction>)>,
    invisible: Query<'w, 's, (), With<WindowVisibility>>,
    reassignments: Query<'w, 's, &'static WindowSpaceReassignmentPending>,
    display_readbacks: Query<'w, 's, &'static DisplayTransferReadback>,
    displays: Query<'w, 's, (&'static Display, Option<&'static crate::ecs::DockPosition>)>,
    commands: Commands<'w, 's>,
    transactions: bevy::ecs::system::ResMut<'w, NativeSpaceTransactions>,
    time: Res<'w, Time>,
    topology: Res<'w, NativeTopology>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn reconcile_native_space_transactions(ctx: NativeSpaceReconciliationCtx) {
    const MOVE_TIMEOUT: Duration = Duration::from_secs(2);
    const FOLLOW_TIMEOUT: Duration = Duration::from_secs(5);
    let NativeSpaceReconciliationCtx {
        window_manager,
        windows,
        config,
        mut spaces,
        invisible,
        reassignments,
        display_readbacks,
        displays,
        mut commands,
        mut transactions,
        time,
        topology,
    } = ctx;
    let mut new_follows = Vec::new();
    let mut completed = Vec::new();
    let mut expired = Vec::new();
    if transactions.moves.is_empty() && transactions.follows.is_empty() {
        return;
    }
    let memberships = topology.observe_memberships(&window_manager).ok();
    let belongs_to = |id, space| {
        memberships
            .as_ref()
            .is_some_and(|members| members.unique_space(id) == Some(space))
    };
    transactions.moves.retain_mut(|pending| {
        if (pending.display.is_none() && !config.space_control_enabled())
            || pending
                .follow
                .is_some_and(|member| invisible.contains(member.entity))
        {
            pending.follow = None;
        }
        let timed_out = time.elapsed().saturating_sub(pending.submitted) >= MOVE_TIMEOUT;
        if pending
            .members
            .iter()
            .any(|member| !member.is_current(&windows))
        {
            expired.extend(pending.members.iter().map(|member| member.entity));
            warn!(
                space_id = pending.target_space_id,
                "native move member identity retired"
            );
            return false;
        }
        let target_ready = !topology.is_fullscreen(pending.target_space_id)
            && pending.display.as_ref().is_none_or(|display| {
                topology.visible_display_for_space(pending.target_space_id)
                    == Some(display.target_display_id)
            })
            && spaces
                .iter()
                .any(|(strip, retiring)| strip.id() == pending.target_space_id && !retiring);
        if target_ready
            && pending
                .members
                .iter()
                .all(|member| member.is_available(&windows))
            && pending
                .window_ids
                .iter()
                .all(|id| belongs_to(*id, pending.target_space_id))
        {
            let moved_entities = pending
                .members
                .iter()
                .map(|member| member.entity)
                .collect::<Vec<_>>();
            let tiled = moved_entities
                .iter()
                .copied()
                .filter(|entity| {
                    windows
                        .get_tracked(*entity)
                        .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
                })
                .collect::<Vec<_>>();
            let mut target_anchor = None;
            for (mut strip, _) in &mut spaces {
                if strip.id() == pending.target_space_id {
                    for entity in &moved_entities {
                        if !tiled.contains(entity) {
                            strip.remove(*entity);
                        }
                    }
                    if tiled.iter().all(|entity| strip.contains(*entity)) {
                        continue;
                    }
                    match &pending.layout {
                        PendingMoveLayout::AssociatedWindows => {
                            strip.append_tab_group(&tiled);
                            target_anchor = tiled.first().copied();
                        }
                        PendingMoveLayout::Columns(_) => {
                            let mut projection = pending
                                .layout
                                .tiled_projection(pending.target_space_id, &tiled);
                            target_anchor = projection.columns().next().and_then(Column::top);
                            for entity in projection.all_windows() {
                                strip.remove(entity);
                            }
                            strip.append_strip(&mut projection);
                        }
                    }
                } else {
                    for entity in &moved_entities {
                        strip.remove(*entity);
                    }
                }
            }
            if let Some(anchor) = target_anchor {
                commands.reshuffle_around(anchor);
            }
            for &entity in &moved_entities {
                let Some((window, _, state)) = windows.get_tracked(entity) else {
                    continue;
                };
                if pending.display.is_some()
                    && let Ok(readback) = display_readbacks.get(entity)
                    && readback.incarnation == window.incarnation()
                    && readback.tiled == state.is_tiled()
                {
                    commands
                        .entity(entity)
                        .insert((
                            Position(readback.frame.min),
                            Bounds(readback.frame.size()),
                            DesiredWindowFrame(readback.frame),
                            PresentedWindowFrame(readback.frame),
                        ))
                        .remove::<(WindowFrameMotion, WindowFrameCommitSuspended)>();
                }
                if state.is_tiled() && !state.is_visible() {
                    let index = reassignments
                        .get(entity)
                        .map_or(0, WindowSpaceReassignmentPending::source_index);
                    commands.entity(entity).insert(PreviousTiledStrip {
                        workspace_id: pending.target_space_id,
                        index,
                    });
                } else if state.is_tiled() {
                    commands.entity(entity).remove::<PreviousTiledStrip>();
                }
            }
            completed.extend_from_slice(&moved_entities);
            debug!(
                space_id = pending.target_space_id,
                windows = ?pending.window_ids,
                "window-to-Space operation reconciled"
            );
            if let Some(display) = &mut pending.display {
                if let Some(member) = pending.follow {
                    new_follows.push(PendingFollow {
                        member,
                        target_space_id: pending.target_space_id,
                        submitted: time.elapsed(),
                        visible_display: Some(display.target_display_id),
                        warp_pointer: true,
                    });
                } else if let Some(mut follow) = display.source_follow.take() {
                    follow.submitted = time.elapsed();
                    new_follows.push(follow);
                }
            } else if let Some(member) = pending.follow
                && let Some(follow) = PendingFollow::submit(
                    member,
                    pending.target_space_id,
                    &window_manager,
                    &config,
                    time.elapsed(),
                )
            {
                new_follows.push(follow);
            }
            false
        } else if timed_out {
            expired.extend(pending.members.iter().map(|member| member.entity));
            warn!(
                space_id = pending.target_space_id,
                windows = ?pending.window_ids,
                "window-to-Space operation timed out without reconciliation"
            );
            false
        } else {
            true
        }
    });
    for entity in completed {
        if let Ok(mut entity) = commands.get_entity(entity) {
            entity.try_remove::<(
                NativeMoveOwner,
                WindowSpaceReassignmentPending,
                DisplayTransferFrame,
                DisplayTransferReadback,
            )>();
        }
    }
    // A timeout releases ownership, not the geometry barrier. The existing
    // membership audit resolves the actual destination before resuming writes.
    for entity in expired {
        if let Ok(mut entity) = commands.get_entity(entity) {
            entity.try_remove::<(
                NativeMoveOwner,
                DisplayTransferFrame,
                DisplayTransferReadback,
            )>();
        }
    }
    transactions.follows.extend(new_follows);
    transactions.follows.retain(|pending| {
        if (pending.visible_display.is_none() && !config.space_control_enabled())
            || !pending.member.is_current(&windows)
            || invisible.contains(pending.member.entity)
            || time.elapsed().saturating_sub(pending.submitted) >= FOLLOW_TIMEOUT
        {
            return false;
        }
        let target_visible = topology
            .visible_display_for_space(pending.target_space_id)
            .is_some_and(|display| {
                pending
                    .visible_display
                    .is_none_or(|expected| expected == display)
            })
            && spaces
                .iter()
                .any(|(strip, retiring)| strip.id() == pending.target_space_id && !retiring);
        if target_visible
            && pending.member.is_available(&windows)
            && belongs_to(pending.member.window_id, pending.target_space_id)
        {
            if pending.warp_pointer {
                let Some(viewport) = displays.iter().find_map(|(display, dock)| {
                    (Some(display.id()) == pending.visible_display
                        && topology.known_displays().any(|(native, _)| {
                            native.id() == display.id() && !display.clone().update_geometry(native)
                        }))
                    .then(|| display.checked_actual_display_bounds(dock, &config))
                    .flatten()
                }) else {
                    return true;
                };
                window_manager.warp_mouse(Origin::new(
                    viewport.min.x.midpoint(viewport.max.x),
                    viewport.min.y.midpoint(viewport.max.y),
                ));
            }
            commands.trigger(super::focus::FocusWindow {
                entity: pending.member.entity,
                raise: pending.visible_display.is_none() || pending.warp_pointer,
                kind: super::focus::FocusRequestKind::Automatic,
                allow_native_activation: false,
                native_space: Some(pending.target_space_id),
            });
            false
        } else {
            true
        }
    });
}

type ObservedNativeSpaces<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LayoutStrip,
        Option<&'static ChildOf>,
        Option<&'static mut NativeSpace>,
        Has<VisibleNativeSpaceMarker>,
        Has<ActiveWorkspaceMarker>,
        Has<PendingSpaceDestruction>,
        Has<FloatingLayer>,
    ),
>;

#[derive(SystemParam)]
pub(crate) struct NativeSpaceObservationCtx<'w, 's> {
    displays: Query<'w, 's, (&'static Display, Entity, Has<ActiveDisplayMarker>)>,
    spaces: ObservedNativeSpaces<'w, 's>,
    topology: Res<'w, super::topology::NativeTopology>,
    generation: Local<'s, u64>,
    commands: Commands<'w, 's>,
}

struct DisplaySpaceProjection<'a> {
    display: &'a Display,
    display_entity: Entity,
    display_active: bool,
    topology: &'a [WorkspaceId],
    visible_id: WorkspaceId,
}

fn reconcile_display_space_projections(
    projection: DisplaySpaceProjection<'_>,
    spaces: &mut ObservedNativeSpaces,
    observation: &super::topology::NativeTopology,
    commands: &mut Commands,
) {
    let DisplaySpaceProjection {
        display,
        display_entity,
        display_active,
        topology,
        visible_id,
    } = projection;
    for (ordinal, space_id) in topology.iter().copied().enumerate() {
        let observed = NativeSpace::new(space_id, ordinal, observation.is_fullscreen(space_id));
        let should_be_visible = space_id == visible_id;
        let should_be_active = display_active && should_be_visible;
        let mut found = false;
        let mut tombstoned = false;

        for (entity, strip, child, native, visible, active, pending, has_layer) in spaces.iter_mut()
        {
            if strip.id() != space_id {
                continue;
            }
            if pending {
                tombstoned = true;
                continue;
            }
            if found {
                continue;
            }
            found = true;

            if let Some(mut native) = native {
                if *native != observed {
                    *native = observed;
                }
            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(observed);
            }
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                if !has_layer {
                    entity_commands.try_insert(FloatingLayer::default());
                }
                if child.is_none_or(|child| child.parent() != display_entity) {
                    entity_commands
                        .try_remove::<DetachedSpace>()
                        .try_insert((ChildOf(display_entity), RefreshWindowSizes::default()));
                }
                if should_be_visible && !visible {
                    entity_commands.try_insert(VisibleNativeSpaceMarker);
                } else if !should_be_visible && visible {
                    entity_commands.try_remove::<VisibleNativeSpaceMarker>();
                }
                if should_be_active && !active {
                    entity_commands.try_insert(ActiveWorkspaceMarker);
                } else if !should_be_active && active {
                    entity_commands.try_remove::<ActiveWorkspaceMarker>();
                }
            }
        }

        if !found && !tombstoned {
            let display_id = display.id();
            debug!(space_id, display_id, "projecting new Space");
            let mut spawned = commands.spawn_layout_strip(
                LayoutStrip::new(space_id),
                display.bounds().min,
                display_entity,
                should_be_active,
            );
            spawned.try_insert((observed, FloatingLayer::default()));
            if should_be_visible {
                spawned.try_insert(VisibleNativeSpaceMarker);
            }
        }
    }
}

/// Reconciles read-only Space topology and per-display visibility from
/// macOS. This system never changes Spaces or moves windows between them.
#[instrument(level = tracing::Level::DEBUG, skip_all)]
pub(crate) fn reconcile_native_spaces(ctx: NativeSpaceObservationCtx) {
    let NativeSpaceObservationCtx {
        displays,
        mut spaces,
        topology: observation,
        mut generation,
        mut commands,
    } = ctx;
    if *generation == observation.generation() {
        return;
    }
    *generation = observation.generation();

    let topology_by_display = observation
        .known_displays()
        .map(|(display, spaces)| (display.id(), spaces))
        .collect::<HashMap<_, _>>();

    for (display, display_entity, marker_active) in &displays {
        let display_id = display.id();
        // macOS moves the menu bar (the active display) to whichever display
        // owns the key window, but `AppKit` posts no notification for that, so
        // the marker has to follow our own observation. Deriving the active
        // Space from a notification-only marker leaves it pinned to the display
        // that was active at launch: the next sample would then withdraw the
        // active Space from the display the user just clicked and re-focus the
        // remembered window of the display they left.
        let display_active = match observation.active_display() {
            Some(active_display_id) => active_display_id == display_id,
            // An unavailable read is not evidence that the display changed.
            None => marker_active,
        };
        if display_active != marker_active
            && let Ok(mut entity_commands) = commands.get_entity(display_entity)
        {
            if display_active {
                debug!(display_id, "display became active");
                entity_commands.try_insert(ActiveDisplayMarker);
            } else {
                debug!(display_id, "display is no longer active");
                entity_commands.try_remove::<ActiveDisplayMarker>();
            }
        }
        let Some(visible_id) = observation.visible_space(display_id) else {
            error!(display_id, "unable to read visible Space");
            continue;
        };
        let Some(topology) = topology_by_display.get(&display_id) else {
            warn!(display_id, "Space topology unavailable");
            continue;
        };
        reconcile_display_space_projections(
            DisplaySpaceProjection {
                display,
                display_entity,
                display_active,
                topology,
                visible_id,
            },
            &mut spaces,
            &observation,
            &mut commands,
        );

        debug!(
            generation = *generation,
            display_id,
            visible_space_id = visible_id,
            spaces = ?topology,
            "observed Space topology"
        );
    }
}

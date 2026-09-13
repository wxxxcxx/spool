//! One observation epoch for display and native Space lifecycle projections.
//! Unknown reads stay unknown; consumers may retain their ECS projection but
//! must not use a previous successful sample as evidence of current membership.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use tracing::warn;

use crate::commands::Action;
use crate::errors::{Error, Result};
use crate::events::Event;
use crate::manager::{Display, DisplayObservation, WindowManager};
use crate::platform::{WinID, WorkspaceId};

/// Membership from one complete scan. Absence and overlapping memberships do
/// not establish a destination; a failed scan never publishes a partial map.
#[derive(Debug)]
pub(crate) struct WindowMemberships {
    by_window: HashMap<WinID, Option<WorkspaceId>>,
    by_space: HashMap<WorkspaceId, Vec<WinID>>,
}

impl WindowMemberships {
    pub(crate) fn unique_space(&self, window_id: WinID) -> Option<WorkspaceId> {
        self.by_window.get(&window_id).copied().flatten()
    }

    /// Every window macOS lists in this Space, including ones another Space
    /// also lists.
    ///
    /// [`Self::windows_in_space`] answers a different question: it removes
    /// overlapping memberships, which is what a destination needs but not what
    /// a projection of the Space's own contents needs.
    pub(crate) fn listed_in(&self, space: WorkspaceId) -> impl Iterator<Item = WinID> + '_ {
        self.by_space.get(&space).into_iter().flatten().copied()
    }

    /// Unique members in the native list's order, with duplicate entries removed.
    pub(crate) fn windows_in_space(&self, space: WorkspaceId) -> impl Iterator<Item = WinID> + '_ {
        self.by_space
            .get(&space)
            .into_iter()
            .flatten()
            .copied()
            .filter(move |&window| self.unique_space(window) == Some(space))
    }
}

/// The answer to a caller's claim that a window sits in a Space that is
/// currently on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpaceClaim {
    /// The Space is the one its display is showing, and the window is in it.
    Confirmed,
    /// Current native evidence did not bear the claim out: the Space is not
    /// the visible one, its display ownership is ambiguous, or the window is
    /// not in it. A retained layout may simply be behind the window's real
    /// Space, so this is not yet a refusal.
    Refused,
    /// An inventory or membership read the answer depends on failed. The full
    /// observation refuses on the same evidence, so there is nothing to fall
    /// back to.
    Unavailable,
}

#[derive(Default, Resource)]
pub(crate) struct NativeTopology {
    generation: u64,
    inventory: Option<Result<Vec<DisplayObservation>>>,
    visible: HashMap<u32, Result<WorkspaceId>>,
    fullscreen: HashSet<WorkspaceId>,
    active_display: Option<u32>,
    explicit_removal: bool,
}

impl NativeTopology {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn displays(&self) -> Option<&[DisplayObservation]> {
        let displays = self.inventory.as_ref()?.as_ref().ok()?;
        (!displays.is_empty() || self.explicit_removal).then_some(displays.as_slice())
    }

    pub(crate) fn known_displays(&self) -> impl Iterator<Item = (&Display, &[WorkspaceId])> {
        self.displays().into_iter().flatten().filter_map(|entry| {
            entry
                .spaces
                .as_ref()
                .ok()
                .map(|spaces| (&entry.display, spaces.as_slice()))
        })
    }

    /// All physical displays and their Space IDs are known. Visibility is a
    /// separate observation and is not needed for unique membership checks.
    pub(crate) fn is_complete(&self) -> bool {
        self.displays()
            .is_some_and(|displays| displays.iter().all(|entry| entry.spaces.is_ok()))
    }

    pub(crate) fn observe_memberships(&self, manager: &WindowManager) -> Result<WindowMemberships> {
        if !self.is_complete() {
            return Err(Error::Generic("native Space topology unavailable".into()));
        }
        let mut spaces = self
            .known_displays()
            .flat_map(|(_, spaces)| spaces.iter().copied())
            .collect::<Vec<_>>();
        spaces.sort_unstable();
        spaces.dedup();
        let mut by_window = HashMap::new();
        let mut by_space = HashMap::<WorkspaceId, Vec<WinID>>::new();
        for space in spaces {
            for window_id in manager.windows_in_workspace(space)? {
                // The per-Space list keeps every Space that lists the window,
                // so a projection can ask what this Space holds; readers that
                // need an unambiguous destination filter it themselves.
                let members = by_space.entry(space).or_default();
                if !members.contains(&window_id) {
                    members.push(window_id);
                }
                by_window
                    .entry(window_id)
                    .and_modify(|previous| {
                        if *previous != Some(space) {
                            *previous = None;
                        }
                    })
                    .or_insert(Some(space));
            }
        }
        Ok(WindowMemberships {
            by_window,
            by_space,
        })
    }

    pub(crate) fn visible_space(&self, display_id: u32) -> Option<WorkspaceId> {
        self.visible.get(&display_id)?.as_ref().ok().copied()
    }

    pub(crate) fn visible_display_for_space(&self, space_id: WorkspaceId) -> Option<u32> {
        if !self.is_complete() {
            return None;
        }
        let mut owners = self
            .known_displays()
            .filter(|(_, spaces)| spaces.contains(&space_id));
        let (display, _) = owners.next()?;
        (owners.next().is_none() && self.visible_space(display.id()) == Some(space_id))
            .then_some(display.id())
    }

    /// Confirms a caller's claim that `window_id` sits in `space_id` and that
    /// macOS is showing that Space.
    ///
    /// The first two steps are the full observation's own: a current display
    /// inventory and the Space's unique visible display. The last step reads
    /// that one Space's membership instead of every Space's, which is what a
    /// Bar click can afford to pay.
    pub(crate) fn confirm_visible_window_space(
        &mut self,
        manager: &WindowManager,
        window_id: WinID,
        space_id: WorkspaceId,
    ) -> SpaceClaim {
        if !self.refresh_for_command(manager) {
            return SpaceClaim::Unavailable;
        }
        if self.visible_display_for_space(space_id).is_none() {
            return SpaceClaim::Refused;
        }
        match manager.windows_in_workspace(space_id) {
            Ok(ids) if ids.contains(&window_id) => SpaceClaim::Confirmed,
            Ok(_) => SpaceClaim::Refused,
            Err(_) => SpaceClaim::Unavailable,
        }
    }

    /// A preceding command may already have changed native visibility in this batch.
    pub(crate) fn observe_visible_window_space(
        &mut self,
        manager: &WindowManager,
        window_id: WinID,
    ) -> Option<WorkspaceId> {
        self.refresh_for_command(manager);
        let space_id = self
            .observe_memberships(manager)
            .ok()?
            .unique_space(window_id)?;
        self.visible_display_for_space(space_id)?;
        Some(space_id)
    }

    pub(crate) fn refresh_for_command(&mut self, manager: &WindowManager) -> bool {
        self.sample(manager, false);
        self.is_complete()
    }

    pub(crate) fn active_display(&self) -> Option<u32> {
        self.active_display
    }

    pub(crate) fn is_fullscreen(&self, space_id: WorkspaceId) -> bool {
        self.fullscreen.contains(&space_id)
    }

    fn sample(&mut self, manager: &WindowManager, explicit_removal: bool) {
        self.generation = self.generation.wrapping_add(1);
        self.explicit_removal = explicit_removal;
        self.visible.clear();
        self.fullscreen.clear();
        self.active_display = manager.active_display_id().ok();
        self.inventory = Some(manager.observe_displays().inspect_err(|error| {
            warn!(%error, "native topology inventory unavailable");
        }));
        if let Some(Ok(displays)) = &self.inventory {
            for entry in displays {
                self.visible.insert(
                    entry.display.id(),
                    manager.active_display_space(entry.display.id()),
                );
                if let Ok(spaces) = &entry.spaces {
                    self.fullscreen.extend(
                        spaces
                            .iter()
                            .copied()
                            .filter(|id| manager.workspace_is_fullscreen(*id)),
                    );
                }
            }
        }
    }
}

pub(crate) fn invalidates_topology(event: &Event) -> bool {
    matches!(
        event,
        Event::SpaceChanged
            | Event::LayoutSpaceRequested { .. }
            | Event::SpaceCreated { .. }
            | Event::SpaceDestroyed { .. }
            | Event::SystemWoke { .. }
            | Event::DisplayAdded { .. }
            | Event::DisplayRemoved { .. }
            | Event::DisplayMoved { .. }
            | Event::DisplayResized { .. }
            | Event::DisplayConfigured { .. }
            | Event::DisplayChanged
            | Event::ActionRequested {
                action: Action::FocusSpace { .. }
                    | Action::CreateSpace { .. }
                    | Action::DeleteSpace { .. }
                    | Action::MoveWindowToSpace { .. }
                    | Action::MoveColumnToSpace { .. }
            }
    )
}

pub(crate) fn gather_initial_topology(
    manager: Res<WindowManager>,
    mut topology: ResMut<NativeTopology>,
) {
    topology.sample(&manager, false);
}

pub(crate) fn refresh_topology(
    manager: Res<WindowManager>,
    mut topology: ResMut<NativeTopology>,
    mut events: MessageReader<Event>,
    time: Res<Time>,
    mut since_audit: Local<Duration>,
) {
    const HEARTBEAT: Duration = Duration::from_secs(1);
    let mut invalidated = false;
    let mut explicit_removal = false;
    for event in events.read() {
        invalidated |= invalidates_topology(event);
        explicit_removal |= matches!(event, Event::DisplayRemoved { .. });
    }
    *since_audit = since_audit.saturating_add(time.delta());
    if topology.generation() != 0 && !invalidated && *since_audit < HEARTBEAT {
        return;
    }
    *since_audit = Duration::ZERO;
    topology.sample(&manager, explicit_removal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::MockState;

    fn membership_fixture(spaces: Vec<WorkspaceId>) -> (NativeTopology, WindowManager, MockState) {
        let mut state = MockState::new();
        state.add_display(1, IRect::new(0, 0, 1000, 800), spaces);
        let manager = WindowManager(Box::new(state.create_window_manager()));
        let mut topology = NativeTopology::default();
        topology.sample(&manager, false);
        (topology, manager, state)
    }

    #[test]
    fn membership_scan_deduplicates_spaces_and_keeps_overlap_ambiguous() {
        let (topology, manager, state) = membership_fixture(vec![1, 2, 2, 3]);
        state.script_workspace_membership_queries(1, [Ok(vec![10, 10, 30])]);
        state.script_workspace_membership_queries(2, [Ok(vec![10, 20]), Err(())]);
        state.script_workspace_membership_queries(3, [Ok(vec![10, 40])]);
        let memberships = topology.observe_memberships(&manager).unwrap();
        assert_eq!(memberships.unique_space(10), None);
        assert_eq!(memberships.unique_space(20), Some(2));
        assert_eq!(memberships.unique_space(30), Some(1));
        assert_eq!(memberships.unique_space(40), Some(3));
        assert_eq!(memberships.unique_space(99), None);
        assert_eq!(
            memberships.windows_in_space(1).collect::<Vec<_>>(),
            vec![30]
        );
        assert_eq!(
            memberships.windows_in_space(2).collect::<Vec<_>>(),
            vec![20]
        );
        assert_eq!(
            memberships.windows_in_space(3).collect::<Vec<_>>(),
            vec![40]
        );
        assert!(
            manager.windows_in_workspace(2).is_err(),
            "each Space is read only once"
        );
    }

    #[test]
    fn membership_scan_rejects_partial_reads_without_reusing_previous_results() {
        let (topology, manager, state) = membership_fixture(vec![1, 2]);
        state.script_workspace_membership_queries(1, [Ok(vec![10]), Ok(vec![10]), Ok(vec![20])]);
        state.script_workspace_membership_queries(2, [Ok(vec![]), Err(()), Ok(vec![])]);
        assert_eq!(
            topology
                .observe_memberships(&manager)
                .unwrap()
                .unique_space(10),
            Some(1)
        );
        assert!(topology.observe_memberships(&manager).is_err());
        let recovered = topology.observe_memberships(&manager).unwrap();
        assert_eq!(recovered.unique_space(10), None);
        assert_eq!(recovered.unique_space(20), Some(1));
    }

    #[test]
    fn membership_scan_waits_for_complete_topology() {
        let (mut topology, manager, state) = membership_fixture(vec![1]);
        state.script_present_display_topology_queries(1, [Err(())]);
        topology.sample(&manager, false);
        state.script_workspace_membership_queries(1, [Ok(vec![10]), Err(())]);
        assert!(topology.observe_memberships(&manager).is_err());
        assert_eq!(manager.windows_in_workspace(1).unwrap(), vec![10]);
        topology.sample(&manager, false);
        state.script_workspace_membership_queries(1, [Ok(vec![10])]);
        assert_eq!(
            topology
                .observe_memberships(&manager)
                .unwrap()
                .unique_space(10),
            Some(1)
        );
    }

    /// Interface contract: a Space holding no windows is a successful empty
    /// observation, not a failed read. Only an unreadable Space may make the
    /// membership scan unavailable. This locks both adapters to the same
    /// answer, because a scan that treats an empty Space as a failure aborts
    /// every other Space's membership with it.
    #[test]
    fn empty_space_is_an_observation_not_a_failed_read() {
        let (topology, manager, state) = membership_fixture(vec![1, 2]);
        state.script_workspace_membership_queries(1, [Ok(vec![10])]);
        state.script_workspace_membership_queries(2, [Ok(vec![]), Ok(vec![])]);

        let memberships = topology.observe_memberships(&manager).unwrap();
        assert_eq!(memberships.unique_space(10), Some(1));
        assert_eq!(memberships.listed_in(2).count(), 0);
        let empty = manager
            .windows_in_workspace(2)
            .expect("an empty Space is a successful read, not a failure");
        assert!(empty.is_empty());

        // The same scan still refuses when a read genuinely fails.
        state.script_workspace_membership_queries(1, [Err(())]);
        assert!(topology.observe_memberships(&manager).is_err());
    }
}

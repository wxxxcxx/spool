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
}

impl WindowMemberships {
    pub(crate) fn unique_space(&self, window_id: WinID) -> Option<WorkspaceId> {
        self.by_window.get(&window_id).copied().flatten()
    }
}

#[derive(Default, Resource)]
pub(crate) struct NativeTopology {
    generation: u64,
    inventory: Option<Result<Vec<DisplayObservation>>>,
    visible: HashMap<u32, Result<WorkspaceId>>,
    fullscreen: HashSet<WorkspaceId>,
    active_display: Option<u32>,
    explicit_removal: bool,
    refresh_requested: bool,
}

impl NativeTopology {
    pub(crate) fn request_refresh(&mut self) {
        self.refresh_requested = true;
    }

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
        for space in spaces {
            for window_id in manager.windows_in_workspace(space)? {
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
        Ok(WindowMemberships { by_window })
    }

    pub(crate) fn visible_space(&self, display_id: u32) -> Option<WorkspaceId> {
        self.visible.get(&display_id)?.as_ref().ok().copied()
    }

    pub(crate) fn active_display(&self) -> Option<u32> {
        self.active_display
    }

    pub(crate) fn is_fullscreen(&self, space_id: WorkspaceId) -> bool {
        self.fullscreen.contains(&space_id)
    }

    fn sample(&mut self, manager: &WindowManager, explicit_removal: bool) {
        self.refresh_requested = false;
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
    if topology.generation() != 0
        && !topology.refresh_requested
        && !invalidated
        && *since_audit < HEARTBEAT
    {
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
}

//! One observation epoch for display and native Space lifecycle projections.
//! Unknown reads stay unknown; consumers may retain their ECS projection but
//! must not use a previous successful sample as evidence of current membership.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use tracing::warn;

use crate::commands::Action;
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Display, DisplayObservation, WindowManager};
use crate::platform::WorkspaceId;

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

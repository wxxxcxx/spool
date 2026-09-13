use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use bevy::prelude::*;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::CGDirectDisplayID;
use stdext::prelude::RwLockExt;

use crate::errors::Error;
use crate::events::{DestroySource, Event};
use crate::manager::app::MockApplicationApi;
use crate::manager::{
    Application, Display, DisplayObservation, MockProcessApi, MockWindowApi, MockWindowManagerApi,
    Origin, Size, Window, WindowPadding, origin_from, origin_to,
};
use crate::platform::{Modifiers, Pid, ProcessSerialNumber, WinID, WorkspaceId};

use super::*;

/// Data for a mocked application.
pub(crate) struct MockAppData {
    pub(crate) psn: ProcessSerialNumber,
    pub(crate) bundle_id: String,
    pub(crate) name: String,
    pub(crate) focused_window_id: Option<WinID>,
    pub(crate) is_frontmost: bool,
    pub(crate) connection: Option<crate::platform::ConnID>,
    pub(crate) running: bool,
    pub(crate) ready: bool,
}

/// Data for a mocked window.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct MockWindowData {
    pub(crate) id: WinID,
    pub(crate) incarnation: crate::platform::WindowIncarnation,
    pub(crate) pid: Pid,
    pub(crate) frame: IRect,
    pub(crate) title: String,
    pub(crate) minimized: bool,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) visible: bool,
    pub(crate) ordered_out: bool,
    pub(crate) published: bool,
    pub(crate) represented_window_id: Option<WinID>,
    pub(crate) role: String,
    pub(crate) subrole: String,
    pub(crate) is_full_screen: bool,
    pub(crate) resizable: bool,
    pub(crate) movable: bool,
    pub(crate) capabilities_available: bool,
    pub(crate) border_radius: Option<f64>,
    pub(crate) horizontal_padding: i32,
    pub(crate) vertical_padding: i32,
    pub(crate) child_role: bool,
    pub(crate) default_floating: bool,
}

impl Default for MockWindowData {
    fn default() -> Self {
        Self {
            id: 0,
            incarnation: 0,
            pid: 0,
            frame: IRect::default(),
            title: String::new(),
            minimized: false,
            workspace_id: 0,
            visible: true,
            ordered_out: false,
            published: true,
            represented_window_id: None,
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            is_full_screen: false,
            resizable: true,
            movable: true,
            capabilities_available: true,
            border_radius: None,
            horizontal_padding: 0,
            vertical_padding: 0,
            child_role: false,
            default_floating: false,
        }
    }
}

/// Data for a mocked display.
struct MockDisplayData {
    bounds: IRect,
    workspaces: Vec<WorkspaceId>,
}

/// The internal state of our "Virtual macOS".
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent simulated capabilities and inventory failure switches"
)]
struct MockStateInner {
    apps: HashMap<Pid, MockAppData>,
    windows: HashMap<WinID, MockWindowData>,
    /// Last frame observed through the mocked AX handle. `windows[*].frame`
    /// is the `WindowServer` truth; keeping the two separate lets tests model a
    /// lost move/resize notification and a stale optimistic cache.
    cached_frames: HashMap<WinID, IRect>,
    displays: HashMap<u32, MockDisplayData>,
    display_observation_count: usize,
    workspace_membership_query_count: usize,
    display_inventory_available: bool,
    fullscreen_spaces: HashSet<WorkspaceId>,
    active_display_id: u32,
    cursor_position: Origin,
    event_queue: VecDeque<Event>,
    /// Windows that are gone but which the app's AX window list still reports,
    /// modelling the lag real apps show right after a window closes.
    stale_window_ids: HashMap<WinID, Pid>,
    stale_window_incarnations: HashMap<WinID, crate::platform::WindowIncarnation>,
    /// Windows missing from both inventories whose cached AX handles still
    /// answer attribute queries, as some applications do after closing.
    stale_ax_handles: HashSet<WinID>,
    /// AX has withdrawn these windows while CoreGraphics still retains an
    /// on-screen surface for them briefly.
    withdrawn_surfaces: HashMap<WinID, MockWindowData>,
    native_space_control: bool,
    native_space_intents: Vec<crate::manager::NativeSpaceIntent>,
    associated_windows: HashMap<WinID, Vec<WinID>>,
    focus_requests: Vec<WinID>,
    raise_requests: Vec<WinID>,
    window_server_inventory_available: bool,
    window_order_in_session: Option<Vec<(WinID, Pid)>>,
    presentation_inventory_available: bool,
    window_server_inventory_omissions: HashSet<WinID>,
    workspace_membership_scripts:
        HashMap<WorkspaceId, VecDeque<std::result::Result<Vec<WinID>, ()>>>,
    active_space_query_scripts: HashMap<u32, VecDeque<std::result::Result<WorkspaceId, ()>>>,
    active_display_query_scripts: VecDeque<std::result::Result<u32, ()>>,
    present_display_topology_scripts: HashMap<u32, VecDeque<std::result::Result<(), ()>>>,
    window_observer_failures: HashMap<WinID, u32>,
    window_observer_attempts: HashMap<WinID, u32>,
    application_inventory_failures: HashMap<Pid, u32>,
    application_ax_errors: HashMap<Pid, i32>,
    application_observer_attempts: HashMap<Pid, u32>,
    application_inventory_attempts: HashMap<Pid, u32>,
    incomplete_application_inventories: HashSet<Pid>,
    omitted_application_inventory_windows: HashSet<(Pid, WinID)>,
    focused_window_query_failures: HashMap<Pid, u32>,
    full_screen_query_failures: HashMap<WinID, u32>,
    application_liveness_failures: HashMap<Pid, u32>,
    constrained_frame_writes: HashSet<WinID>,
    rejected_frame_writes: HashSet<WinID>,
    progressive_frame_writes: HashSet<WinID>,
    frame_write_attempts: HashMap<WinID, u32>,
    position_write_attempts: HashMap<WinID, u32>,
    resize_write_attempts: HashMap<WinID, u32>,
    frame_write_readback_failures: HashMap<WinID, u32>,
    frame_update_failures: HashMap<WinID, u32>,
    applied_horizontal_padding: HashMap<WinID, i32>,
    applied_vertical_padding: HashMap<WinID, i32>,
    notification_request_failures: u32,
    notification_request_attempts: u32,
    next_window_incarnation: crate::platform::WindowIncarnation,
}

fn application_inventory_omits(inner: &MockStateInner, pid: Pid, window_id: WinID) -> bool {
    inner
        .omitted_application_inventory_windows
        .contains(&(pid, window_id))
        || inner
            .windows
            .get(&window_id)
            .is_some_and(|window| !window.published)
}

#[derive(Clone)]
pub struct MockState {
    inner: Arc<RwLock<MockStateInner>>,
}

impl MockState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MockStateInner {
                apps: HashMap::new(),
                windows: HashMap::new(),
                cached_frames: HashMap::new(),
                displays: HashMap::new(),
                display_observation_count: 0,
                workspace_membership_query_count: 0,
                display_inventory_available: true,
                fullscreen_spaces: HashSet::new(),
                active_display_id: 0,
                cursor_position: Origin::ZERO,
                event_queue: VecDeque::new(),
                stale_window_ids: HashMap::new(),
                stale_window_incarnations: HashMap::new(),
                stale_ax_handles: HashSet::new(),
                withdrawn_surfaces: HashMap::new(),
                native_space_control: false,
                native_space_intents: Vec::new(),
                associated_windows: HashMap::new(),
                focus_requests: Vec::new(),
                raise_requests: Vec::new(),
                window_server_inventory_available: true,
                window_order_in_session: None,
                presentation_inventory_available: true,
                window_server_inventory_omissions: HashSet::new(),
                workspace_membership_scripts: HashMap::new(),
                active_space_query_scripts: HashMap::new(),
                active_display_query_scripts: VecDeque::new(),
                present_display_topology_scripts: HashMap::new(),
                window_observer_failures: HashMap::new(),
                window_observer_attempts: HashMap::new(),
                application_inventory_failures: HashMap::new(),
                application_ax_errors: HashMap::new(),
                application_observer_attempts: HashMap::new(),
                application_inventory_attempts: HashMap::new(),
                incomplete_application_inventories: HashSet::new(),
                omitted_application_inventory_windows: HashSet::new(),
                focused_window_query_failures: HashMap::new(),
                full_screen_query_failures: HashMap::new(),
                application_liveness_failures: HashMap::new(),
                constrained_frame_writes: HashSet::new(),
                rejected_frame_writes: HashSet::new(),
                progressive_frame_writes: HashSet::new(),
                frame_write_attempts: HashMap::new(),
                position_write_attempts: HashMap::new(),
                resize_write_attempts: HashMap::new(),
                frame_write_readback_failures: HashMap::new(),
                frame_update_failures: HashMap::new(),
                applied_horizontal_padding: HashMap::new(),
                applied_vertical_padding: HashMap::new(),
                notification_request_failures: 0,
                notification_request_attempts: 0,
                next_window_incarnation: 0,
            })),
        }
    }

    pub(crate) fn window_visible(&self, window_id: WinID, visible: bool) {
        let mut state = self.inner.force_write();
        let window = state.windows.get_mut(&window_id).expect("finding window");
        window.visible = visible;
    }

    pub(crate) fn enable_native_space_control(&self) {
        self.inner.force_write().native_space_control = true;
    }

    pub(crate) fn disable_native_space_control(&self) {
        self.inner.force_write().native_space_control = false;
    }

    pub(crate) fn native_space_intents(&self) -> Vec<crate::manager::NativeSpaceIntent> {
        self.inner.force_read().native_space_intents.clone()
    }

    pub(crate) fn set_associated_windows(&self, window: WinID, associated: Vec<WinID>) {
        self.inner
            .force_write()
            .associated_windows
            .insert(window, associated);
    }

    pub(crate) fn window_workspace(&self, window_id: WinID) -> Option<WorkspaceId> {
        self.inner
            .force_read()
            .windows
            .get(&window_id)
            .map(|window| window.workspace_id)
    }

    // --- OS Behavior Methods ---

    pub fn spawn_app(&self, pid: Pid, bundle_id: &str, name: &str) {
        let mut inner = self.inner.force_write();
        inner.apps.insert(
            pid,
            MockAppData {
                psn: ProcessSerialNumber {
                    high: 0,
                    low: pid.cast_unsigned(),
                },
                bundle_id: bundle_id.to_string(),
                name: name.to_string(),
                focused_window_id: None,
                is_frontmost: true,
                connection: Some(0),
                running: true,
                ready: true,
            },
        );
    }

    pub fn set_app_running(&self, pid: Pid, running: bool) {
        if let Some(app) = self.inner.force_write().apps.get_mut(&pid) {
            app.running = running;
        }
    }

    pub fn fail_application_liveness(&self, pid: Pid, attempts: u32) {
        self.inner
            .force_write()
            .application_liveness_failures
            .insert(pid, attempts);
    }

    pub fn fail_focused_window_queries(&self, pid: Pid, attempts: u32) {
        self.inner
            .force_write()
            .focused_window_query_failures
            .insert(pid, attempts);
    }

    pub fn fail_full_screen_queries(&self, id: WinID, attempts: u32) {
        self.inner
            .force_write()
            .full_screen_query_failures
            .insert(id, attempts);
    }

    pub fn spawn_window(
        &self,
        pid: Pid,
        workspace_id: WorkspaceId,
        id: WinID,
        frame: IRect,
    ) -> Window {
        let mut inner = self.inner.force_write();
        inner.next_window_incarnation = inner.next_window_incarnation.saturating_add(1);
        let incarnation = inner.next_window_incarnation;
        inner.windows.insert(
            id,
            MockWindowData {
                id,
                incarnation,
                pid,
                frame,
                title: format!("Window {id}"),
                workspace_id,
                ..default()
            },
        );
        inner.cached_frames.insert(id, frame);
        drop(inner);
        self.create_window(id)
    }

    pub fn focus_window(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        if let Some(win) = inner.windows.get(&id) {
            let pid = win.pid;
            if let Some(app) = inner.apps.get_mut(&pid) {
                app.focused_window_id = Some(id);
                let psn = app.psn;
                inner
                    .event_queue
                    .push_back(Event::ApplicationFrontSwitched { psn });
                inner.event_queue.push_back(Event::window_focused(id));
            }
        }
    }

    pub(crate) fn take_focus_requests(&self) -> Vec<WinID> {
        std::mem::take(&mut self.inner.force_write().focus_requests)
    }

    pub(crate) fn take_raise_requests(&self) -> Vec<WinID> {
        std::mem::take(&mut self.inner.force_write().raise_requests)
    }

    fn request_focus(&self, id: WinID) {
        self.inner.force_write().focus_requests.push(id);
        self.focus_window(id);
    }

    pub fn add_display(&mut self, id: u32, bounds: IRect, workspaces: Vec<WorkspaceId>) {
        let mut inner = self.inner.force_write();
        if inner.displays.is_empty() {
            inner.active_display_id = id;
        }
        inner
            .displays
            .insert(id, MockDisplayData { bounds, workspaces });
    }

    #[allow(unused)]
    pub fn remove_display(&self, id: u32) {
        let mut inner = self.inner.force_write();
        inner.displays.remove(&id);
        if inner.active_display_id == id {
            inner.active_display_id = inner.displays.keys().copied().next().unwrap_or(0);
        }
    }

    pub fn active_display(&self) -> CGDirectDisplayID {
        self.inner.force_read().active_display_id
    }

    /// Moves the menu bar display the way macOS does after a click hands the
    /// key window to another display. Deliberately does not queue an
    /// `Event::DisplayChanged`: `AppKit` posts no active-display notification.
    pub fn set_active_display(&self, id: u32) {
        self.inner.force_write().active_display_id = id;
    }

    pub(crate) fn activate_workspace(
        &self,
        display_id: u32,
        workspace_id: WorkspaceId,
        fullscreen: bool,
    ) {
        let mut inner = self.inner.force_write();
        {
            let display = inner
                .displays
                .get_mut(&display_id)
                .expect("finding display");
            display.workspaces.retain(|id| *id != workspace_id);
            display.workspaces.insert(0, workspace_id);
        }
        if fullscreen {
            inner.fullscreen_spaces.insert(workspace_id);
        } else {
            inner.fullscreen_spaces.remove(&workspace_id);
        }
    }

    pub(crate) fn destroy_workspace(&self, display_id: u32, workspace_id: WorkspaceId) {
        let mut inner = self.inner.force_write();
        let display = inner
            .displays
            .get_mut(&display_id)
            .expect("finding display");
        display.workspaces.retain(|id| *id != workspace_id);
        inner.fullscreen_spaces.remove(&workspace_id);
    }

    pub(crate) fn script_workspace_membership_queries(
        &self,
        workspace_id: WorkspaceId,
        responses: impl IntoIterator<Item = std::result::Result<Vec<WinID>, ()>>,
    ) {
        self.inner
            .force_write()
            .workspace_membership_scripts
            .insert(workspace_id, responses.into_iter().collect());
    }

    pub(crate) fn script_active_space_queries(
        &self,
        display_id: u32,
        responses: impl IntoIterator<Item = std::result::Result<WorkspaceId, ()>>,
    ) {
        self.inner
            .force_write()
            .active_space_query_scripts
            .insert(display_id, responses.into_iter().collect());
    }

    pub(crate) fn script_active_display_queries(
        &self,
        responses: impl IntoIterator<Item = std::result::Result<u32, ()>>,
    ) {
        self.inner.force_write().active_display_query_scripts = responses.into_iter().collect();
    }

    pub(crate) fn script_present_display_topology_queries(
        &self,
        display_id: u32,
        responses: impl IntoIterator<Item = std::result::Result<(), ()>>,
    ) {
        self.inner
            .force_write()
            .present_display_topology_scripts
            .insert(display_id, responses.into_iter().collect());
    }

    fn observe_displays(&self) -> crate::errors::Result<Vec<DisplayObservation>> {
        let mut inner = self.inner.force_write();
        inner.display_observation_count += 1;
        if !inner.display_inventory_available {
            return Err(Error::Generic("display inventory unavailable".into()));
        }
        let ids = inner.displays.keys().copied().collect::<Vec<_>>();
        Ok(ids
            .into_iter()
            .map(|id| {
                let available = inner
                    .present_display_topology_scripts
                    .get_mut(&id)
                    .and_then(VecDeque::pop_front)
                    .unwrap_or(Ok(()));
                let display = &inner.displays[&id];
                DisplayObservation {
                    display: Display::new(id, display.bounds, TEST_MENUBAR_HEIGHT),
                    spaces: available
                        .map(|()| display.workspaces.clone())
                        .map_err(|()| Error::Generic(format!("display {id} topology unavailable"))),
                }
            })
            .collect())
    }

    pub(crate) fn set_display_inventory_available(&self, available: bool) {
        self.inner.force_write().display_inventory_available = available;
    }

    pub(crate) fn display_observation_count(&self) -> usize {
        self.inner.force_read().display_observation_count
    }

    /// Native per-Space membership reads, which one membership scan costs one
    /// of per Space.
    pub(crate) fn workspace_membership_query_count(&self) -> usize {
        self.inner.force_read().workspace_membership_query_count
    }

    fn query_workspace_windows(
        &self,
        workspace_id: WorkspaceId,
    ) -> crate::errors::Result<Vec<WinID>> {
        let mut inner = self.inner.force_write();
        inner.workspace_membership_query_count += 1;
        if let Some(response) = inner
            .workspace_membership_scripts
            .get_mut(&workspace_id)
            .and_then(VecDeque::pop_front)
        {
            return response.map_err(|()| {
                Error::Generic(format!(
                    "workspace {workspace_id} scripted membership unavailable"
                ))
            });
        }
        let mut windows = inner
            .windows
            .values()
            .chain(inner.withdrawn_surfaces.values())
            .filter_map(|window| (window.workspace_id == workspace_id).then_some(window.id))
            .collect::<Vec<_>>();
        windows.sort_unstable();
        Ok(windows)
    }

    pub fn drain_events(&self) -> Vec<Event> {
        let mut inner = self.inner.force_write();
        inner.event_queue.drain(..).collect()
    }

    // --- State Mutation Methods ---

    pub fn update_window<F>(&self, id: WinID, f: F)
    where
        F: FnOnce(&mut MockWindowData),
    {
        let mut inner = self.inner.force_write();
        if let Some(w) = inner.windows.get_mut(&id) {
            f(w);
        }
    }

    /// Changes the `WindowServer` frame without sending an AX notification.
    pub fn os_set_window_frame_silently(&self, id: WinID, frame: IRect) {
        self.update_window(id, |window| window.frame = frame);
    }

    pub fn actual_window_frame(&self, id: WinID) -> Option<IRect> {
        self.inner
            .force_read()
            .windows
            .get(&id)
            .map(|window| window.frame)
    }

    pub fn cached_window_frame(&self, id: WinID) -> Option<IRect> {
        self.inner.force_read().cached_frames.get(&id).copied()
    }

    pub fn set_window_server_inventory_available(&self, available: bool) {
        self.inner.force_write().window_server_inventory_available = available;
    }

    pub(crate) fn set_window_order_in_session(&self, order: Vec<(WinID, Pid)>) {
        self.inner.force_write().window_order_in_session = Some(order);
    }

    pub fn set_presentation_inventory_available(&self, available: bool) {
        self.inner.force_write().presentation_inventory_available = available;
    }

    pub fn omit_window_from_window_server_inventory(&self, id: WinID, omit: bool) {
        let mut inner = self.inner.force_write();
        if omit {
            inner.window_server_inventory_omissions.insert(id);
        } else {
            inner.window_server_inventory_omissions.remove(&id);
        }
    }

    pub fn fail_window_observer_attempts(&self, id: WinID, attempts: u32) {
        self.inner
            .force_write()
            .window_observer_failures
            .insert(id, attempts);
    }

    pub fn fail_application_inventory_attempts(&self, pid: Pid, attempts: u32) {
        self.inner
            .force_write()
            .application_inventory_failures
            .insert(pid, attempts);
    }

    pub fn set_application_ax_error(&self, pid: Pid, error: Option<i32>) {
        let mut inner = self.inner.force_write();
        if let Some(code) = error {
            inner.application_ax_errors.insert(pid, code);
        } else {
            inner.application_ax_errors.remove(&pid);
        }
    }

    pub fn application_ax_attempts(&self, pid: Pid) -> (u32, u32) {
        let inner = self.inner.force_read();
        (
            inner
                .application_observer_attempts
                .get(&pid)
                .copied()
                .unwrap_or_default(),
            inner
                .application_inventory_attempts
                .get(&pid)
                .copied()
                .unwrap_or_default(),
        )
    }

    pub fn set_application_inventory_complete(&self, pid: Pid, complete: bool) {
        let mut inner = self.inner.force_write();
        if complete {
            inner.incomplete_application_inventories.remove(&pid);
        } else {
            inner.incomplete_application_inventories.insert(pid);
        }
    }

    pub fn omit_window_from_application_inventory(
        &self,
        pid: Pid,
        window_id: WinID,
        omitted: bool,
    ) {
        let mut inner = self.inner.force_write();
        if omitted {
            inner
                .omitted_application_inventory_windows
                .insert((pid, window_id));
        } else {
            inner
                .omitted_application_inventory_windows
                .remove(&(pid, window_id));
        }
    }

    pub fn window_observer_attempts(&self, id: WinID) -> u32 {
        self.inner
            .force_read()
            .window_observer_attempts
            .get(&id)
            .copied()
            .unwrap_or_default()
    }

    pub fn constrain_frame_writes(&self, id: WinID, constrained: bool) {
        let mut inner = self.inner.force_write();
        if constrained {
            inner.constrained_frame_writes.insert(id);
        } else {
            inner.constrained_frame_writes.remove(&id);
        }
    }

    pub fn reject_frame_writes(&self, id: WinID, rejected: bool) {
        let mut inner = self.inner.force_write();
        if rejected {
            inner.rejected_frame_writes.insert(id);
        } else {
            inner.rejected_frame_writes.remove(&id);
        }
    }

    pub fn progress_frame_writes(&self, id: WinID, progressive: bool) {
        let mut inner = self.inner.force_write();
        if progressive {
            inner.progressive_frame_writes.insert(id);
        } else {
            inner.progressive_frame_writes.remove(&id);
        }
    }

    pub fn fail_frame_write_readbacks(&self, id: WinID, attempts: u32) {
        self.inner
            .force_write()
            .frame_write_readback_failures
            .insert(id, attempts);
    }

    pub fn fail_frame_updates(&self, id: WinID, attempts: u32) {
        self.inner
            .force_write()
            .frame_update_failures
            .insert(id, attempts);
    }

    pub fn frame_write_attempts(&self, id: WinID) -> u32 {
        self.inner
            .force_read()
            .frame_write_attempts
            .get(&id)
            .copied()
            .unwrap_or_default()
    }

    pub fn position_write_attempts(&self, id: WinID) -> u32 {
        self.inner
            .force_read()
            .position_write_attempts
            .get(&id)
            .copied()
            .unwrap_or(0)
    }

    pub fn resize_write_attempts(&self, id: WinID) -> u32 {
        self.inner
            .force_read()
            .resize_write_attempts
            .get(&id)
            .copied()
            .unwrap_or_default()
    }

    pub fn applied_horizontal_padding(&self, id: WinID) -> Option<i32> {
        self.inner
            .force_read()
            .applied_horizontal_padding
            .get(&id)
            .copied()
    }

    pub fn fail_notification_requests(&self, attempts: u32) {
        self.inner.force_write().notification_request_failures = attempts;
    }

    pub fn notification_request_attempts(&self) -> u32 {
        self.inner.force_read().notification_request_attempts
    }

    #[allow(unused)]
    pub fn update_app(&self, pid: Pid, f: impl FnOnce(&mut MockAppData)) {
        let mut inner = self.inner.force_write();
        if let Some(a) = inner.apps.get_mut(&pid) {
            f(a);
        }
    }

    // --- OS Behavior Methods ---

    #[allow(unused)]
    pub fn os_move_window(&self, id: WinID, origin: Origin) {
        let mut inner = self.inner.force_write();
        if let Some(w) = inner.windows.get_mut(&id) {
            let size = w.frame.size();
            w.frame.min = origin;
            w.frame.max = origin + size;
            let incarnation = w.incarnation;
            inner.event_queue.push_back(Event::WindowMoved {
                window_id: id,
                incarnation,
            });
        }
    }

    #[allow(unused)]
    pub fn os_resize_window(&self, id: WinID, size: Size) {
        let mut inner = self.inner.force_write();
        if let Some(w) = inner.windows.get_mut(&id) {
            w.frame.max = w.frame.min + size;
            let incarnation = w.incarnation;
            inner.event_queue.push_back(Event::WindowResized {
                window_id: id,
                incarnation,
            });
        }
    }

    #[allow(unused)]
    pub fn os_minimize_window(&self, id: WinID, minimized: bool) {
        let mut inner = self.inner.force_write();
        if let Some(w) = inner.windows.get_mut(&id) {
            w.minimized = minimized;
            let incarnation = w.incarnation;
            if minimized {
                inner.event_queue.push_back(Event::WindowMinimized {
                    window_id: id,
                    incarnation: Some(incarnation),
                });
            } else {
                inner.event_queue.push_back(Event::WindowDeminimized {
                    window_id: id,
                    incarnation: Some(incarnation),
                });
            }
        }
    }

    /// Closes a window while its application keeps running, the way macOS
    /// actually reports it: the SLS space notification first, then the AX
    /// element teardown, with the AX element and the app's window list still
    /// reporting the window for a while after, as they do on real apps.
    #[allow(unused)]
    pub fn os_close_window(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        let Some(window) = inner.windows.remove(&id) else {
            return;
        };
        inner.stale_window_ids.insert(id, window.pid);
        inner
            .stale_window_incarnations
            .insert(id, window.incarnation);
        inner.event_queue.push_back(Event::WindowDestroyed {
            window_id: id,
            source: DestroySource::SpaceNotification,
            incarnation: None,
        });
        inner.event_queue.push_back(Event::WindowDestroyed {
            window_id: id,
            source: DestroySource::Accessibility,
            incarnation: Some(window.incarnation),
        });
    }

    /// Leaves the retired AX identity in the application's inventory without
    /// delivering either close notification. This models ID reuse during the
    /// short interval before the application's AX list catches up.
    pub fn os_stale_window_without_notifications(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        let Some(window) = inner.windows.remove(&id) else {
            return;
        };
        inner.stale_window_ids.insert(id, window.pid);
        inner
            .stale_window_incarnations
            .insert(id, window.incarnation);
    }

    /// Makes a window disappear with no notification at all, modelling a
    /// destroy event that never arrived — a lost notification, or a window
    /// closed while spool was not running.
    #[allow(unused)]
    pub fn os_vanish_window(&self, id: WinID) {
        self.inner.force_write().windows.remove(&id);
    }

    /// Makes a window disappear without notifications while its cached AX
    /// element keeps responding to attribute queries.
    #[allow(unused)]
    pub fn os_vanish_window_with_stale_ax_handle(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        if inner.windows.remove(&id).is_some() {
            inner.stale_ax_handles.insert(id);
        }
    }

    /// Withdraws the AX window while leaving its CoreGraphics surface visible.
    #[allow(unused)]
    pub fn os_withdraw_window(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        if let Some(window) = inner.windows.remove(&id) {
            inner.withdrawn_surfaces.insert(id, window);
        }
    }

    pub fn os_order_out_withdrawn_surface(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        let window = inner
            .withdrawn_surfaces
            .get_mut(&id)
            .expect("withdrawn surface");
        window.visible = false;
        window.ordered_out = true;
    }

    /// Lets CoreGraphics catch up after an AX-withdrawn window is closed.
    #[allow(unused)]
    pub fn os_settle_withdrawn_surface(&self, id: WinID) {
        self.inner.force_write().withdrawn_surfaces.remove(&id);
    }

    /// Makes an AX-withdrawn window available again.
    #[allow(unused)]
    pub fn os_restore_withdrawn_window(&self, id: WinID) {
        let mut inner = self.inner.force_write();
        if let Some(window) = inner.withdrawn_surfaces.remove(&id) {
            inner.windows.insert(id, window);
        }
    }

    /// Lets the app's window list catch up with reality after a close.
    #[allow(unused)]
    pub fn os_settle_window_list(&self) {
        let mut inner = self.inner.force_write();
        inner.stale_window_ids.clear();
        inner.stale_window_incarnations.clear();
    }

    // --- Interaction Helpers ---

    #[allow(unused)]
    pub fn simulate_click(&self, point: Origin) {
        let mut inner = self.inner.force_write();
        let point = CGPoint::new(point.x.into(), point.y.into());
        inner.event_queue.push_back(Event::MouseDown {
            point,
            modifiers: Modifiers::empty(),
        });
        inner.event_queue.push_back(Event::MouseUp {
            point,
            modifiers: Modifiers::empty(),
        });
    }

    #[allow(unused)]
    pub fn simulate_window_click(&self, id: WinID) {
        let inner = self.inner.force_read();
        if let Some(w) = inner.windows.get(&id) {
            let center = w.frame.center();
            drop(inner);
            self.simulate_click(center);
        }
    }

    #[allow(unused)]
    pub fn simulate_drag(&self, start: Origin, end: Origin) {
        let mut inner = self.inner.force_write();
        let start_p = CGPoint::new(start.x.into(), start.y.into());
        let end_p = CGPoint::new(end.x.into(), end.y.into());
        inner.event_queue.push_back(Event::MouseDown {
            point: start_p,
            modifiers: Modifiers::empty(),
        });
        inner.event_queue.push_back(Event::MouseDragged {
            point: end_p,
            modifiers: Modifiers::empty(),
        });
        inner.event_queue.push_back(Event::MouseUp {
            point: end_p,
            modifiers: Modifiers::empty(),
        });
    }

    pub fn cursor_position(&self) -> IVec2 {
        self.inner.force_read().cursor_position
    }

    // --- Mock Factory Methods ---

    #[allow(clippy::too_many_lines)]
    pub fn create_window(&self, id: WinID) -> Window {
        let inner = self.inner.force_read();
        let incarnation = inner
            .windows
            .get(&id)
            .or_else(|| inner.withdrawn_surfaces.get(&id))
            .map(|window| window.incarnation)
            .unwrap_or_default();
        drop(inner);
        self.create_window_with_incarnation(id, incarnation)
    }

    pub fn create_stale_window(&self, id: WinID) -> Window {
        let incarnation = self
            .inner
            .force_read()
            .stale_window_incarnations
            .get(&id)
            .copied()
            .expect("stale window incarnation");
        self.create_window_with_incarnation(id, incarnation)
    }

    #[allow(clippy::too_many_lines)]
    fn create_window_with_incarnation(
        &self,
        id: WinID,
        incarnation: crate::platform::WindowIncarnation,
    ) -> Window {
        let mut mw = MockWindowApi::new();

        mw.expect_id().return_const(id);
        let s = self.clone();
        mw.expect_default_floating().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .is_some_and(|window| window.default_floating)
        });
        mw.expect_incarnation().return_const(incarnation);
        let s = self.clone();
        mw.expect_represented_window_id().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|window| window.represented_window_id.unwrap_or(id))
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        mw.expect_is_resizable().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .filter(|window| window.capabilities_available)
                .map(|window| window.resizable)
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        mw.expect_is_movable().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .filter(|window| window.capabilities_available)
                .map(|window| window.movable)
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        mw.expect_pid().returning(move || {
            let inner = s.inner.force_read();
            inner
                .windows
                .get(&id)
                .map(|w| w.pid)
                .or_else(|| inner.stale_window_ids.get(&id).copied())
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        mw.expect_frame().returning(move || {
            s.inner
                .force_read()
                .cached_frames
                .get(&id)
                .copied()
                .unwrap_or_default()
        });

        let s_move = self.clone();
        mw.expect_reposition().returning(move |origin| {
            let mut inner = s_move.inner.force_write();
            *inner.position_write_attempts.entry(id).or_default() += 1;
            let frame = if let Some(w) = inner.windows.get_mut(&id) {
                let size = w.frame.size();
                w.frame.min = origin;
                w.frame.max = origin + size;
                w.frame
            } else {
                return Err(Error::InvalidWindow);
            };
            inner.cached_frames.insert(id, frame);
            Ok(frame)
        });

        let s_resize = self.clone();
        mw.expect_resize_preserving_origin()
            .returning(move |frame| {
                let mut inner = s_resize.inner.force_write();
                *inner.resize_write_attempts.entry(id).or_default() += 1;
                if inner.constrained_frame_writes.contains(&id) {
                    return inner
                        .windows
                        .get(&id)
                        .map(|window| window.frame)
                        .ok_or(Error::InvalidWindow);
                }
                let Some(window) = inner.windows.get_mut(&id) else {
                    return Err(Error::InvalidWindow);
                };
                window.frame = frame;
                if let Some(remaining) = inner.frame_write_readback_failures.get_mut(&id)
                    && *remaining > 0
                {
                    *remaining -= 1;
                    return Err(Error::Generic("mock AX frame readback failure".to_string()));
                }
                inner.cached_frames.insert(id, frame);
                Ok(frame)
            });

        let s = self.clone();
        mw.expect_set_frame().returning(move |frame| {
            let mut inner = s.inner.force_write();
            *inner.frame_write_attempts.entry(id).or_default() += 1;
            if inner.rejected_frame_writes.contains(&id) {
                return Err(Error::Generic("mock AX frame write rejected".to_string()));
            }
            if inner.progressive_frame_writes.contains(&id) {
                let Some(window) = inner.windows.get_mut(&id) else {
                    return Err(Error::InvalidWindow);
                };
                window.frame =
                    IRect::from_corners(window.frame.min + IVec2::X, window.frame.max + IVec2::X);
                let observed = window.frame;
                inner.cached_frames.insert(id, observed);
                return Ok(observed);
            }
            if inner.constrained_frame_writes.contains(&id) {
                return inner
                    .windows
                    .get(&id)
                    .map(|window| window.frame)
                    .ok_or(Error::InvalidWindow);
            }
            let Some(window) = inner.windows.get_mut(&id) else {
                return Err(Error::InvalidWindow);
            };
            window.frame = frame;
            if let Some(remaining) = inner.frame_write_readback_failures.get_mut(&id)
                && *remaining > 0
            {
                *remaining -= 1;
                return Err(Error::Generic("mock AX frame readback failure".to_string()));
            }
            inner.cached_frames.insert(id, frame);
            Ok(frame)
        });

        let s = self.clone();
        mw.expect_focus_with_raise().returning(move |_psn| {
            s.request_focus(id);
        });

        let s = self.clone();
        mw.expect_title().returning(move || {
            Ok(s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|w| w.title.clone())
                .unwrap_or_default())
        });
        // The mock reads its title from shared state every time, so there's
        // nothing to invalidate — but the call still needs an expectation.
        mw.expect_invalidate_title().return_const(());
        let s = self.clone();
        mw.expect_retained_title().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|window| window.title.clone())
        });

        let s = self.clone();
        mw.expect_is_minimized().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .is_some_and(|w| w.minimized)
        });

        let s = self.clone();
        mw.expect_update_frame().returning(move || {
            let mut inner = s.inner.force_write();
            if let Some(remaining) = inner.frame_update_failures.get_mut(&id)
                && *remaining > 0
            {
                *remaining -= 1;
                return Err(Error::Generic("mock AX frame update failure".to_string()));
            }
            if let Some(frame) = inner.windows.get(&id).map(|window| window.frame) {
                inner.cached_frames.insert(id, frame);
                Ok(frame)
            } else if inner.stale_window_ids.contains_key(&id)
                || inner.stale_ax_handles.contains(&id)
            {
                inner
                    .cached_frames
                    .get(&id)
                    .copied()
                    .ok_or(Error::InvalidWindow)
            } else {
                Err(Error::InvalidWindow)
            }
        });

        let s = self.clone();
        mw.expect_role().returning(move || {
            let inner = s.inner.force_read();
            if let Some(window) = inner.windows.get(&id) {
                Ok(window.role.clone())
            } else if inner.stale_window_ids.contains_key(&id)
                || inner.stale_ax_handles.contains(&id)
            {
                Ok("AXWindow".to_string())
            } else {
                Err(Error::InvalidWindow)
            }
        });

        let s = self.clone();
        mw.expect_subrole().returning(move || {
            Ok(s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|w| w.subrole.clone())
                .unwrap_or_default())
        });

        let s = self.clone();
        mw.expect_child_role().returning(move || {
            Ok(s.inner
                .force_read()
                .windows
                .get(&id)
                .is_some_and(|w| w.child_role))
        });

        let s = self.clone();
        mw.expect_horizontal_padding().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|w| w.horizontal_padding)
                .unwrap_or_default()
        });

        let s = self.clone();
        mw.expect_vertical_padding().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .map(|w| w.vertical_padding)
                .unwrap_or_default()
        });

        let s = self.clone();
        mw.expect_is_full_screen().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .is_some_and(|w| w.is_full_screen)
        });

        let s = self.clone();
        mw.expect_try_is_full_screen().returning(move || {
            let mut inner = s.inner.force_write();
            if let Some(remaining) = inner.full_screen_query_failures.get_mut(&id)
                && *remaining > 0
            {
                *remaining -= 1;
                return Err(Error::InvalidWindow);
            }
            inner
                .windows
                .get(&id)
                .map(|window| window.is_full_screen)
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        mw.expect_border_radius().returning(move || {
            s.inner
                .force_read()
                .windows
                .get(&id)
                .and_then(|w| w.border_radius)
        });

        // Fill in remaining defaults
        mw.expect_element().return_const(None);
        let s = self.clone();
        mw.expect_raise_without_focus().returning(move || {
            s.inner.force_write().raise_requests.push(id);
        });
        let s = self.clone();
        mw.expect_focus_without_raise()
            .returning(move |_psn, _focused_window, _focused_psn| {
                s.request_focus(id);
            });
        let s = self.clone();
        mw.expect_set_padding().returning(move |padding| {
            let mut inner = s.inner.force_write();
            match padding {
                WindowPadding::Horizontal(value) => {
                    inner.applied_horizontal_padding.insert(id, value);
                }
                WindowPadding::Vertical(value) => {
                    inner.applied_vertical_padding.insert(id, value);
                }
            }
        });

        Window::new(Box::new(mw))
    }

    fn mock_application_windows(&self, application: &mut MockApplicationApi, pid: Pid) {
        let s = self.clone();
        application
            .expect_observe_window()
            .returning(move |window| {
                let id = window.id();
                let mut inner = s.inner.force_write();
                *inner.window_observer_attempts.entry(id).or_default() += 1;
                let Some(remaining) = inner.window_observer_failures.get_mut(&id) else {
                    return Ok(true);
                };
                if *remaining == 0 {
                    return Ok(true);
                }
                *remaining -= 1;
                Ok(false)
            });
        application.expect_unobserve_window().return_const(());
        self.mock_application_inventory(application, pid);
    }

    #[allow(clippy::too_many_lines)]
    fn mock_application_inventory(&self, application: &mut MockApplicationApi, pid: Pid) {
        let s = self.clone();
        let window_ids = move || {
            let inner = s.inner.force_read();
            inner
                .windows
                .values()
                .filter(|w| w.pid == pid)
                .map(|w| w.id)
                .chain(
                    inner
                        .stale_window_ids
                        .iter()
                        .filter(|&(_, &owner)| owner == pid)
                        .map(|(&id, _)| id),
                )
                .collect::<Vec<_>>()
        };

        let (s, ids) = (self.clone(), window_ids.clone());
        application.expect_window_list().returning(move |_| {
            ids()
                .into_iter()
                .filter(|id| {
                    s.inner
                        .force_read()
                        .windows
                        .get(id)
                        .is_some_and(|window| window.role != "AXUnknown" && window.published)
                })
                .map(|id| s.create_window(id))
                .collect()
        });
        let s = self.clone();
        application.expect_window_inventory().returning(move |_| {
            let (identities, candidate_keys, complete) = {
                let mut inner = s.inner.force_write();
                *inner.application_inventory_attempts.entry(pid).or_default() += 1;
                if let Some(&code) = inner.application_ax_errors.get(&pid) {
                    return Err(Error::macos("mock AXWindows", code));
                }
                if let Some(remaining) = inner.application_inventory_failures.get_mut(&pid)
                    && *remaining > 0
                {
                    *remaining -= 1;
                    return Err(Error::InvalidWindow);
                }
                let identities = inner
                    .windows
                    .values()
                    .filter(|window| {
                        window.pid == pid && !application_inventory_omits(&inner, pid, window.id)
                    })
                    .map(|window| (window.id, window.incarnation))
                    .chain(
                        inner
                            .stale_window_ids
                            .iter()
                            .filter(|&(_, &owner)| owner == pid)
                            .map(|(&id, _)| {
                                (
                                    id,
                                    inner
                                        .stale_window_incarnations
                                        .get(&id)
                                        .copied()
                                        .unwrap_or_default(),
                                )
                            }),
                    )
                    .collect();
                let candidate_keys = inner
                    .windows
                    .values()
                    .filter(|window| {
                        window.pid == pid
                            && window.role != "AXUnknown"
                            && !application_inventory_omits(&inner, pid, window.id)
                    })
                    .map(|window| (window.id, window.incarnation))
                    .chain(
                        inner
                            .stale_window_ids
                            .iter()
                            .filter(|&(_, &owner)| owner == pid)
                            .filter_map(|(&id, _)| {
                                inner
                                    .stale_window_incarnations
                                    .get(&id)
                                    .copied()
                                    .map(|incarnation| (id, incarnation))
                            }),
                    )
                    .collect::<Vec<_>>();
                let complete = !inner.incomplete_application_inventories.contains(&pid)
                    && !inner
                        .omitted_application_inventory_windows
                        .iter()
                        .any(|(owner, _)| *owner == pid);
                (identities, candidate_keys, complete)
            };
            Ok(crate::manager::app::ApplicationWindowInventory {
                identities,
                candidates: candidate_keys
                    .into_iter()
                    .map(|(id, incarnation)| s.create_window_with_incarnation(id, incarnation))
                    .collect(),
                complete,
            })
        });
        let s = self.clone();
        application.expect_owns_window().returning(move |window| {
            let inner = s.inner.force_read();
            let identity = (window.id(), window.incarnation());
            Ok(inner.windows.values().any(|candidate| {
                candidate.pid == pid
                    && candidate.published
                    && (candidate.id, candidate.incarnation) == identity
            }) || inner.stale_window_ids.get(&window.id()) == Some(&pid)
                && inner.stale_window_incarnations.get(&window.id()) == Some(&window.incarnation()))
        });
    }

    pub fn create_application(&self, pid: Pid) -> Application {
        self.create_application_with_liveness(pid, None)
    }

    pub fn create_application_with_running(&self, pid: Pid, running: bool) -> Application {
        self.create_application_with_liveness(pid, Some(Ok(running)))
    }

    fn create_application_with_liveness(
        &self,
        pid: Pid,
        liveness: Option<crate::errors::Result<bool>>,
    ) -> Application {
        let mut ma = MockApplicationApi::new();
        let s = self.clone();

        ma.expect_pid().return_const(pid);
        ma.expect_psn()
            .returning(move || s.inner.force_read().apps.get(&pid).map(|a| a.psn).unwrap());

        let s = self.clone();
        ma.expect_focused_window_id().returning(move || {
            let mut inner = s.inner.force_write();
            if let Some(remaining) = inner.focused_window_query_failures.get_mut(&pid)
                && *remaining > 0
            {
                *remaining -= 1;
                return Err(Error::macos("mock focused-window query", -1));
            }
            inner
                .apps
                .get(&pid)
                .and_then(|a| a.focused_window_id)
                .ok_or(Error::InvalidWindow)
        });

        let s = self.clone();
        ma.expect_bundle_id().returning(move || {
            s.inner
                .force_read()
                .apps
                .get(&pid)
                .map(|a| a.bundle_id.clone())
        });

        let name = self
            .inner
            .force_read()
            .apps
            .get(&pid)
            .map(|a| a.name.clone())
            .unwrap();
        ma.expect_name().return_const(name);

        let s = self.clone();
        ma.expect_is_frontmost().returning(move || {
            s.inner
                .force_read()
                .apps
                .get(&pid)
                .is_some_and(|a| a.is_frontmost)
        });

        let s = self.clone();
        ma.expect_connection().returning(move || {
            s.inner
                .force_read()
                .apps
                .get(&pid)
                .and_then(|a| a.connection)
        });

        if let Some(liveness) = liveness {
            ma.expect_is_running().return_const(liveness);
        } else {
            let s = self.clone();
            ma.expect_is_running().returning(move || {
                let mut inner = s.inner.force_write();
                if let Some(remaining) = inner.application_liveness_failures.get_mut(&pid)
                    && *remaining > 0
                {
                    *remaining -= 1;
                    return Err(Error::macos("mock application liveness", -1));
                }
                Ok(inner.apps.get(&pid).is_some_and(|app| app.running))
            });
        }

        let s = self.clone();
        ma.expect_observe().returning(move || {
            let mut inner = s.inner.force_write();
            *inner.application_observer_attempts.entry(pid).or_default() += 1;
            if let Some(&code) = inner.application_ax_errors.get(&pid) {
                Err(Error::macos("AXObserverAddNotification(AXCreated)", code))
            } else {
                Ok(true)
            }
        });
        self.mock_application_windows(&mut ma, pid);

        Application::new(Box::new(ma))
    }

    fn mock_native_space_queries(&self, wm: &mut MockWindowManagerApi) {
        let s = self.clone();
        wm.expect_native_space_capabilities().returning(move || {
            let enabled = s.inner.force_read().native_space_control;
            crate::manager::NativeSpaceCapabilities {
                move_windows: enabled,
                focus: enabled,
                ..crate::manager::NativeSpaceCapabilities::default()
            }
        });
        let s = self.clone();
        wm.expect_perform_native_space_intent()
            .returning(move |intent| {
                let mut state = s.inner.force_write();
                if !state.native_space_control {
                    return Err(Error::Generic("Space capability unavailable".to_string()));
                }
                if let crate::manager::NativeSpaceIntent::MoveWindows {
                    window_ids,
                    space_id,
                } = intent
                {
                    for id in window_ids {
                        if let Some(window) = state.windows.get_mut(id) {
                            window.workspace_id = *space_id;
                        }
                    }
                } else if let crate::manager::NativeSpaceIntent::Focus { space_id, .. } = intent {
                    let active_display_id = state.active_display_id;
                    if let Some(display) = state.displays.get_mut(&active_display_id) {
                        display.workspaces.retain(|id| id != space_id);
                        display.workspaces.insert(0, *space_id);
                        state.event_queue.push_back(Event::SpaceChanged);
                    }
                }
                state.native_space_intents.push(intent.clone());
                Ok(())
            });
        let s = self.clone();
        wm.expect_workspace_is_fullscreen()
            .returning(move |workspace_id| {
                s.inner
                    .force_read()
                    .fullscreen_spaces
                    .contains(&workspace_id)
            });
    }

    fn mock_window_server_inventory(&self, wm: &mut MockWindowManagerApi) {
        let state = self.clone();
        wm.expect_window_order_in_session()
            .returning(move || state.inner.force_read().window_order_in_session.clone());
        let s = self.clone();
        wm.expect_window_owners_in_session().returning(move || {
            let inner = s.inner.force_read();
            if !inner.window_server_inventory_available {
                return None;
            }
            Some(
                inner
                    .windows
                    .iter()
                    .chain(inner.withdrawn_surfaces.iter())
                    .filter(|(id, _)| !inner.window_server_inventory_omissions.contains(id))
                    .map(|(id, window)| (*id, window.pid))
                    .collect(),
            )
        });

        let s = self.clone();
        wm.expect_presentation_windows_in_workspace()
            .returning(move |workspace_id| {
                let inner = s.inner.force_read();
                if !inner.presentation_inventory_available {
                    return Err(Error::Generic(
                        "mock presentation inventory unavailable".into(),
                    ));
                }
                Ok(inner
                    .windows
                    .values()
                    .chain(inner.withdrawn_surfaces.values())
                    .filter(|window| {
                        window.workspace_id == workspace_id
                            && !window.ordered_out
                            && !window.minimized
                    })
                    .map(|window| window.id)
                    .collect())
            });

        let s = self.clone();
        wm.expect_request_window_notifications()
            .returning(move |_| {
                let mut inner = s.inner.force_write();
                inner.notification_request_attempts += 1;
                if inner.notification_request_failures == 0 {
                    return Ok(());
                }
                inner.notification_request_failures -= 1;
                Err(Error::Generic(
                    "mock WindowServer subscription failure".to_string(),
                ))
            });
    }

    pub fn create_window_manager(&self) -> MockWindowManagerApi {
        let mut wm = MockWindowManagerApi::new();

        let s = self.clone();
        wm.expect_new_application()
            .returning(move |process| Ok(s.create_application(process.pid())));

        let s = self.clone();
        wm.expect_active_display_id().returning(move || {
            let mut inner = s.inner.force_write();
            if let Some(response) = inner.active_display_query_scripts.pop_front() {
                return response
                    .map_err(|()| Error::Generic("scripted active display unavailable".into()));
            }
            Ok(inner.active_display_id)
        });

        let s = self.clone();
        wm.expect_active_display_space().returning(move |id| {
            let mut inner = s.inner.force_write();
            if let Some(response) = inner
                .active_space_query_scripts
                .get_mut(&id)
                .and_then(VecDeque::pop_front)
            {
                return response.map_err(|()| {
                    Error::Generic(format!("display {id} scripted active Space unavailable"))
                });
            }
            inner
                .displays
                .get(&id)
                .and_then(|display| display.workspaces.first().copied())
                .ok_or(Error::InvalidWindow)
        });

        self.mock_native_space_queries(&mut wm);

        let s = self.clone();
        wm.expect_observe_displays()
            .returning(move || s.observe_displays());
        let s = self.clone();
        wm.expect_present_displays().returning(move || {
            s.observe_displays()
                .unwrap_or_default()
                .into_iter()
                .filter_map(DisplayObservation::into_known_topology)
                .collect()
        });

        let s = self.clone();
        wm.expect_find_existing_application_windows()
            .returning(move |app, spaces, _config| {
                let pid = app.pid();
                let mut windows = s
                    .inner
                    .force_read()
                    .windows
                    .values()
                    .filter_map(|w| {
                        (w.pid == pid && spaces.contains(&w.workspace_id))
                            .then_some(s.create_window(w.id))
                    })
                    .collect::<Vec<_>>();
                windows.sort_unstable_by_key(|window| window.id());
                Ok((windows, vec![]))
            });

        let s = self.clone();
        wm.expect_windows_in_workspace()
            .returning(move |workspace_id| s.query_workspace_windows(workspace_id));

        self.mock_window_server_inventory(&mut wm);

        let s = self.clone();
        wm.expect_warp_mouse()
            .returning(move |origin| s.inner.force_write().cursor_position = origin);

        let s = self.clone();
        wm.expect_cursor_position()
            .returning(move || Some(origin_to(s.inner.force_read().cursor_position)));

        let s = self.clone();
        wm.expect_get_associated_windows().returning(move |id| {
            s.inner
                .force_read()
                .associated_windows
                .get(&id)
                .cloned()
                .unwrap_or_default()
        });

        let s = self.clone();
        wm.expect_find_window_at_point().returning(move |at_point| {
            let point = origin_from(*at_point);
            s.inner
                .force_read()
                .windows
                .iter()
                .find_map(|(id, window)| window.frame.contains(point).then_some(id))
                .ok_or(Error::NotFound(format!("no window found at point {point}")))
                .copied()
        });

        wm
    }

    pub fn create_process(&self, pid: Pid) -> MockProcessApi {
        let mut mp = MockProcessApi::new();
        let s = self.clone();

        let name = self
            .inner
            .force_read()
            .apps
            .get(&pid)
            .map(|a| a.name.clone())
            .unwrap();
        mp.expect_name().return_const(name);

        mp.expect_pid().return_const(pid);
        mp.expect_psn()
            .returning(move || s.inner.force_read().apps.get(&pid).map(|a| a.psn).unwrap());
        mp.expect_is_observable().returning(|| true);
        mp.expect_application().return_const(None);
        let s = self.clone();
        mp.expect_ready().returning(move || {
            s.inner
                .force_read()
                .apps
                .get(&pid)
                .is_some_and(|app| app.ready)
        });
        mp.expect_force_track().return_const(());

        mp
    }
}

use bevy::ecs::message::Message;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::CGDirectDisplayID;
use spool_shared_types::wire::{Response, ScriptStateRequest};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::commands::Action;
use crate::config::Config;
use crate::ecs::state::StateQueryKind;
use crate::errors::Result;
use crate::platform::{
    EventLoopWaker, Modifiers, Pid, ProcessSerialNumber, WinID, WindowIncarnation, WorkspaceId,
    WorkspaceObserver,
};
use crate::util::AXUIWrapper;

/// Where a [`Event::WindowDestroyed`] came from, which decides how far it can be
/// trusted. macOS reports a closing window through several unrelated channels;
/// only the AX element teardown and subscribed `WindowServer` close are
/// definitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestroySource {
    /// `kAXUIElementDestroyedNotification` on the window's own AX element. The
    /// element itself has been torn down, so the window is definitively gone.
    Accessibility,
    /// SLS `SpaceWindowDestroyed`. Despite the name this also fires when a
    /// window merely leaves a space, so it has to be confirmed before acting.
    SpaceNotification,
    /// SLS `WindowClosed`, delivered for explicitly subscribed windows on
    /// macOS 15 and newer. Unlike the Space notification, this identifies an
    /// actual `WindowServer` close.
    WindowServer,
    /// A complete AX plus `WindowServer` inventory audit confirmed that one
    /// specific tracked incarnation disappeared.
    Reconciliation,
}

/// Limits a window-inventory reconciliation to one application or all known
/// applications. Application-scoped requests are cheap enough to issue from
/// noisy AX notifications; the full scope is reserved for explicit recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileScope {
    Application(Pid),
    All,
}

/// Identifies the macOS signal that caused Spool to resolve the focused
/// window. UI-element notifications deliberately carry no window id: their AX
/// element may be a control or native tab, so the owning application must be
/// queried again for its actual focused window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusSource {
    AccessibilityWindow,
    AccessibilityUiElement,
    ApplicationFrontSwitch,
    StateSync,
    Retry,
    Internal,
}

/// A focused-window observation plus enough provenance to reject it when a
/// newer application transition has already superseded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusObservation {
    pub window_id: WinID,
    pub pid: Option<Pid>,
    pub incarnation: Option<WindowIncarnation>,
    pub source: FocusSource,
    pub generation: Option<u64>,
}

impl FocusObservation {
    pub const fn internal(window_id: WinID) -> Self {
        Self {
            window_id,
            pid: None,
            incarnation: None,
            source: FocusSource::Internal,
            generation: None,
        }
    }

    pub const fn resolved(
        window_id: WinID,
        pid: Pid,
        source: FocusSource,
        generation: u64,
    ) -> Self {
        Self {
            window_id,
            pid: Some(pid),
            incarnation: None,
            source,
            generation: Some(generation),
        }
    }
}

/// Where a client's answer goes.
///
/// Bounded to one: exactly one answer is ever sent, and the ECS side answers
/// with `try_send` so it never blocks the main thread.
pub type Reply = async_channel::Sender<Response>;

/// `Event` represents various system-level and application-specific occurrences that the window manager reacts to.
/// These events drive the core logic of the window manager, from window creation to display changes.
#[cfg_attr(not(feature = "lua"), allow(dead_code))]
/// The pointer and gesture subset of [`Event`], republished on its own stream
/// by [`demux_input_events`](crate::ecs::systems::demux_input_events) so
/// input-heavy systems don't have to scan the full event stream. Events still
/// appear on [`Event`] too, for the Lua bridge and IPC subscribers.
#[derive(Clone, Debug, Message)]
pub struct InputEvent(pub Event);

/// Several variants carry a payload used only by the Lua bridge
/// (`src/lua/convert.rs`). The dead-code lint is suppressed only when the
/// `lua` feature is off, so it still applies to the build that ships.
#[cfg_attr(not(feature = "lua"), allow(dead_code))]
#[derive(Clone, Debug, Message)]
pub enum Event {
    /// Signals the application to exit.
    Exit,
    /// Indicates that the initial set of processes has been loaded.
    ProcessesLoaded,

    /// Announces the initialy loaded configuration
    InitialConfig(Config),
    /// Signals that the configuration should be reloaded.
    ConfigRefresh(notify::Event),

    /// An application has been launched.
    ApplicationLaunched {
        psn: ProcessSerialNumber,
        observer: Retained<WorkspaceObserver>,
    },

    /// An application has terminated.
    ApplicationTerminated { psn: ProcessSerialNumber },
    /// The frontmost application has switched.
    ApplicationFrontSwitched { psn: ProcessSerialNumber },
    /// An application has become the active (frontmost) application. Carries
    /// the pid for subscribers; Spool's own focus handling uses
    /// [`Event::ApplicationFrontSwitched`] instead.
    ApplicationActivated { pid: i32 },
    /// An application has stopped being the active application.
    ApplicationDeactivated { pid: i32 },
    /// An application has become visible.
    ApplicationVisible { pid: i32 },
    /// An application has become hidden.
    ApplicationHidden { pid: i32 },

    /// A window has been created.
    WindowCreated { element: CFRetained<AXUIWrapper> },
    /// A window has been fully spawned and populated in the window manager.
    WindowSpawned {
        window_id: WinID,
        pid: Pid,
        app_name: String,
        bundle_id: String,
        title: String,
        frame: spool_shared_types::state::Frame,
        floating: bool,
    },
    /// A window has been destroyed. `source` records which notification
    /// reported it; see [`DestroySource`].
    WindowDestroyed {
        window_id: WinID,
        source: DestroySource,
        /// Present for AX element teardown, whose observer is bound to one
        /// concrete window incarnation. ID-only system notifications leave it
        /// absent and are confirmed through inventory reconciliation.
        incarnation: Option<WindowIncarnation>,
    },
    /// A window has gained focus.
    WindowFocused(FocusObservation),
    /// A macOS signal changed what may be focused, but did not itself provide
    /// a trustworthy window id. Query the application's focused window.
    FocusRevalidationRequested { pid: Pid, source: FocusSource },
    /// A window has been moved.
    WindowMoved {
        window_id: WinID,
        incarnation: WindowIncarnation,
    },
    /// A window has been resized.
    WindowResized {
        window_id: WinID,
        incarnation: WindowIncarnation,
    },
    /// A window has been minimized.
    WindowMinimized {
        window_id: WinID,
        incarnation: Option<WindowIncarnation>,
    },
    /// A window has been de-minimized (restored).
    WindowDeminimized {
        window_id: WinID,
        incarnation: Option<WindowIncarnation>,
    },
    /// A window's title has changed.
    WindowTitleChanged {
        window_id: WinID,
        incarnation: Option<WindowIncarnation>,
    },
    /// Requests that the current macOS window inventory be reconciled with ECS.
    ReconcileWindows { scope: ReconcileScope },

    /// A mouse down event has occurred.
    MouseDown {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse up event has occurred.
    MouseUp {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse drag event has occurred.
    MouseDragged {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse move event has occurred.
    MouseMoved {
        point: CGPoint,
        modifiers: Modifiers,
    },

    /// A swipe gesture has been detected.
    Swipe { delta: f64, fingers: usize },

    /// A mouse scroll has been detected.
    Scroll { delta: f64 },

    /// Fingers have been placed on the touchpad.
    TouchpadDown,
    /// All fingers are up from the touchpad.
    TouchpadUp,

    /// A new space (virtual desktop) has been created.
    SpaceCreated { space_id: WorkspaceId },
    /// A space has been destroyed.
    SpaceDestroyed { space_id: WorkspaceId },
    /// The active space has changed.
    SpaceChanged,

    /// A new display has been added.
    DisplayAdded { display_id: CGDirectDisplayID },
    /// A display has been removed.
    DisplayRemoved { display_id: CGDirectDisplayID },
    /// A display has been moved.
    DisplayMoved { display_id: CGDirectDisplayID },
    /// A display has been resized.
    DisplayResized { display_id: CGDirectDisplayID },
    /// A display's configuration has changed.
    DisplayConfigured { display_id: CGDirectDisplayID },
    /// The overall display arrangement has changed.
    DisplayChanged,

    /// Mission Control: Show all windows.
    MissionControlShowAllWindows,
    /// Mission Control: Show frontmost application windows.
    MissionControlShowFrontWindows,
    /// Mission Control: Show desktop.
    MissionControlShowDesktop,
    /// Mission Control: Exit.
    MissionControlExit,

    /// Dock preferences have changed.
    DockDidChangePref { msg: String },
    /// The Dock has restarted.
    DockDidRestart { msg: String },

    /// A menu has been opened.
    MenuOpened { window_id: WinID },
    /// A menu has been closed.
    MenuClosed { window_id: WinID },
    /// The visibility of the menu bar has changed.
    MenuBarHiddenChanged { msg: String },
    /// The system has woken from sleep.
    SystemWoke { msg: String },

    /// The system appearance (Light/Dark mode) has changed.
    ThemeChanged,

    /// An action has been requested from the window manager.
    ActionRequested { action: Action },

    /// A structured state query has been issued by a client.
    StateQuery {
        kind: StateQueryKind,
        respond_to: Reply,
    },

    /// A client has asked for the window set: the same layout value a
    /// `spool.windows` handler is given inside the daemon.
    WindowSetQuery { respond_to: Reply },

    /// A client has subscribed to state events. Carries the channel they are
    /// pushed to, which outlives the request that delivered it.
    StateSubscribe {
        subscriber: Arc<spool_local_ipc::Subscriber>,
        raw: bool,
    },

    /// A client has read or written the script state store. Answered
    /// from the same store the embedded Lua runtime uses, so the two see each
    /// other's writes.
    ScriptState {
        request: ScriptStateRequest,
        respond_to: Reply,
    },
}

impl Event {
    /// Creates the single ECS entry event for every requested action.
    pub const fn action_requested(action: Action) -> Self {
        Self::ActionRequested { action }
    }

    pub const fn window_focused(window_id: WinID) -> Self {
        Self::WindowFocused(FocusObservation::internal(window_id))
    }

    pub const fn resolved_focus(
        window_id: WinID,
        pid: Pid,
        source: FocusSource,
        generation: u64,
    ) -> Self {
        Self::WindowFocused(FocusObservation::resolved(
            window_id, pid, source, generation,
        ))
    }
}

/// `EventSender` is a thin wrapper around a `std::sync::mpsc::Sender` for `Event`s.
/// It provides a convenient way to send events to the main event loop from various parts of the application.
#[derive(Clone, Debug)]
pub struct EventSender {
    tx: Sender<Event>,
    /// Ends the Cocoa pump's wait once the event is queued. Shared so a
    /// wake-up from any producer covers every event queued before it.
    waker: Arc<EventLoopWaker>,
}

impl Event {
    /// Whether this is one of the pointer or gesture events republished on
    /// [`InputEvent`].
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Event::MouseDown { .. }
                | Event::MouseUp { .. }
                | Event::MouseDragged { .. }
                | Event::MouseMoved { .. }
                | Event::Swipe { .. }
                | Event::Scroll { .. }
                | Event::TouchpadDown
                | Event::TouchpadUp
        )
    }
}

impl EventSender {
    /// Creates a new `EventSender` and its corresponding `Receiver`.
    /// This function initializes an MPSC channel.
    ///
    /// # Returns
    ///
    /// A tuple containing the `EventSender` and `Receiver` for the created channel.
    pub fn new() -> (Self, Receiver<Event>) {
        let (tx, rx) = channel::<Event>();
        (
            Self {
                tx,
                waker: Arc::new(EventLoopWaker::new()),
            },
            rx,
        )
    }

    /// The waker shared by every clone of this sender.
    pub fn waker(&self) -> &Arc<EventLoopWaker> {
        &self.waker
    }

    /// Sends an `Event` through the internal channel.
    ///
    /// # Arguments
    ///
    /// * `event` - The `Event` to send.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the event is sent successfully, otherwise `Err(Error)` if the receiver has disconnected.
    pub fn send(&self, event: Event) -> Result<()> {
        self.tx.send(event)?;
        // After the queue push, so the pump cannot wake to an empty channel and
        // go back to sleep past the event that woke it.
        self.waker.wake();
        Ok(())
    }

    /// Dispatches an action through the application's single action seam.
    pub fn dispatch(&self, action: Action) -> Result<()> {
        self.send(Event::action_requested(action))
    }
}

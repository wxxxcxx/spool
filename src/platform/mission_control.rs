use accessibility_sys::{
    AXObserverRef, AXUIElementCreateApplication, AXUIElementRef,
    kAXErrorNotificationAlreadyRegistered, kAXErrorSuccess,
};
use core::ptr::NonNull;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFRetained, CFString, kCFRunLoopDefaultMode};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};
use objc2_foundation::ns_string;
use std::ffi::c_void;
use std::ptr::null_mut;
use stdext::function_name;
use tracing::{debug, error, warn};

use super::{
    AXObserverAddNotification, AXObserverCreate, AXObserverRemoveNotification, CFStringRef, Pid,
};
use crate::errors::{Error, Result};
use crate::events::{Event, EventSender};
use crate::util::{AXUIWrapper, add_run_loop, remove_run_loop};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemOverview {
    MissionControl,
    ShowDesktop,
}

/// `F11`, the system's default Show Desktop shortcut. The system listens for it
/// with the secondary-fn flag, which is what an Apple keyboard sends when the
/// function keys are not configured as standard function keys.
const SHOW_DESKTOP_KEYCODE: u16 = 0x67;
const SHOW_DESKTOP_FLAGS: CGEventFlags = CGEventFlags::MaskSecondaryFn;

/// How one system overview is requested from macOS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverviewRequest {
    /// Launch the Mission Control docklet with this mode argument.
    Docklet(&'static str),
    /// Post this key event at the HID tap.
    Shortcut(u16, CGEventFlags),
}

impl SystemOverview {
    /// The request that reaches this overview.
    ///
    /// The docklet reads its mode from `atoi(argv[1])` and falls back to
    /// `com.apple.expose.awake` — Mission Control — when it has no argument.
    /// Launch Services does not deliver `--args` to it on current macOS, so the
    /// docklet can only ever reach Mission Control, and Show Desktop has to be
    /// requested through the system shortcut instead. Posting an argumentless
    /// launch for Show Desktop is what made both Bar buttons open Mission
    /// Control.
    fn request(self) -> OverviewRequest {
        match self {
            Self::MissionControl => OverviewRequest::Docklet("0"),
            Self::ShowDesktop => {
                OverviewRequest::Shortcut(SHOW_DESKTOP_KEYCODE, SHOW_DESKTOP_FLAGS)
            }
        }
    }

    /// Requests the overview.
    ///
    /// The docklet launch is reaped on a helper thread: neither launching nor
    /// waiting may stall the main-thread `AppKit` event pump. Posting the shortcut
    /// returns immediately.
    pub(crate) fn launch(self) -> Result<()> {
        match self.request() {
            OverviewRequest::Docklet(argument) => std::thread::Builder::new()
                .name("spool-system-overview".to_owned())
                .spawn(move || match docklet_command(argument).status() {
                    Ok(status) if status.success() => {}
                    Ok(status) => {
                        warn!(overview = ?self, %status, "system overview launch request failed");
                    }
                    Err(error) => {
                        warn!(overview = ?self, %error, "unable to submit system overview launch request");
                    }
                })
                .map(|_| ())
                .map_err(Error::from),
            OverviewRequest::Shortcut(keycode, flags) => post_shortcut(keycode, flags),
        }
    }
}

fn docklet_command(argument: &str) -> std::process::Command {
    // Direct execution of the Docklet violates macOS AMFI launch constraints.
    // Launch Services must start the bundle. The mode argument is sent for
    // completeness only: the docklet's argumentless default is already Mission
    // Control, which is the one mode it can reach.
    let mut command = std::process::Command::new("/usr/bin/open");
    command.args(["-n", "/System/Applications/Mission Control.app", "--args"]);
    command.arg(argument);
    command
}

fn post_shortcut(keycode: u16, flags: CGEventFlags) -> Result<()> {
    for key_down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, keycode, key_down).ok_or_else(|| {
            Error::Generic("unable to create the Show Desktop shortcut".to_string())
        })?;
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

#[cfg(test)]
mod command_tests {
    use super::{OverviewRequest, SystemOverview, docklet_command};

    #[test]
    fn mission_control_uses_the_docklet_and_show_desktop_uses_the_shortcut() {
        assert_eq!(
            SystemOverview::MissionControl.request(),
            OverviewRequest::Docklet("0")
        );
        assert_eq!(
            SystemOverview::ShowDesktop.request(),
            OverviewRequest::Shortcut(0x67, objc2_core_graphics::CGEventFlags::MaskSecondaryFn),
            "the docklet cannot be told to show the desktop, so F11 is the only request that reaches it"
        );
    }

    #[test]
    fn the_docklet_launch_goes_through_launch_services() {
        let command = docklet_command("0");
        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                "-n",
                "/System/Applications/Mission Control.app",
                "--args",
                "0",
            ]
        );
    }
}

/// `MissionControlHandler` manages observation of Mission Control related accessibility events from the Dock process.
/// It dispatches specific `Event` types when Mission Control actions (e.g., showing all windows, showing desktop) occur.
#[derive(Debug)]
pub(super) struct MissionControlHandler {
    /// The `EventSender` to dispatch Mission Control events.
    events: EventSender,
    /// An optional `AXUIWrapper` for the Dock application's UI element.
    element: Option<CFRetained<AXUIWrapper>>,
    /// An optional `AXUIWrapper` for the `AXObserver` instance.
    observer: Option<CFRetained<AXUIWrapper>>,
}

impl MissionControlHandler {
    /// Creates a new `MissionControlHandler` instance.
    ///
    /// # Arguments
    ///
    /// * `events` - An `EventSender` to send Mission Control related events.
    ///
    /// # Returns
    ///
    /// A new `MissionControlHandler`.
    pub(super) fn new(events: EventSender) -> Self {
        Self {
            events,
            element: None,
            observer: None,
        }
    }

    /// A constant array of `&str` representing the Mission Control accessibility event names that are observed.
    const EVENTS: [&str; 4] = [
        "AXExposeShowAllWindows",
        "AXExposeShowFrontWindows",
        "AXExposeShowDesktop",
        "AXExposeExit",
    ];

    /// Handles Mission Control accessibility notifications. It translates the notification string into a corresponding `Event`.
    ///
    /// # Arguments
    ///
    /// * `_observer` - The `AXObserverRef` (unused).
    /// * `_element` - The `AXUIElementRef` (unused).
    /// * `notification` - The name of the Mission Control notification as a string.
    fn mission_control_handler(
        &self,
        _observer: AXObserverRef,
        _element: AXUIElementRef,
        notification: &str,
    ) {
        let event = match notification {
            "AXExposeShowAllWindows" => Event::MissionControlShowAllWindows,
            "AXExposeShowFrontWindows" => Event::MissionControlShowFrontWindows,
            "AXExposeShowDesktop" => Event::MissionControlShowDesktop,
            "AXExposeExit" => Event::MissionControlExit,
            _ => {
                warn!("Unknown mission control event: {notification}");
                return;
            }
        };
        _ = self
            .events
            .send(event)
            .inspect_err(|err| error!("error sending event: {err}"));
    }

    /// Retrieves the process ID (`Pid`) of the Dock application.
    /// This function uses `NSRunningApplication` to find the Dock by its bundle identifier.
    ///
    /// # Returns
    ///
    /// `Ok(Pid)` with the Dock's process ID if found, otherwise `Err(Error)`.
    fn dock_pid() -> Result<Pid> {
        let dock = ns_string!("com.apple.dock");
        let array = NSRunningApplication::runningApplicationsWithBundleIdentifier(dock);
        array
            .iter()
            .next()
            .map(|running| running.processIdentifier())
            .ok_or(Error::NotFound(format!(
                "{}: can not find dock.",
                function_name!()
            )))
    }

    /// Starts observing Mission Control accessibility notifications from the Dock process.
    /// It creates an `AXObserver` for the Dock application and registers for specific Mission Control events.
    /// The observer is then added to the run loop.
    ///
    /// # Returns
    ///
    /// `Ok(())` if observation is started successfully, otherwise `Err(Error)` if permissions are denied or setup fails.
    pub(super) fn observe(&mut self) -> Result<()> {
        let pid = MissionControlHandler::dock_pid()?;
        let element = AXUIWrapper::from_retained(unsafe { AXUIElementCreateApplication(pid) })?;
        let observer = unsafe {
            let mut observer_ref: AXObserverRef = null_mut();
            if kAXErrorSuccess == AXObserverCreate(pid, Self::callback, &mut observer_ref) {
                AXUIWrapper::from_retained(observer_ref)?
            } else {
                return Err(Error::PermissionDenied(format!(
                    "{}: error creating observer.",
                    function_name!()
                )));
            }
        };

        for name in &Self::EVENTS {
            debug!("{name:?} {:?}", observer.as_ptr::<AXObserverRef>());
            let notification = CFString::from_static_str(name);
            match unsafe {
                AXObserverAddNotification(
                    observer.as_ptr(),
                    element.as_ptr(),
                    &notification,
                    NonNull::new_unchecked(self).as_ptr().cast(),
                )
            } {
                accessibility_sys::kAXErrorSuccess
                | accessibility_sys::kAXErrorNotificationAlreadyRegistered => (),
                result => error!("error registering {name} for application {pid}: {result}"),
            }
        }
        unsafe { add_run_loop(&observer, kCFRunLoopDefaultMode)? };
        self.observer = observer.into();
        self.element = element.into();
        Ok(())
    }

    /// Stops observing Mission Control accessibility notifications and cleans up resources.
    /// It removes all registered notifications from the `AXObserver` and invalidates the run loop source.
    ///
    /// # Side Effects
    ///
    /// - Deregisters `AXObserver` notifications.
    /// - Removes the `AXObserver` from the run loop.
    fn unobserve(&mut self) {
        if let Some((observer, element)) = self.observer.take().zip(self.element.as_ref()) {
            for name in &Self::EVENTS {
                debug!("{name:?} {:?}", observer.as_ptr::<AXObserverRef>());
                let notification = CFString::from_static_str(name);
                let result = unsafe {
                    AXObserverRemoveNotification(observer.as_ptr(), element.as_ptr(), &notification)
                };
                if result != kAXErrorSuccess && result != kAXErrorNotificationAlreadyRegistered {
                    error!("error unregistering {name}: {result}");
                }
            }
            remove_run_loop(&observer);
            drop(observer);
        } else {
            warn!("unobserving without observe or element");
        }
    }

    /// The static callback function for the Mission Control `AXObserver`. It dispatches to the `mission_control_handler` method.
    /// This function is declared as `extern "C"`.
    ///
    /// # Arguments
    ///
    /// * `observer` - The `AXObserverRef` that invoked the callback.
    /// * `element` - The `AXUIElementRef` associated with the notification.
    /// * `notification` - The raw `CFStringRef` representing the notification name.
    /// * `context` - A raw pointer to the `MissionControlHandler` instance.
    extern "C" fn callback(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        context: *mut c_void,
    ) {
        let Some(notification) = NonNull::new(notification.cast_mut()) else {
            error!("nullptr 'notification' passed.");
            return;
        };

        if let Some(this) = NonNull::new(context)
            .map(|this| unsafe { this.cast::<MissionControlHandler>().as_ref() })
        {
            let notification = unsafe { notification.as_ref() }.to_string();
            this.mission_control_handler(observer, element, &notification);
        } else {
            error!("Zero passed to MissionControlHandler.");
        }
    }
}

impl Drop for MissionControlHandler {
    /// Unobserves Mission Control notifications when the `MissionControlHandler` is dropped.
    /// This ensures that system resources are properly released when the handler is no longer needed.
    fn drop(&mut self) {
        self.unobserve();
    }
}

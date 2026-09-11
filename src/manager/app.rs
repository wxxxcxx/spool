use accessibility_sys::{
    AXObserverRef, AXUIElementCreateApplication, AXUIElementRef, kAXErrorSuccess,
};
use bevy::ecs::component::Component;
use core::ptr::NonNull;
use derive_more::{DerefMut, with_trait::Deref};
use mockall::automock;
use objc2_core_foundation::{CFRetained, CFString, kCFRunLoopCommonModes};
use std::collections::HashSet;
use std::ffi::c_void;
use std::pin::Pin;
use std::ptr::null_mut;
use std::sync::{Arc, LazyLock, RwLock};
use stdext::sync::rw_lock::RwLockExt;

use stdext::function_name;
use tracing::{debug, error};

use super::skylight::_SLPSGetFrontProcess;
use super::{
    ProcessApi, Window, WindowOS, ax_window_id, process::pid_for_psn,
    windows::ax_window_incarnation,
};
use crate::config::Config;
use crate::errors::{Error, Result};
use crate::events::{DestroySource, Event, EventSender, FocusSource, ReconcileScope};
use crate::platform::{
    AXObserverAddNotification, AXObserverCreate, AXObserverRemoveNotification, CFStringRef, ConnID,
    Pid, ProcessSerialNumber, WinID, WindowIncarnation,
};
use crate::util::{AXUIAttributes, AXUIWrapper, MacResult, add_run_loop, remove_run_loop};

/// A static `LazyLock` that holds a list of `AXNotification` strings to be observed for application-level events.
/// These notifications are general events related to an application's lifecycle and state changes,
/// such as a new window being created, the focused window changing, or a menu being opened/closed.
pub static AX_NOTIFICATIONS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        accessibility_sys::kAXCreatedNotification,
        accessibility_sys::kAXFocusedWindowChangedNotification,
        accessibility_sys::kAXFocusedUIElementChangedNotification,
        accessibility_sys::kAXWindowMovedNotification,
        accessibility_sys::kAXWindowResizedNotification,
        accessibility_sys::kAXMenuOpenedNotification,
        accessibility_sys::kAXMenuClosedNotification,
    ]
});

/// A static `LazyLock` that holds a list of `AXNotification` strings to be observed for window-specific events.
/// These notifications are related to individual window lifecycle events,
/// such as a window being destroyed, miniaturized (minimized), or deminiaturized (restored).
pub static AX_WINDOW_NOTIFICATIONS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        accessibility_sys::kAXUIElementDestroyedNotification,
        accessibility_sys::kAXWindowMiniaturizedNotification,
        accessibility_sys::kAXWindowDeminiaturizedNotification,
        // Observed per window, not per application: on the app observer this
        // notification's element carries no resolvable window id, which left
        // `WindowOS::title` caching a title it was never told to invalidate.
        accessibility_sys::kAXTitleChangedNotification,
    ]
});

fn inventory_window_identity(
    role: Result<String>,
    identity: impl FnOnce() -> Result<(WinID, WindowIncarnation)>,
) -> Result<Option<(WinID, WindowIncarnation)>> {
    // Finder includes its desktop AXScrollArea in AXWindows. It is not a
    // missing window identity and must not poison the complete snapshot.
    if role? != "AXWindow" {
        return Ok(None);
    }
    identity().map(Some)
}

#[automock]
pub trait ApplicationApi: Send + Sync {
    /// Returns the process ID of the application.
    fn pid(&self) -> Pid;
    /// Returns the process serial number of the application.
    fn psn(&self) -> ProcessSerialNumber;
    /// Returns the connection ID of the application.
    fn connection(&self) -> Option<ConnID>;
    /// Returns the ID of the currently focused window for this application.
    ///
    /// # Errors
    ///
    /// Returns an `Error` if the focused window cannot be determined.
    fn focused_window_id(&self) -> Result<WinID>;
    /// Returns one AX snapshot containing both raw identities and the subset
    /// accepted by Spool's trackability rules.
    fn window_inventory(&self, config: &Config) -> Result<ApplicationWindowInventory>;
    /// Whether this application owns this concrete AX window incarnation.
    fn owns_window(&self, window: &Window) -> Result<bool>;
    /// Returns a list of all windows belonging to this application.
    ///
    /// # Arguments
    ///
    /// * `config` - The current Spool configuration, used to evaluate window rules.
    ///
    /// # Errors
    ///
    /// Returns an `Error` if the window list cannot be retrieved.
    fn window_list(&self, config: &Config) -> Vec<Window>;
    /// Starts observing supported application-level accessibility notifications.
    /// `Ok(true)` means no retryable registrations remain, not that every
    /// requested notification is supported. Inventory reconciliation is fallback.
    ///
    /// # Errors
    ///
    /// Returns an `Error` if observers cannot be registered.
    fn observe(&mut self) -> Result<bool>;
    /// Starts observing window-specific accessibility notifications for a given window.
    /// Unsupported notifications settle without making the window untrackable.
    ///
    /// # Arguments
    ///
    /// * `window` - The `Window` to observe.
    ///
    /// # Errors
    ///
    /// Returns an `Error` if observers cannot be registered.
    fn observe_window(&mut self, window: &Window) -> Result<bool>;
    /// Stops observing window-specific accessibility notifications for a given window.
    ///
    /// # Arguments
    ///
    /// * `window` - The `Window` to unobserve.
    fn unobserve_window(&mut self, window: &Window);
    /// Checks if the application is currently the frontmost application.
    fn is_frontmost(&self) -> bool;
    /// Returns the bundle identifier of the application.
    fn bundle_id(&self) -> Option<String>;
    /// Returns the display name of the application.
    fn name(&self) -> &str;
    /// Whether the original process serial number still resolves to this PID.
    ///
    /// Query failures are distinct from a definitively terminated process so a
    /// transient Process Manager error cannot retire a live application.
    fn is_running(&self) -> Result<bool>;
}

pub struct ApplicationWindowInventory {
    pub identities: Vec<(WinID, WindowIncarnation)>,
    pub candidates: Vec<Window>,
    /// False when the AX window list succeeded but at least one element could
    /// not be identified. Known entries remain useful for discovery, while
    /// absence from this snapshot is not yet destructive evidence.
    pub complete: bool,
}

/// A wrapper struct for `ApplicationApi` trait objects, allowing for dynamic dispatch.
/// It implements `Deref` and `DerefMut` to easily access the underlying `ApplicationApi` methods.
#[derive(Component, Deref, DerefMut)]
pub struct Application(Box<dyn ApplicationApi>);

impl std::fmt::Debug for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "app (pid {})", self.pid())
    }
}

impl std::fmt::Display for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "app (pid {})", self.pid())
    }
}

impl Application {
    /// Creates a new `Application` instance from a boxed `ApplicationApi` trait object.
    ///
    /// # Arguments
    ///
    /// * `app` - A `Box<dyn ApplicationApi>` representing the application implementation.
    pub fn new(app: Box<dyn ApplicationApi>) -> Self {
        Application(app)
    }
}

/// `ApplicationOS` is a concrete implementation of the `ApplicationApi` trait for macOS.
/// It manages an application's accessibility UI element, process information, and event observation.
pub struct ApplicationOS {
    element: CFRetained<AXUIWrapper>,
    psn: ProcessSerialNumber,
    pid: Pid,
    connection: Option<ConnID>,
    handler: AxObserverHandler,
    bundle_id: Option<String>,
    name: String,
}

impl Drop for ApplicationOS {
    /// Cleans up the `AXObserver` by removing all registered notifications when the `Application` is dropped.
    fn drop(&mut self) {
        self.handler
            .remove_observer(&ObserverType::Application, &self.element, &AX_NOTIFICATIONS);
    }
}

impl std::fmt::Display for ApplicationOS {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "app '{}' (pid {})", self.name, self.pid)
    }
}

impl ApplicationOS {
    /// Creates a new `Application` instance for a given process.
    /// It obtains the Accessibility UI element for the application and its connection ID.
    ///
    /// # Arguments
    ///
    /// * `connection` - The main connection ID for the `SkyLight` API.
    /// * `process` - A reference to the `Process` associated with this application.
    /// * `events` - An `EventSender` to send events from the `AXObserver`.
    ///
    /// # Returns
    ///
    /// `Ok(Self)` if the `Application` is created successfully, otherwise `Err(Error)`.
    pub fn new(
        connection: Option<ConnID>,
        process: &dyn ProcessApi,
        events: &EventSender,
    ) -> Result<Self> {
        let refer =
            AXUIWrapper::from_retained(unsafe { AXUIElementCreateApplication(process.pid()) })?;
        let bundle_id = process
            .application()
            .as_ref()
            .and_then(|app| app.bundleIdentifier())
            .map(|id| id.to_string());
        Ok(Self {
            element: refer,
            psn: process.psn(),
            pid: process.pid(),
            connection,
            handler: AxObserverHandler::new(process.pid(), events.clone())?,
            bundle_id,
            name: process.name().to_string(),
        })
    }
}

impl ApplicationApi for ApplicationOS {
    /// Retrieves the process ID (Pid) of the application.
    ///
    /// # Returns
    ///
    /// The process ID.
    fn pid(&self) -> Pid {
        self.pid
    }

    /// Retrieves the `ProcessSerialNumber` of the application.
    ///
    /// # Returns
    ///
    /// The process serial number.
    fn psn(&self) -> ProcessSerialNumber {
        self.psn
    }

    /// Retrieves the connection ID (`ConnID`) of the application.
    ///
    /// # Returns
    ///
    /// The connection ID.
    fn connection(&self) -> Option<ConnID> {
        self.connection
    }

    /// Retrieves the focused window ID of the application.
    ///
    /// # Returns
    ///
    /// `Ok(WinID)` with the focused window ID if successful, otherwise `Err(Error)`.
    fn focused_window_id(&self) -> Result<WinID> {
        self.element.focused_window_id()
    }

    fn window_inventory(&self, config: &Config) -> Result<ApplicationWindowInventory> {
        let bundle_id = self.bundle_id.as_deref();
        let mut identities = Vec::new();
        let mut candidates = Vec::new();
        let mut complete = true;
        for element in self.element.windows()? {
            let identity = inventory_window_identity(element.role(), || {
                Ok((
                    ax_window_id(element.as_ptr())?,
                    ax_window_incarnation(&element),
                ))
            });
            let Ok(identity) = identity.inspect_err(|error| {
                debug!(%error, "unable to identify one AX window inventory element");
            }) else {
                complete = false;
                continue;
            };
            let Some(identity) = identity else {
                continue;
            };
            identities.push(identity);
            if let Ok(window) = WindowOS::new_with_config(&element, config, bundle_id) {
                candidates.push(Window::new(Box::new(window)));
            }
        }
        Ok(ApplicationWindowInventory {
            identities,
            candidates,
            complete,
        })
    }

    fn owns_window(&self, window: &Window) -> Result<bool> {
        let target = (window.id(), window.incarnation());
        for element in self.element.windows()? {
            let Ok(window_id) = ax_window_id(element.as_ptr()).inspect_err(|error| {
                debug!(%error, "unable to identify one AX ownership element");
            }) else {
                continue;
            };
            let identity = (window_id, ax_window_incarnation(&element));
            if identity == target {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Retrieves a list of all windows associated with the application.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<Result<Window>>)` containing the list of window objects if successful, otherwise `Err(Error)`.
    fn window_list(&self, config: &Config) -> Vec<Window> {
        let bundle_id = self.bundle_id.as_deref();
        self.element
            .windows()
            .map(|windows| {
                windows
                    .into_iter()
                    .flat_map(|element| {
                        WindowOS::new_with_config(&element, config, bundle_id)
                            .map(|window| Window::new(Box::new(window)))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Registers observers for general application-level accessibility notifications (e.g., `kAXCreatedNotification`).
    ///
    /// # Returns
    ///
    /// `Ok(true)` once every notification is registered or known unsupported;
    /// `Ok(false)`/`Err` retain retryable registration failures.
    fn observe(&mut self) -> Result<bool> {
        self.handler
            .add_observer(&self.element, &AX_NOTIFICATIONS, ObserverType::Application)
            .map(|retry| retry.is_empty())
    }

    /// Registers observers for specific window-level accessibility notifications (e.g., `kAXUIElementDestroyedNotification`).
    ///
    /// # Arguments
    ///
    /// * `window` - A reference to the `Window` object to observe.
    ///
    /// # Returns
    ///
    /// `Ok(true)` once every notification is registered or known unsupported;
    /// `Ok(false)`/`Err` retain retryable registration failures.
    fn observe_window(&mut self, window: &Window) -> Result<bool> {
        if let Some(element) = window.element() {
            self.handler
                .add_observer(
                    &element,
                    &AX_WINDOW_NOTIFICATIONS,
                    ObserverType::Window(window.id(), window.incarnation()),
                )
                .map(|retry| retry.is_empty())
        } else {
            Err(Error::InvalidWindow)
        }
    }

    /// Unregisters observers for a specific window's accessibility notifications.
    ///
    /// # Arguments
    ///
    /// * `window` - A reference to the `Window` object to unobserve.
    fn unobserve_window(&mut self, window: &Window) {
        if let Some(element) = window.element() {
            self.handler.remove_observer(
                &ObserverType::Window(window.id(), window.incarnation()),
                &element,
                &AX_WINDOW_NOTIFICATIONS,
            );
        }
    }

    /// Checks if the application is currently the frontmost application.
    ///
    /// # Returns
    ///
    /// `true` if the application is frontmost, `false` otherwise.
    fn is_frontmost(&self) -> bool {
        let mut psn = ProcessSerialNumber::default();
        unsafe { _SLPSGetFrontProcess(&mut psn) }
            .to_result(function_name!())
            .is_ok()
            && self.psn == psn
    }

    /// Returns the bundle identifier of the application.
    ///
    /// # Returns
    ///
    /// An `Option<&str>` containing the bundle ID if available, otherwise `None`.
    fn bundle_id(&self) -> Option<String> {
        self.bundle_id.clone()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn is_running(&self) -> Result<bool> {
        pid_for_psn(self.psn).map(|pid| pid == Some(self.pid))
    }
}

/// An enum representing the type of observer being used.
/// `Application` refers to an observer for application-level events.
/// `Window(WinID)` refers to an observer for a specific window, identified by its `WinID`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ObserverType {
    Application,
    Window(WinID, WindowIncarnation),
}

/// `ObserverContext` holds the `EventSender` and the `ObserverType`,
/// which are used within the `AXObserver` callback to dispatch accessibility events.
struct ObserverContext {
    events: EventSender,
    which: ObserverType,
    pid: Pid,
}

impl ObserverContext {
    /// Notifies the event sender about an accessibility event.
    /// It dispatches the event to either `notify_app` or `notify_window` based on the `ObserverType`.
    ///
    /// # Arguments
    ///
    /// * `notification` - The name of the accessibility notification as a `&str`.
    /// * `element` - The `AXUIElementRef` associated with the notification.
    fn notify(&self, notification: &str, element: AXUIElementRef) {
        match self.which {
            ObserverType::Application => self.notify_app(notification, element),
            ObserverType::Window(id, incarnation) => {
                self.notify_window(notification, id, incarnation);
            }
        }
    }

    /// Notifies the event sender about an application-level accessibility event.
    /// It translates the notification string and element into a corresponding `Event`.
    ///
    /// # Arguments
    ///
    /// * `notification` - The name of the accessibility notification as a `&str`.
    /// * `element` - The `AXUIElementRef` associated with the notification.
    fn notify_app(&self, notification: &str, element: AXUIElementRef) {
        // Handled before the window-id lookup below: a creation notification is
        // the one case whose element is not yet a window spool knows about.
        if notification == accessibility_sys::kAXCreatedNotification {
            let Ok(element) = AXUIWrapper::retain(element).inspect_err(|err| {
                error!("invalid element {element:?}: {err}");
            }) else {
                return;
            };
            _ = self.events.send(Event::WindowCreated { element });
            self.request_reconciliation();
            return;
        }

        if notification == accessibility_sys::kAXFocusedUIElementChangedNotification {
            _ = self.events.send(Event::FocusRevalidationRequested {
                pid: self.pid,
                source: FocusSource::AccessibilityUiElement,
            });
            return;
        }
        if notification == accessibility_sys::kAXFocusedWindowChangedNotification {
            _ = self.events.send(Event::FocusRevalidationRequested {
                pid: self.pid,
                source: FocusSource::AccessibilityWindow,
            });
            self.request_reconciliation();
            return;
        }

        let Ok(window_id) =
            ax_window_id(element).inspect_err(|err| debug!("notification {notification}: {err}"))
        else {
            return;
        };
        let event = match notification {
            accessibility_sys::kAXWindowMovedNotification => {
                let Ok(element) = AXUIWrapper::retain(element).inspect_err(|err| {
                    debug!(window_id, "unable to retain moved window element: {err}");
                }) else {
                    return;
                };
                Event::WindowMoved {
                    window_id,
                    incarnation: ax_window_incarnation(&element),
                }
            }
            accessibility_sys::kAXWindowResizedNotification => {
                let Ok(element) = AXUIWrapper::retain(element).inspect_err(|err| {
                    debug!(window_id, "unable to retain resized window element: {err}");
                }) else {
                    return;
                };
                Event::WindowResized {
                    window_id,
                    incarnation: ax_window_incarnation(&element),
                }
            }
            accessibility_sys::kAXMenuOpenedNotification => Event::MenuOpened { window_id },
            accessibility_sys::kAXMenuClosedNotification => Event::MenuClosed { window_id },
            _ => {
                error!("unhandled application notification: {notification:?}");
                return;
            }
        };
        _ = self.events.send(event);
    }

    fn request_reconciliation(&self) {
        _ = self.events.send(Event::ReconcileWindows {
            scope: ReconcileScope::Application(self.pid),
        });
    }

    /// Notifies the event sender about a window-level accessibility event.
    /// It translates the notification string and window ID into a corresponding `Event`.
    ///
    /// # Arguments
    ///
    /// * `notification` - The name of the accessibility notification as a `&str`.
    /// * `window_id` - The ID of the window associated with the notification.
    fn notify_window(&self, notification: &str, window_id: WinID, incarnation: WindowIncarnation) {
        let event = match notification {
            accessibility_sys::kAXWindowMiniaturizedNotification => Event::WindowMinimized {
                window_id,
                incarnation: Some(incarnation),
            },
            accessibility_sys::kAXWindowDeminiaturizedNotification => Event::WindowDeminimized {
                window_id,
                incarnation: Some(incarnation),
            },
            accessibility_sys::kAXUIElementDestroyedNotification => Event::WindowDestroyed {
                window_id,
                source: DestroySource::Accessibility,
                incarnation: Some(incarnation),
            },
            accessibility_sys::kAXTitleChangedNotification => Event::WindowTitleChanged {
                window_id,
                incarnation: Some(incarnation),
            },

            _ => {
                error!("unhandled window notification: {notification:?}");
                return;
            }
        };
        _ = self.events.send(event);
    }
}

#[derive(Default)]
struct NotificationRegistrations {
    // Registration support belongs to one observer target, not the entire app
    // family. A reused window ID must not inherit another incarnation's result.
    settled: HashSet<(ObserverType, &'static str)>,
}

impl NotificationRegistrations {
    fn add(
        &mut self,
        which: ObserverType,
        notifications: &[&'static str],
        mut register: impl FnMut(&'static str) -> i32,
    ) -> Result<Vec<&'static str>> {
        let mut retry = Vec::new();
        let mut any_settled = false;
        let mut first_error = None;
        for &name in notifications {
            if self.settled.contains(&(which, name)) {
                any_settled = true;
                continue;
            }
            let result = register(name);
            if result == accessibility_sys::kAXErrorSuccess
                || result == accessibility_sys::kAXErrorNotificationAlreadyRegistered
                || result == accessibility_sys::kAXErrorNotificationUnsupported
            {
                if result == accessibility_sys::kAXErrorNotificationUnsupported {
                    debug!(
                        ?which,
                        notification = name,
                        result,
                        "AX notification unsupported; using reconciliation fallback"
                    );
                }
                self.settled.insert((which, name));
                any_settled = true;
            } else {
                // A transport or permission failure affects the endpoint, not
                // this notification. Let the caller back off the whole probe.
                if matches!(
                    result,
                    accessibility_sys::kAXErrorCannotComplete
                        | accessibility_sys::kAXErrorAPIDisabled
                ) {
                    return Err(Error::macos(
                        format!("AXObserverAddNotification({name})"),
                        result,
                    ));
                }
                error!(
                    ?which,
                    notification = name,
                    result,
                    "error adding AX observer"
                );
                first_error.get_or_insert_with(|| {
                    Error::macos(format!("AXObserverAddNotification({name})"), result)
                });
                retry.push(name);
            }
        }
        if !any_settled && let Some(error) = first_error {
            return Err(error);
        }
        Ok(retry)
    }

    fn remove(&mut self, which: ObserverType, notifications: &[&'static str]) {
        for &name in notifications {
            self.settled.remove(&(which, name));
        }
    }
}

/// `AxObserverHandler` manages the lifecycle of an `AXObserver`,
/// including its creation, registration of notifications, and removal from the run loop.
struct AxObserverHandler {
    observer: CFRetained<AXUIWrapper>,
    events: EventSender,
    pid: Pid,
    contexts: Arc<RwLock<Vec<Pin<Box<ObserverContext>>>>>,
    registrations: NotificationRegistrations,
}

impl Drop for AxObserverHandler {
    /// Invalidates the run loop source associated with the `AXObserver` when the `AxObserverHandler` is dropped.
    fn drop(&mut self) {
        remove_run_loop(&self.observer);
    }
}

impl AxObserverHandler {
    /// Creates a new `AxObserverHandler` instance for a given process ID.
    /// It creates an `AXObserver` and adds its run loop source to the main run loop.
    ///
    /// # Arguments
    ///
    /// * `pid` - The process ID to create the observer for.
    /// * `events` - An `EventSender` to send events generated by the observer.
    ///
    /// # Returns
    ///
    /// `Ok(Self)` if the handler is created successfully, otherwise `Err(Error)`.
    fn new(pid: Pid, events: EventSender) -> Result<Self> {
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

        unsafe { add_run_loop(&observer, kCFRunLoopCommonModes)? };
        Ok(Self {
            observer,
            events,
            pid,
            contexts: Arc::new(RwLock::new(Vec::new())),
            registrations: NotificationRegistrations::default(),
        })
    }

    /// Adds accessibility notifications to be observed for a given UI element.
    ///
    /// # Arguments
    ///
    /// * `element` - The `&AXUIWrapper` to observe.
    /// * `notifications` - A slice of static strings representing the notification names to add.
    /// * `which` - The type of observer context (application-specific or window-specific).
    ///
    /// # Returns
    ///
    /// The notifications still eligible for retry. Unsupported notifications
    /// are settled for this observer target, not reported as permission errors.
    pub fn add_observer(
        &mut self,
        element: &AXUIWrapper,
        notifications: &[&'static str],
        which: ObserverType,
    ) -> Result<Vec<&str>> {
        let observer: AXObserverRef = self.observer.as_ptr();
        let context_ptr = self.get_or_insert_context(which).as_ptr();

        let _span = tracing::debug_span!(
            "ax_observer_registration",
            pid = self.pid,
            ?which,
            ?element,
            ?observer
        )
        .entered();
        self.registrations.add(which, notifications, |name| {
            debug!("adding {name} {element:x?} {observer:?}");
            let notification = CFString::from_static_str(name);
            unsafe {
                AXObserverAddNotification(
                    observer,
                    element.as_ptr(),
                    &notification,
                    context_ptr.cast(),
                )
            }
        })
    }

    /// Removes accessibility notifications from being observed for a given UI element.
    ///
    /// # Arguments
    ///
    /// * `which` - The type of observer context (application-specific or window-specific) for which to remove notifications.
    /// * `element` - The `&AXUIWrapper` from which to remove notifications.
    /// * `notifications` - A slice of static strings representing the notification names to remove.
    pub fn remove_observer(
        &mut self,
        which: &ObserverType,
        element: &AXUIWrapper,
        notifications: &[&'static str],
    ) {
        self.registrations.remove(*which, notifications);
        if self.get_context(*which).is_none() {
            debug!("{which:?} ({element}) already un-observed, skipping!");
            return;
        }

        for name in notifications {
            let observer: AXObserverRef = self.observer.deref().as_ptr();
            let notification = CFString::from_static_str(name);
            debug!("removing {name} {element:x?} {observer:?}");
            let result =
                unsafe { AXObserverRemoveNotification(observer, element.as_ptr(), &notification) };
            if result != kAXErrorSuccess {
                debug!("error removing {name} {element:x?} {observer:?}: {result}");
            }
        }
        // AX may already have queued callbacks with this context pointer. Keep
        // every refcon alive until `drop` removes the observer's run-loop source.
    }

    /// The static callback function for `AXObserver`. This function is called by the macOS Accessibility API
    /// when an observed accessibility event occurs. It dispatches the event to the appropriate `notify_app` or `notify_window` handler.
    ///
    /// # Arguments
    ///
    /// * `_observer` - The `AXObserverRef` (unused).
    /// * `element` - The `AXUIElementRef` associated with the notification.
    /// * `notification` - The raw `CFStringRef` representing the notification name.
    /// * `context` - A raw pointer to the user-defined context `ObserverContext`.
    extern "C" fn callback(
        _observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        context: *mut c_void,
    ) {
        let notification = NonNull::new(notification.cast_mut())
            .map(|ptr| unsafe { ptr.as_ref() })
            .map(CFString::to_string);
        let context =
            NonNull::new(context.cast::<ObserverContext>()).map(|ptr| unsafe { ptr.as_ref() });
        let Some((notification, context)) = notification.zip(context) else {
            return;
        };

        context.notify(&notification, element);
    }

    fn get_context(&self, which: ObserverType) -> Option<NonNull<ObserverContext>> {
        let contexts = self.contexts.force_read();
        contexts
            .iter()
            .find(|context| context.which == which)
            .map(|context| NonNull::from_ref(context.as_ref().get_ref()))
    }

    fn get_or_insert_context(
        &self,
        // contexts: &mut Vec<Pin<Box<ObserverContext>>>,
        which: ObserverType,
    ) -> NonNull<ObserverContext> {
        if let Some(context) = self.get_context(which) {
            return context;
        }
        self.contexts.force_write().push(Box::pin(ObserverContext {
            events: self.events.clone(),
            which,
            pid: self.pid,
        }));
        self.get_context(which)
            .expect("inserted observer context must be present")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finder_desktop_does_not_make_window_inventory_incomplete() {
        let desktop = super::inventory_window_identity(Ok("AXScrollArea".into()), || {
            Err(crate::errors::Error::InvalidWindow)
        });
        assert!(
            matches!(desktop, Ok(None)),
            "non-window desktop is not a failed window identity"
        );
    }

    #[test]
    fn unreadable_window_identity_still_makes_inventory_incomplete() {
        let window = super::inventory_window_identity(Ok("AXWindow".into()), || {
            Err(crate::errors::Error::InvalidWindow)
        });
        assert!(window.is_err());
    }

    #[test]
    fn unreadable_role_does_not_silently_discard_a_window() {
        let result =
            super::inventory_window_identity(Err(crate::errors::Error::InvalidWindow), || {
                Ok((42, 1))
            });
        assert!(result.is_err());
    }
    use std::{
        collections::HashMap,
        io::{self, Write},
        mem::ManuallyDrop,
        ptr::NonNull,
        sync::{Arc, Mutex, RwLock},
    };
    use stdext::sync::rw_lock::RwLockExt as _;

    use super::{NotificationRegistrations, ObserverType};
    use crate::{events::EventSender, manager::app::AxObserverHandler, util::AXUIWrapper};

    #[test]
    fn same_observer_type_reuses_stable_context_pointer() {
        let fake_ptr = NonNull::<AXUIWrapper>::dangling().as_ptr();
        let observer = AXUIWrapper::from_retained(fake_ptr).unwrap();
        let (events, _receiver) = EventSender::new();
        let handler = ManuallyDrop::new(AxObserverHandler {
            observer,
            events,
            pid: 1,
            contexts: Arc::new(RwLock::new(Vec::new())),
            registrations: NotificationRegistrations::default(),
        });

        let first = handler.get_or_insert_context(ObserverType::Window(42, 1));
        let reused = handler.get_or_insert_context(ObserverType::Window(42, 1));

        assert_eq!(reused, first);
        assert_eq!(handler.contexts.as_ref().force_read().len(), 1);
    }

    #[test]
    fn unsupported_ax_notifications_stop_retrying() {
        let notifications = [
            "AXFocusedWindowChanged",
            "AXWindowMoved",
            "AXWindowResized",
            "AXMenuOpened",
            "AXMenuClosed",
        ];
        let mut registrations = NotificationRegistrations::default();
        let mut complete = false;
        let mut attempts = 0;
        for _ in 0..3 {
            if complete {
                break;
            }
            complete = registrations
                .add(ObserverType::Application, &notifications, |_| {
                    attempts += 1;
                    accessibility_sys::kAXErrorNotificationUnsupported
                })
                .is_ok_and(|retry| retry.is_empty());
        }
        assert_eq!(
            attempts,
            notifications.len(),
            "-25207 must settle as unsupported, not be re-registered on each reconciliation"
        );
        assert!(
            complete,
            "an element supporting no notifications must not be misreported as a permission failure"
        );
    }

    #[test]
    fn ax_communication_failure_stops_the_registration_batch() {
        let mut registrations = NotificationRegistrations::default();
        let mut attempts = 0;
        let error = registrations
            .add(ObserverType::Application, &super::AX_NOTIFICATIONS, |_| {
                attempts += 1;
                accessibility_sys::kAXErrorCannotComplete
            })
            .unwrap_err();

        assert_eq!(
            error.macos_code(),
            Some(accessibility_sys::kAXErrorCannotComplete)
        );
        assert_eq!(
            attempts, 1,
            "an unavailable AX endpoint must not be called again for every notification"
        );
    }

    #[test]
    fn ax_registration_retries_only_unsettled_notifications() {
        let mut registrations = NotificationRegistrations::default();
        let mut attempts = HashMap::new();
        let notifications = ["AXCreated", "AXMenuOpened", "AXWindowResized"];
        let mut register = |name| {
            let attempt = attempts.entry(name).or_insert(0);
            *attempt += 1;
            match name {
                "AXCreated" => accessibility_sys::kAXErrorNotificationAlreadyRegistered,
                "AXMenuOpened" => accessibility_sys::kAXErrorNotificationUnsupported,
                _ if *attempt == 1 => accessibility_sys::kAXErrorCannotComplete,
                _ => accessibility_sys::kAXErrorSuccess,
            }
        };
        assert_eq!(
            registrations
                .add(ObserverType::Application, &notifications, &mut register)
                .unwrap_err()
                .macos_code(),
            Some(accessibility_sys::kAXErrorCannotComplete)
        );
        assert!(
            registrations
                .add(ObserverType::Application, &notifications, &mut register)
                .unwrap()
                .is_empty()
        );
        assert!(
            registrations
                .add(ObserverType::Application, &notifications, |_| panic!(
                    "settled notifications must not be re-registered"
                ))
                .unwrap()
                .is_empty()
        );
        assert_eq!(attempts["AXCreated"], 1);
        assert_eq!(attempts["AXMenuOpened"], 1);
        assert_eq!(attempts["AXWindowResized"], 2);
    }

    #[test]
    fn ax_registration_preserves_actual_errors_and_can_recover() {
        for code in [
            accessibility_sys::kAXErrorCannotComplete,
            accessibility_sys::kAXErrorAPIDisabled,
        ] {
            let mut registrations = NotificationRegistrations::default();
            let notifications = ["AXCreated"];
            let error = registrations
                .add(ObserverType::Application, &notifications, |_| code)
                .unwrap_err();
            assert_eq!(error.macos_code(), Some(code));
            assert!(
                registrations
                    .add(ObserverType::Application, &notifications, |_| {
                        accessibility_sys::kAXErrorSuccess
                    })
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn ax_registration_cache_is_per_target_incarnation_and_clears_on_remove() {
        let mut registrations = NotificationRegistrations::default();
        let notifications = ["AXTitleChanged"];
        let mut attempts = 0;
        let mut register = |_| {
            attempts += 1;
            accessibility_sys::kAXErrorNotificationUnsupported
        };
        for which in [
            ObserverType::Application,
            ObserverType::Window(42, 1),
            ObserverType::Window(42, 2),
            ObserverType::Window(43, 1),
        ] {
            for _ in 0..2 {
                assert!(
                    registrations
                        .add(which, &notifications, &mut register)
                        .unwrap()
                        .is_empty()
                );
            }
        }
        registrations.remove(ObserverType::Window(42, 1), &notifications);
        assert!(
            registrations
                .add(ObserverType::Window(42, 1), &notifications, &mut register)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            attempts, 5,
            "each target is probed once; removing a subscription resets only that target"
        );
    }

    struct CapturedLog(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn unsupported_ax_notifications_never_emit_error_logs() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(move || CapturedLog(captured.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut registrations = NotificationRegistrations::default();
            for _ in 0..3 {
                assert!(
                    registrations
                        .add(ObserverType::Application, &["AXWindowResized"], |_| {
                            accessibility_sys::kAXErrorNotificationUnsupported
                        })
                        .unwrap()
                        .is_empty()
                );
            }
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(!log.contains("ERROR"), "{log}");
        assert_eq!(
            log.matches("AX notification unsupported").count(),
            1,
            "{log}"
        );
    }
}

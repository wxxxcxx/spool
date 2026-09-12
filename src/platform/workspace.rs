use objc2::rc::Retained;
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey};
use objc2_foundation::{
    NSDictionary, NSDistributedNotificationCenter, NSKeyValueChangeNewKey, NSNotification,
    NSNotificationCenter, NSNumber, NSObject, NSString,
};
use std::ffi::c_void;
use tracing::{debug, info};

use crate::events::{Event, EventSender, ReconcileScope};
use crate::platform::{Pid, notify::CallbackRegistry};

/// `Ivars` is a helper struct to hold instance variables for Objective-C classes implemented in Rust.
/// It primarily stores an `EventSender` for communication with the main event loop.
#[derive(Debug)]
pub struct Ivars {
    /// The `EventSender` to dispatch events.
    events: EventSender,
    process_observations: CallbackRegistry<ProcessObservation>,
}

#[derive(Debug)]
struct ProcessObservation {
    pid: Pid,
    key_path: &'static str,
}

fn process_observation_ready(key_path: &str, value: Option<i32>) -> bool {
    match key_path {
        "finishedLaunching" => value == Some(1),
        "activationPolicy" => matches!(value, Some(0 | 1)),
        _ => false,
    }
}

define_class!(
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `Observer` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    // If we were implementing delegate methods like `NSApplicationDelegate`,
    // we would specify the object to only be usable on the main thread:
    // #[thread_kind = MainThreadOnly]
    #[name = "Observer"]
    #[ivars = Ivars]
    #[derive(Debug)]
    pub struct WorkspaceObserver;

    impl WorkspaceObserver {
        /// Called when the active space changes.
        ///
        /// # Arguments
        ///
        /// * `_` - The notification object (unused).
        #[unsafe(method(activeSpaceDidChange:))]
        fn space_changed(&self, _: &NSNotification) {
            _ = self.ivars().events.send(Event::SpaceChanged);
        }

        /// Called when an application is hidden.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object containing application info.
        #[unsafe(method(didHideApplication:))]
        fn application_hidden(&self, notification: &NSObject) {
            let pid = unsafe {
                let user_info: &NSDictionary = msg_send![notification, userInfo];
                let app: &NSRunningApplication =  msg_send![user_info, objectForKey: NSWorkspaceApplicationKey];
                app.processIdentifier()
            };

            let msg = Event::ApplicationHidden{ pid };
            _ = self.ivars().events.send(msg);
        }

        /// Called when an application becomes the active application.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object containing application info.
        #[unsafe(method(didActivateApplication:))]
        fn application_activated(&self, notification: &NSObject) {
            let pid = unsafe {
                let user_info: &NSDictionary = msg_send![notification, userInfo];
                let app: &NSRunningApplication =  msg_send![user_info, objectForKey: NSWorkspaceApplicationKey];
                app.processIdentifier()
            };

            let msg = Event::ApplicationActivated{ pid };
            _ = self.ivars().events.send(msg);
        }

        /// Called when an application stops being the active application.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object containing application info.
        #[unsafe(method(didDeactivateApplication:))]
        fn application_deactivated(&self, notification: &NSObject) {
            let pid = unsafe {
                let user_info: &NSDictionary = msg_send![notification, userInfo];
                let app: &NSRunningApplication =  msg_send![user_info, objectForKey: NSWorkspaceApplicationKey];
                app.processIdentifier()
            };

            let msg = Event::ApplicationDeactivated{ pid };
            _ = self.ivars().events.send(msg);
        }

        /// Called when an application is unhidden.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object containing application info.
        #[unsafe(method(didUnhideApplication:))]
        fn application_unhidden(&self, notification: &NSObject) {
            let pid = unsafe {
                let user_info: &NSDictionary = msg_send![notification, userInfo];
                let app: &NSRunningApplication =  msg_send![user_info, objectForKey: NSWorkspaceApplicationKey];
                app.processIdentifier()
            };
            let msg = Event::ApplicationVisible{ pid };
            _ = self.ivars().events.send(msg);
        }

        /// Called when the system wakes from sleep.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object.
        #[unsafe(method(didWake:))]
        fn system_woke(&self, notification: &NSObject) {
            let msg = Event::SystemWoke{
                msg: format!("WorkspaceObserver: {notification:?}"),
            };
            _ = self.ivars().events.send(msg);
        }

        /// Called when the menu bar hiding state changes.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object.
        #[unsafe(method(didChangeMenuBarHiding:))]
        fn menubar_hidden(&self, notification: &NSObject) {
            let msg = Event::MenuBarHiddenChanged{
                msg: format!("WorkspaceObserver: {notification:?}"),
            };
            _ = self.ivars().events.send(msg);
        }

        /// Called when the Dock restarts.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object.
        #[unsafe(method(didRestartDock:))]
        fn dock_restarted(&self, notification: &NSObject) {
            let msg = Event::DockDidRestart{
                msg: format!("WorkspaceObserver: {notification:?}"),
            };
            _ = self.ivars().events.send(msg);
        }

        /// Called when Dock preferences change.
        ///
        /// # Arguments
        ///
        /// * `notification` - The notification object.
        #[unsafe(method(didChangeDockPref:))]
        fn dock_pref_changed(&self, notification: &NSObject) {
            let msg = Event::DockDidChangePref{
                msg: format!("WorkspaceObserver: {notification:?}"),
            };
            _ = self.ivars().events.send(msg);
        }

        /// Called when the system theme (Light/Dark mode) changes.
        ///
        /// # Arguments
        ///
        /// * `_` - The notification object (unused).
        #[unsafe(method(didChangeTheme:))]
        fn theme_changed(&self, _: &NSNotification) {
            _ = self.ivars().events.send(Event::ThemeChanged);
        }

        /// Called when a key-value observed property changes for a process.
        ///
        /// # Arguments
        ///
        /// * `key_path` - The key path of the changed property.
        /// * `_object` - The object being observed (unused).
        /// * `change` - A dictionary containing details of the change.
        /// * `context` - An opaque registration token, never a Process pointer.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value_for_keypath(
            &self,
            key_path: &NSString,
            _object: &NSObject,
            change: &NSDictionary,
            context: *mut c_void,
        ) {
            let Some(observation) = self.ivars().process_observations.get(context.addr()) else {
                return;
            };

            let result = unsafe { change.objectForKey(NSKeyValueChangeNewKey) };
            let policy = result.and_then(|result| result.downcast_ref::<NSNumber>().map(NSNumber::intValue));

            let key_path = key_path.to_string();
            if key_path != observation.key_path || !process_observation_ready(&key_path, policy) {
                return;
            }
            // Initial is synchronous with addObserver. Never borrow Process,
            // mutate its policy, or unregister from inside this callback.
            _ = self.ivars().events.send(Event::ReconcileWindows {
                scope: ReconcileScope::Application(observation.pid),
            });
        }
    }

);

impl WorkspaceObserver {
    pub(crate) fn register_process_observation(
        &self,
        pid: Pid,
        key_path: &'static str,
    ) -> Option<usize> {
        self.ivars()
            .process_observations
            .insert(ProcessObservation { pid, key_path })
    }

    pub(crate) fn retire_process_observation(&self, token: usize) {
        self.ivars().process_observations.remove(token);
    }

    /// Creates a new `WorkspaceObserver` instance.
    ///
    /// # Arguments
    ///
    /// * `events` - An `EventSender` to send workspace-related events.
    ///
    /// # Returns
    ///
    /// A `Retained<Self>` containing the new `WorkspaceObserver` instance.
    pub(super) fn new(events: EventSender) -> Retained<Self> {
        // Initialize instance variables.
        let this = Self::alloc().set_ivars(Ivars {
            events,
            process_observations: CallbackRegistry::new(),
        });
        // Call `NSObject`'s `init` method.
        unsafe { msg_send![super(this), init] }
    }

    /// Starts observing workspace notifications by registering selectors with `NSWorkspace` and `NSDistributedNotificationCenter`.
    pub(super) fn start(&self) {
        let methods = [
            (
                sel!(activeSpaceDidChange:),
                "NSWorkspaceActiveSpaceDidChangeNotification",
            ),
            (
                sel!(didActivateApplication:),
                "NSWorkspaceDidActivateApplicationNotification",
            ),
            (
                sel!(didDeactivateApplication:),
                "NSWorkspaceDidDeactivateApplicationNotification",
            ),
            (
                sel!(didHideApplication:),
                "NSWorkspaceDidHideApplicationNotification",
            ),
            (
                sel!(didUnhideApplication:),
                "NSWorkspaceDidUnhideApplicationNotification",
            ),
            (sel!(didWake:), "NSWorkspaceDidWakeNotification"),
        ];
        let shared_ws = NSWorkspace::sharedWorkspace();
        let notification_center = shared_ws.notificationCenter();

        for (sel, name) in &methods {
            debug!("registering {} with {name}", *sel);
            let notification_type = NSString::from_str(name);
            unsafe {
                notification_center.addObserver_selector_name_object(
                    self,
                    *sel,
                    Some(&notification_type),
                    None,
                );
            };
        }

        let methods = [
            (
                sel!(didChangeMenuBarHiding:),
                "AppleInterfaceMenuBarHidingChangedNotification",
            ),
            (
                sel!(didChangeTheme:),
                "AppleInterfaceThemeChangedNotification",
            ),
            (sel!(didChangeDockPref:), "com.apple.dock.prefchanged"),
        ];
        let distributed_notification_center = NSDistributedNotificationCenter::defaultCenter();
        for (sel, name) in &methods {
            debug!("registering {} with {name}", *sel);
            let notification_type = NSString::from_str(name);
            unsafe {
                distributed_notification_center.addObserver_selector_name_object(
                    self,
                    *sel,
                    Some(&notification_type),
                    None,
                );
            };
        }

        let methods = [(
            sel!(didRestartDock:),
            "NSApplicationDockDidRestartNotification",
        )];
        let default_center = NSNotificationCenter::defaultCenter();
        for (sel, name) in &methods {
            debug!("registering {} with {name}", *sel);
            let notification_type = NSString::from_str(name);
            unsafe {
                default_center.addObserver_selector_name_object(
                    self,
                    *sel,
                    Some(&notification_type),
                    None,
                );
            };
        }
    }
}

impl Drop for WorkspaceObserver {
    /// Deregisters all previously registered notification callbacks when the `WorkspaceObserver` is dropped.
    fn drop(&mut self) {
        info!("deregistering callbacks.");
        unsafe {
            NSWorkspace::sharedWorkspace()
                .notificationCenter()
                .removeObserver(self);
            NSNotificationCenter::defaultCenter().removeObserver(self);
            NSDistributedNotificationCenter::defaultCenter().removeObserver(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::Process;
    use objc2_app_kit::NSApplicationActivationPolicy;

    fn deliver_policy(observer: &WorkspaceObserver, token: usize) {
        let value = NSNumber::numberWithInt(0);
        let change = NSDictionary::from_slices(
            &[unsafe { NSKeyValueChangeNewKey }],
            &[&*value as &objc2::runtime::AnyObject],
        );
        observer.observe_value_for_keypath(
            sel!(observeValueForKeyPath:ofObject:change:context:),
            &NSString::from_str("activationPolicy"),
            &NSObject::new(),
            unsafe { &*std::ptr::from_ref(&*change).cast::<NSDictionary>() },
            token as *mut c_void,
        );
    }

    #[test]
    fn kvo_initial_callback_does_not_mutate_the_borrowed_process() {
        let (events, receiver) = EventSender::new();
        let observer = WorkspaceObserver::new(events);
        let process = Box::pin(Process::for_observation_test(observer.clone()));
        let borrowed = process.as_ref().get_ref();
        deliver_policy(&observer, borrowed.observation_context_for_test().addr());
        assert_eq!(borrowed.policy, NSApplicationActivationPolicy::Prohibited);
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::ReconcileWindows {
                scope: ReconcileScope::Application(123),
            })
        ));
    }

    #[test]
    fn kvo_registration_survives_initial_delivery_and_retires_before_native_removal() {
        let (events, receiver) = EventSender::new();
        let observer = WorkspaceObserver::new(events);
        let mut process = Process::for_observation_test(observer.clone());
        process.unobserve_activation_policy();
        let mut registrations = Vec::new();
        for _ in 0..3 {
            process.observe_with("activationPolicy", |token| {
                registrations.push(token);
                deliver_policy(&observer, token);
            });
        }
        assert_eq!(
            registrations.len(),
            1,
            "Initial must not clear the registered marker"
        );
        assert_eq!(receiver.try_iter().count(), 1);
        assert_eq!(process.policy, NSApplicationActivationPolicy::Prohibited);

        let token = registrations[0];
        let mut removals = 0;
        for _ in 0..3 {
            process.unobserve_with("activationPolicy", |removed| {
                removals += 1;
                assert_eq!(removed, token);
                assert!(observer.ivars().process_observations.get(token).is_none());
                deliver_policy(&observer, token);
            });
        }
        assert_eq!(removals, 1);
        assert!(receiver.try_recv().is_err());

        process.observe_with("activationPolicy", |next| {
            assert_ne!(next, token);
            deliver_policy(&observer, token);
        });
        let next = process.observation_context_for_test().addr();
        assert!(receiver.try_recv().is_err());
        drop(process);
        assert!(observer.ivars().process_observations.get(next).is_none());
        deliver_policy(&observer, next);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn kvo_missing_or_unready_values_do_not_report_readiness() {
        for key in ["activationPolicy", "finishedLaunching", "unknown"] {
            assert!(!process_observation_ready(key, None));
            assert!(!process_observation_ready(key, Some(2)));
        }
        assert!(!process_observation_ready("finishedLaunching", Some(0)));
        assert!(process_observation_ready("finishedLaunching", Some(1)));
        assert!(process_observation_ready("activationPolicy", Some(0)));
        assert!(process_observation_ready("activationPolicy", Some(1)));
    }
}

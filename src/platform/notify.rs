use num_enum::{IntoPrimitive, TryFromPrimitive};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use stdext::function_name;
use tracing::{Level, debug, error, instrument};

use crate::errors::{Error, Result};
use crate::events::{DestroySource, Event, EventSender};
use crate::platform::{ConnID, OSStatus, WinID, WorkspaceId, macos_major_version};
use crate::util::MacResult;

unsafe extern "C" {
    pub fn SLSMainConnectionID() -> ConnID;
    pub fn SLSSpaceGetType(cid: ConnID, sid: WorkspaceId) -> libc::c_int;
    pub fn SLSRegisterConnectionNotifyProc(
        cid: ConnID,
        callback: extern "C-unwind" fn(u32, *mut c_void, usize, *mut c_void, ConnID),
        event: u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn SLSRemoveConnectionNotifyProc(
        cid: ConnID,
        callback: extern "C-unwind" fn(u32, *mut c_void, usize, *mut c_void, ConnID),
        event: u32,
        data: *mut c_void,
    ) -> OSStatus;
}

trait NotifyApi {
    fn register(&self, cid: ConnID, event: u32, context: *mut c_void) -> Result<()>;
    fn unregister(&self, cid: ConnID, event: u32, context: *mut c_void) -> Result<()>;
}

struct SystemNotifyApi;

impl NotifyApi for SystemNotifyApi {
    fn register(&self, cid: ConnID, event: u32, context: *mut c_void) -> Result<()> {
        unsafe { SLSRegisterConnectionNotifyProc(cid, NotifyHandler::callback, event, context) }
            .to_result(function_name!())
    }

    fn unregister(&self, cid: ConnID, event: u32, context: *mut c_void) -> Result<()> {
        unsafe { SLSRemoveConnectionNotifyProc(cid, NotifyHandler::callback, event, context) }
            .to_result(function_name!())
    }
}

static NEXT_CALLBACK_TOKEN: AtomicUsize = AtomicUsize::new(1);

/// Opaque refcons are never dereferenced or reused. A callback takes its own
/// lease before teardown removes the lookup, independent of native queue drain.
#[derive(Debug)]
pub(crate) struct CallbackRegistry<T> {
    contexts: Mutex<HashMap<usize, Arc<T>>>,
}

impl<T> CallbackRegistry<T> {
    pub(crate) fn new() -> Self {
        Self {
            contexts: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn insert(&self, context: T) -> Option<usize> {
        let token = NEXT_CALLBACK_TOKEN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()?;
        self.contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, Arc::new(context));
        Some(token)
    }

    pub(crate) fn get(&self, token: usize) -> Option<Arc<T>> {
        self.contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .cloned()
    }

    pub(crate) fn remove(&self, token: usize) {
        self.contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&token);
    }
}

static NOTIFY_CONTEXTS: LazyLock<CallbackRegistry<NotifyHandler>> =
    LazyLock::new(CallbackRegistry::new);

pub(super) struct NotifyHandler {
    events: EventSender,
    conn: ConnID,
}

pub(super) type PinnedNotifyHandler = NotifyRegistration;

pub(super) struct NotifyRegistration {
    conn: ConnID,
    token: usize,
    registered: Vec<u32>,
    api: Box<dyn NotifyApi>,
}

impl Drop for NotifyRegistration {
    fn drop(&mut self) {
        // Close delivery first. Even failed unregisters can only carry an inert
        // token afterwards; callbacks already holding an Arc remain memory-safe.
        NOTIFY_CONTEXTS.remove(self.token);
        for event in self.registered.drain(..).rev() {
            if let Err(error) = self
                .api
                .unregister(self.conn, event, self.token as *mut c_void)
            {
                error!(event, %error, "unable to unregister WindowServer notification");
            }
        }
    }
}

impl NotifyHandler {
    pub(super) fn new(events: EventSender) -> Self {
        Self {
            events,
            conn: unsafe { SLSMainConnectionID() },
        }
    }

    pub(super) fn start(self) -> Result<PinnedNotifyHandler> {
        let mut events = vec![
            KnownCGSEvent::SpaceCreated,
            KnownCGSEvent::SpaceCurrentChanged,
            KnownCGSEvent::SpaceDestroyed,
            KnownCGSEvent::SpaceWindowDestroyed,
        ];
        if macos_major_version() >= 15 {
            events.push(KnownCGSEvent::WindowClosed);
        }
        self.start_with_api(Box::new(SystemNotifyApi), &events)
    }

    fn start_with_api(
        self,
        api: Box<dyn NotifyApi>,
        events: &[KnownCGSEvent],
    ) -> Result<PinnedNotifyHandler> {
        debug!("Registering notify handler");
        let cid = self.conn;
        let token = NOTIFY_CONTEXTS
            .insert(self)
            .ok_or_else(|| Error::InvalidInput("callback token space exhausted".to_owned()))?;
        let mut registration = NotifyRegistration {
            conn: cid,
            token,
            registered: Vec::new(),
            api,
        };
        for &event in events {
            let event = event.into();
            if registration.registered.contains(&event) {
                continue;
            }
            registration
                .api
                .register(cid, event, token as *mut c_void)?;
            registration.registered.push(event);
        }
        Ok(registration)
    }

    extern "C-unwind" fn callback(
        event_id: u32,
        data: *mut c_void,
        len: usize,
        context: *mut c_void,
        cid: ConnID,
    ) {
        if let Some(this) = NOTIFY_CONTEXTS.get(context.addr()) {
            this.notify_handler(event_id, data, len, cid);
        }
    }

    #[instrument(level = Level::DEBUG, skip_all, fields(event_id, len))]
    fn notify_handler(&self, event_id: u32, data: *mut c_void, len: usize, _cid: ConnID) {
        let CGSEventType::Known(event) = event_id.into() else {
            debug!("Unknown event received: {event_id}");
            return;
        };

        match event {
            KnownCGSEvent::SpaceCreated
            | KnownCGSEvent::SpaceDestroyed
            | KnownCGSEvent::SpaceCurrentChanged => {
                if let Some(space_id) = from_bytes::<WorkspaceId>(data, len) {
                    if matches!(event, KnownCGSEvent::SpaceDestroyed) {
                        _ = self.events.send(Event::SpaceDestroyed { space_id });
                    } else {
                        let space_type = unsafe { SLSSpaceGetType(self.conn, space_id) };
                        if space_type == 0 && matches!(event, KnownCGSEvent::SpaceCreated) {
                            _ = self.events.send(Event::SpaceCreated { space_id });
                        } else {
                            debug!("{event} space = {space_id}, space_type = {space_type}");
                        }
                    }
                }
            }

            KnownCGSEvent::SpaceWindowDestroyed => {
                let offset = std::mem::size_of::<u64>();
                if let Some(space) = from_bytes::<WorkspaceId>(data, len)
                    && let Some(window_id) = from_bytes::<WinID>(
                        unsafe { data.byte_add(offset) },
                        len.saturating_sub(offset),
                    )
                {
                    debug!("{event} space = {space}, window_id = {window_id}");
                    _ = self.events.send(Event::WindowDestroyed {
                        window_id,
                        source: DestroySource::SpaceNotification,
                        incarnation: None,
                    });
                }
            }

            KnownCGSEvent::WindowClosed => {
                if let Some(window_id) = from_bytes::<WinID>(data, len) {
                    debug!("{event} window_id = {window_id}");
                    _ = self.events.send(Event::WindowDestroyed {
                        window_id,
                        source: DestroySource::WindowServer,
                        incarnation: None,
                    });
                }
            }

            KnownCGSEvent::SpaceWindowCreated
            | KnownCGSEvent::WindowMoved
            | KnownCGSEvent::WindowResized
            | KnownCGSEvent::WindowReordered
            | KnownCGSEvent::WindowLevelChanged
            | KnownCGSEvent::WindowUnhidden
            | KnownCGSEvent::WindowHidden
            | KnownCGSEvent::WindowManagerActivatingClickOrdering
            | KnownCGSEvent::WindowOrderingGroupChanged
            | KnownCGSEvent::WindowParentChanged => {
                let window_id = from_bytes::<WinID>(data, len);
                debug!("{event} window_id = {window_id:?}");
            }

            _ => {
                let bytes = (!data.is_null() && len > 0)
                    .then(|| unsafe { std::slice::from_raw_parts(data as *const u8, len) });
                debug!("Unhandled event {event}: {bytes:?}");
            }
        }
    }
}

fn from_bytes<T: Copy>(data: *const c_void, len: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    if data.is_null() || len < size {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(data.cast::<T>()) })
}

// credits
// https://github.com/asmagill/hs._asm.undocumented.spaces/blob/master/CGSSpace.h.
// https://github.com/koekeishiya/yabai/blob/d55a647913ab72d8d8b348bee2d3e59e52ce4a5d/src/misc/extern.h.
// https://github.com/acsandmann/rift
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
enum KnownCGSEvent {
    DisplayWillSleep = 102,
    DisplayDidWake = 103,
    WindowUpdated = 723,
    // maybe loginwindow active? kCGSEventNotificationSystemDefined = 724,
    WindowClosed = 804,
    WindowMoved = 806,
    WindowResized = 807,
    WindowReordered = 808,
    WindowLevelChanged = 811,
    WindowUnhidden = 815,
    WindowHidden = 816,
    MissionControlEntered = 1204,
    /// Named in `_WSLogStringForNotifyType`; observed when the active display /
    /// status-bar space changes, including current-space and capability updates.
    PackagesStatusBarSpaceChanged = 1308,
    WindowTitleChanged = 1322,
    SpaceWindowCreated = 1325,
    SpaceWindowDestroyed = 1326,
    SpaceCreated = 1327,
    SpaceDestroyed = 1328,
    /// Posted by `managed_display_set_current_space` through
    /// `post_space_lifecycle_notification`; likely carries the new current
    /// space id for a display transition.
    SpaceCurrentChanged = 1329,
    /// Local WM notification posted during activating-click ordering; payload is
    /// believed to be window/order metadata, but the exact layout is still
    /// under investigation.
    WindowManagerActivatingClickOrdering = 1333,
    /// Local notification posted when the front connection for the current
    /// space changes.
    WindowManagerSpaceFrontConnectionChanged = 1334,
    /// Local notification posted when the global front connection changes.
    WindowManagerGlobalFrontConnectionChanged = 1335,
    /// Posted from `finish_order_windows`; observed payload is 3 x u32.
    WindowOrderingGroupChanged = 1336,
    /// Posted by `-[PKGSpaceWindowManager_commitTransaction]`; useful as a
    /// transaction boundary even when per-window membership notifications race.
    SpaceWindowTransactionCommitted = 1338,
    /// Posted from `finishBatchReassociateWindows`; observed payload starts with
    /// a u64 key/space followed by a u32 count and repeated window ids.
    SpaceWindowBatchReassociated = 1339,
    /// Posted via `__XSetSpaceWindowManagementCapabilities`; likely tied to
    /// space/window-management mode changes for a display or space.
    SpaceWindowManagementCapabilitiesChanged = 1340,
    /// Posted from `_WSWindowSetParent` and related reassociation paths.
    WindowParentChanged = 1341,
    /// Local notification from `managed_space_update_membership`; likely marks
    /// a completed space-membership mutation and may carry space/window ids.
    ManagedSpaceMembershipUpdated = 1342,
    WorkspaceWillChange = 1400,
    WorkspaceDidChange = 1401,
    WorkspaceWindowIsViewable = 1402,
    WorkspaceWindowIsNotViewable = 1403,
    WorkspaceWindowDidMove = 1404,
    WorkspacePrefsDidChange = 1405,
    WorkspacesWindowDragDidStart = 1411,
    WorkspacesWindowDragDidEnd = 1412,
    WorkspacesWindowDragWillEnd = 1413,
    WorkspacesShowSpaceForProcess = 1414,
    WorkspacesWindowDidOrderInOnNonCurrentManagedSpacesOnly = 1415,
    WorkspacesWindowDidOrderOutOnNonCurrentManagedSpaces = 1416,
    FrontmostApplicationChanged = 1508,
    TransitionDidFinish = 1700,
    All = 0xFFFF_FFFF,
}

#[derive(Debug, Clone, Copy, Hash)]
enum CGSEventType {
    Known(KnownCGSEvent),
    Unknown(u32),
}

impl From<u32> for CGSEventType {
    fn from(v: u32) -> Self {
        match KnownCGSEvent::try_from(v) {
            Ok(k) => Self::Known(k),
            Err(_) => Self::Unknown(v),
        }
    }
}
impl From<CGSEventType> for u32 {
    fn from(k: CGSEventType) -> u32 {
        match k {
            CGSEventType::Known(k) => k as u32,
            CGSEventType::Unknown(v) => v,
        }
    }
}

impl std::fmt::Display for KnownCGSEvent {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::fmt::Display for CGSEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CGSEventType::Known(k) => write!(f, "{k}"),
            CGSEventType::Unknown(v) => write!(f, "Unknown({v})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{KnownCGSEvent, NOTIFY_CONTEXTS, NotifyApi, NotifyHandler, from_bytes};
    use crate::errors::{Error, Result};
    use crate::events::{DestroySource, Event, EventSender};
    use crate::platform::ConnID;

    #[derive(Default)]
    struct RegistrationLog {
        registered: Vec<u32>,
        removed: Vec<u32>,
        context: usize,
        live_during_unregister: bool,
    }

    struct FakeNotifyApi {
        log: Rc<RefCell<RegistrationLog>>,
        fail: Option<u32>,
        fail_unregister: bool,
    }

    impl NotifyApi for FakeNotifyApi {
        fn register(&self, _: ConnID, event: u32, context: *mut std::ffi::c_void) -> Result<()> {
            if self.fail == Some(event) {
                return Err(Error::macos("injected registration failure", -1));
            }
            let mut log = self.log.borrow_mut();
            log.registered.push(event);
            log.context = context.addr();
            Ok(())
        }

        fn unregister(&self, _: ConnID, event: u32, context: *mut std::ffi::c_void) -> Result<()> {
            let mut log = self.log.borrow_mut();
            log.removed.push(event);
            log.live_during_unregister |= NOTIFY_CONTEXTS.get(context.addr()).is_some();
            if self.fail_unregister {
                return Err(Error::macos("injected unregister failure", -1));
            }
            Ok(())
        }
    }

    fn notification_fixture() -> NotifyHandler {
        let (events, _receiver) = EventSender::new();
        NotifyHandler { events, conn: 0 }
    }

    #[test]
    fn notification_registration_failure_rolls_back_successful_registrations() {
        let log = Rc::new(RefCell::new(RegistrationLog::default()));
        let result = notification_fixture().start_with_api(
            Box::new(FakeNotifyApi {
                log: log.clone(),
                fail: Some(KnownCGSEvent::SpaceDestroyed as u32),
                fail_unregister: false,
            }),
            &[
                KnownCGSEvent::SpaceCreated,
                KnownCGSEvent::SpaceCurrentChanged,
                KnownCGSEvent::SpaceDestroyed,
            ],
        );
        assert!(result.is_err());
        assert_eq!(log.borrow().removed, vec![1329, 1327]);
        assert!(!log.borrow().live_during_unregister);
    }

    #[test]
    fn notification_registration_drop_unregisters_every_success_once() {
        let log = Rc::new(RefCell::new(RegistrationLog::default()));
        let registration = notification_fixture()
            .start_with_api(
                Box::new(FakeNotifyApi {
                    log: log.clone(),
                    fail: None,
                    fail_unregister: false,
                }),
                &[
                    KnownCGSEvent::SpaceCreated,
                    KnownCGSEvent::WindowClosed,
                    KnownCGSEvent::SpaceCreated,
                ],
            )
            .unwrap();
        drop(registration);
        assert_eq!(
            log.borrow().removed,
            vec![KnownCGSEvent::WindowClosed as u32, 1327]
        );
        assert!(!log.borrow().live_during_unregister);
        assert_eq!(log.borrow().registered, vec![1327, 804]);
    }

    #[test]
    fn notification_retirement_ignores_late_tokens_and_keeps_in_flight_context_alive() {
        let (events, receiver) = EventSender::new();
        let log = Rc::new(RefCell::new(RegistrationLog::default()));
        let registration = NotifyHandler { events, conn: 0 }
            .start_with_api(
                Box::new(FakeNotifyApi {
                    log: log.clone(),
                    fail: None,
                    fail_unregister: false,
                }),
                &[KnownCGSEvent::WindowClosed],
            )
            .unwrap();
        let token = log.borrow().context;
        let in_flight = NOTIFY_CONTEXTS.get(token).unwrap();
        let weak = std::sync::Arc::downgrade(&in_flight);
        drop(registration);
        assert!(NOTIFY_CONTEXTS.get(token).is_none());
        assert!(weak.upgrade().is_some());
        // A retired callback must not even inspect this intentionally invalid payload.
        NotifyHandler::callback(804, std::ptr::null_mut(), usize::MAX, token as *mut _, 0);
        assert!(receiver.try_recv().is_err());
        drop(in_flight);
        assert!(weak.upgrade().is_none());

        let next = notification_fixture()
            .start_with_api(
                Box::new(FakeNotifyApi {
                    log: log.clone(),
                    fail: None,
                    fail_unregister: false,
                }),
                &[KnownCGSEvent::WindowClosed],
            )
            .unwrap();
        assert_ne!(log.borrow().context, token);
        drop(next);
    }

    #[test]
    fn notification_unregister_failure_still_retires_context_and_attempts_every_removal() {
        let log = Rc::new(RefCell::new(RegistrationLog::default()));
        let registration = notification_fixture()
            .start_with_api(
                Box::new(FakeNotifyApi {
                    log: log.clone(),
                    fail: None,
                    fail_unregister: true,
                }),
                &[KnownCGSEvent::SpaceCreated, KnownCGSEvent::WindowClosed],
            )
            .unwrap();
        let token = log.borrow().context;
        drop(registration);
        assert_eq!(log.borrow().removed, vec![804, 1327]);
        assert!(NOTIFY_CONTEXTS.get(token).is_none());
        NotifyHandler::callback(804, std::ptr::null_mut(), usize::MAX, token as *mut _, 0);
    }

    #[test]
    fn short_notification_payload_never_reads_beyond_its_length() {
        const PROBE: &str = "SPOOL_SHORT_NOTIFICATION_PROBE";
        if let Ok(case) = std::env::var(PROBE) {
            if case == "null" {
                assert_eq!(from_bytes::<u64>(std::ptr::null(), 0), None);
                return;
            }
            let len: usize = case.parse().unwrap();
            unsafe {
                let page = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap();
                let allocation = libc::mmap(
                    std::ptr::null_mut(),
                    2 * page,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                );
                assert_ne!(allocation, libc::MAP_FAILED);
                assert_eq!(
                    libc::mprotect(allocation.byte_add(page), page, libc::PROT_NONE),
                    0
                );
                // Only `len` readable bytes remain before the guard page.
                let payload = allocation.byte_add(page - len);
                assert_eq!(from_bytes::<u64>(payload, len), None);
                assert_eq!(libc::munmap(allocation, 2 * page), 0);
            }
            return;
        }

        // Isolate an invalid read so a regression fails this test, not the suite.
        for case in ["1", "7", "0", "2", "3", "4", "5", "6", "null"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform::notify::tests::short_notification_payload_never_reads_beyond_its_length",
                    "--test-threads=1",
                ])
                .env(PROBE, case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "payload {case}: {}; stdout: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }

    #[test]
    fn window_server_close_notification_reports_definitive_destruction() {
        let (events, receiver) = EventSender::new();
        let handler = NotifyHandler { events, conn: 0 };
        let window_id = 42;

        handler.notify_handler(
            KnownCGSEvent::WindowClosed as u32,
            std::ptr::from_ref(&window_id).cast_mut().cast(),
            std::mem::size_of_val(&window_id),
            0,
        );

        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::WindowDestroyed {
                window_id: 42,
                source: DestroySource::WindowServer,
                incarnation: None,
            })
        ));
    }
}

use accessibility_sys::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElementCreateSystemWide,
    AXUIElementSetMessagingTimeout, kAXTrustedCheckOptionPrompt,
};
use bevy::ecs::resource::Resource;
use bevy::math::{IRect, IVec2};
use core::ptr::NonNull;
use derive_more::{DerefMut, with_trait::Deref};
use mockall::automock;
#[cfg(feature = "lua")]
use notify::{RecursiveMode, Watcher};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType, CGPoint, CGRect,
    CGSize, kCFBooleanTrue,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayBounds, CGEvent,
    CGEventField, CGEventFlags, CGEventTapLocation, CGGetActiveDisplayList,
    CGWarpMouseCursorPosition, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    kCGWindowAlpha, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "lua")]
use std::path::Path;
use std::ptr::null_mut;
#[cfg(feature = "lua")]
use std::time::Duration;
use stdext::function_name;
use tracing::{Level, debug, instrument, trace, warn};

use crate::config::Config;
use crate::errors::{Error, Result};
use crate::events::{Event, EventSender};
use crate::manager::skylight::SLSSetWindowListBrightness;
use crate::platform::{ConnID, Pid, ProcessSerialNumber, WinID, WorkspaceId};
#[cfg(feature = "lua")]
use crate::util::symlink_target;
use crate::util::{AXUIWrapper, MacResult, create_array, round_px};
use app::ApplicationOS;
pub use app::{Application, ApplicationApi};
pub use display::{Display, DisplayObservation};
pub use process::{Process, ProcessApi};
pub use skylight::AXUIElementCopyAttributeValue;
use skylight::{
    SLSCopyActiveMenuBarDisplayIdentifier, SLSCopyAssociatedWindows, SLSCopyManagedDisplaySpaces,
    SLSCopyWindowsWithOptionsAndTags, SLSFindWindowAndOwner, SLSGetConnectionIDForPSN,
    SLSGetCurrentCursorLocation, SLSGetDisplayMenubarHeight, SLSGetSpaceManagementMode,
    SLSMainConnectionID, SLSManagedDisplayGetCurrentSpace, SLSMoveWindowsToManagedSpace,
    SLSRequestNotificationsForWindows, SLSSpaceGetType, SLSWindowIteratorAdvance,
    SLSWindowIteratorGetAttributes, SLSWindowIteratorGetParentID, SLSWindowIteratorGetTags,
    SLSWindowIteratorGetWindowID, SLSWindowQueryResultCopyWindows, SLSWindowQueryWindows,
};
pub use windows::{Window, WindowApi, WindowOS, WindowPadding, ax_window_id};

#[cfg(test)]
pub use process::MockProcessApi;
#[cfg(test)]
pub use windows::MockWindowApi;

pub(crate) mod app;
pub(crate) mod discovery;
mod display;
pub(crate) mod inspection;
mod process;
mod skylight;
mod windows;

pub type Origin = IVec2;
pub type Size = IVec2;

/// Assigns one Spool-owned `AppKit` window to exactly one native Space.
///
/// This private `SkyLight` operation is permitted for windows owned by the
/// calling process and does not require Dock injection or disabling SIP.
pub(crate) fn move_owned_window_to_space(window_id: WinID, space_id: WorkspaceId) -> Result<()> {
    let windows = create_array(&[window_id], CFNumberType::SInt32Type)?;
    unsafe {
        SLSMoveWindowsToManagedSpace(SLSMainConnectionID(), &raw const *windows, space_id);
    }
    Ok(())
}

/// Exact forward membership; a void move call is never completion evidence.
pub(crate) fn owned_window_is_in_space(window_id: WinID, space_id: WorkspaceId) -> Result<bool> {
    let id = u64::try_from(window_id)?;
    let spaces = inspection::window_spaces(id).map_err(Error::Generic)?;
    Ok(!spaces.is_empty() && spaces.iter().all(|space| *space == space_id))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent runtime capabilities"
)]
pub struct NativeSpaceCapabilities {
    pub move_windows: bool,
    pub focus: bool,
    pub create: bool,
    pub delete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSpaceIntent {
    MoveWindows {
        window_ids: Vec<WinID>,
        space_id: WorkspaceId,
    },
    Focus {
        space_id: WorkspaceId,
        animate: bool,
    },
    Create {
        display_id: CGDirectDisplayID,
    },
    Delete {
        space_id: WorkspaceId,
    },
}

fn bridged_window_move_class() -> Option<&'static AnyClass> {
    let class = AnyClass::get(c"SLSBridgedMoveWindowsToManagedSpaceOperation")?;
    let has_initializer: bool =
        unsafe { msg_send![class, instancesRespondToSelector: sel!(initWithWindows:spaceID:)] };
    let has_executor: bool =
        unsafe { msg_send![class, instancesRespondToSelector: sel!(performWithWMBridgeDelegate)] };
    (has_initializer && has_executor).then_some(class)
}

fn create_bridged_window_move_operation(
    class: &AnyClass,
    windows: &CFArray,
    space_id: WorkspaceId,
) -> Result<*mut AnyObject> {
    // CFArray and NSArray are toll-free bridged. The Objective-C initializer
    // expects an object (`id`), matching yabai's `(__bridge id)window_list_ref`.
    let windows_object: &AnyObject = unsafe { &*std::ptr::from_ref(windows).cast() };
    let operation = unsafe {
        let allocated: *mut AnyObject = msg_send![class, alloc];
        let operation: *mut AnyObject = msg_send![allocated,
            initWithWindows: windows_object,
            spaceID: space_id
        ];
        operation
    };
    if operation.is_null() {
        Err(Error::Generic(
            "native window-to-Space operation initialization failed".to_string(),
        ))
    } else {
        Ok(operation)
    }
}

fn native_space_gesture_delta(
    spaces: &[WorkspaceId],
    current: WorkspaceId,
    target: WorkspaceId,
) -> Result<isize> {
    let current = spaces
        .iter()
        .position(|space_id| *space_id == current)
        .ok_or_else(|| Error::NotFound("current Space is not in its display".to_string()))?;
    let target = spaces
        .iter()
        .position(|space_id| *space_id == target)
        .ok_or_else(|| Error::NotFound("target Space is not in its display".to_string()))?;
    Ok(target.cast_signed() - current.cast_signed())
}

/// The Space list of the display that has to carry the target Space.
///
/// Only the active display's own Space read can confirm that the target Space
/// lives there. A read that did not happen is reported as a read that did not
/// happen rather than as "the target Space is not on the active display": the
/// two call for different responses, and only the second is evidence about the
/// target.
fn active_display_spaces(
    displays: &[DisplayObservation],
    active_display_id: CGDirectDisplayID,
    target_space_id: WorkspaceId,
) -> Result<Vec<WorkspaceId>> {
    let spaces = displays
        .iter()
        .find(|observation| observation.display.id() == active_display_id)
        .and_then(|observation| observation.spaces.as_ref().ok())
        .ok_or_else(|| {
            Error::Generic("the active display's Space list could not be read".to_string())
        })?;
    if !spaces.contains(&target_space_id) {
        return Err(Error::InvalidInput(
            "target Space is not on the active display".to_string(),
        ));
    }
    Ok(spaces.clone())
}

fn post_native_space_gesture(delta: isize) -> Result<()> {
    // Private CGEvent fields used by yabai's SIP-on gesture fallback.
    const EVENT_TYPE: CGEventField = CGEventField(55);
    const GESTURE_HID_TYPE: CGEventField = CGEventField(110);
    const SWIPE_MOTION: CGEventField = CGEventField(123);
    const SWIPE_PROGRESS: CGEventField = CGEventField(124);
    const SWIPE_VELOCITY_X: CGEventField = CGEventField(129);
    const GESTURE_PHASE: CGEventField = CGEventField(132);

    if delta == 0 {
        return Ok(());
    }

    let event = CGEvent::new(None)
        .ok_or_else(|| Error::Generic("unable to create Space gesture".to_string()))?;
    let event = event.as_ref();
    let sign = if delta > 0 { 1.0 } else { -1.0 };
    CGEvent::set_integer_value_field(Some(event), EVENT_TYPE, 30);
    CGEvent::set_integer_value_field(Some(event), GESTURE_HID_TYPE, 23);
    CGEvent::set_integer_value_field(Some(event), SWIPE_MOTION, 1);
    CGEvent::set_double_value_field(Some(event), SWIPE_PROGRESS, sign);
    CGEvent::set_double_value_field(Some(event), SWIPE_VELOCITY_X, sign * 9999.0);

    for _ in 0..delta.unsigned_abs() {
        CGEvent::set_integer_value_field(Some(event), GESTURE_PHASE, 1);
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
        CGEvent::set_integer_value_field(Some(event), GESTURE_PHASE, 4);
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
    }
    Ok(())
}

fn post_animated_space_shortcut(delta: isize) -> Result<()> {
    const LEFT_ARROW_KEYCODE: u16 = 0x7b;
    const RIGHT_ARROW_KEYCODE: u16 = 0x7c;

    if delta == 0 {
        return Ok(());
    }

    let keycode = if delta > 0 {
        RIGHT_ARROW_KEYCODE
    } else {
        LEFT_ARROW_KEYCODE
    };
    let flags = CGEventFlags::MaskControl | CGEventFlags::MaskSecondaryFn;
    for _ in 0..delta.unsigned_abs() {
        for key_down in [true, false] {
            let event = CGEvent::new_keyboard_event(None, keycode, key_down).ok_or_else(|| {
                Error::Generic("unable to create animated Space shortcut".to_string())
            })?;
            CGEvent::set_flags(Some(&event), flags);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
    Ok(())
}

pub fn origin_from(point: CGPoint) -> Origin {
    Origin::new(round_px(point.x), round_px(point.y))
}

pub fn origin_to(point: Origin) -> CGPoint {
    CGPoint::new(point.x.into(), point.y.into())
}

pub fn size_from(size: CGSize) -> Size {
    Size::new(round_px(size.width), round_px(size.height))
}

pub fn irect_from(rect: CGRect) -> IRect {
    let mid = rect.mid();
    IRect::from_center_size(origin_from(mid), size_from(rect.size))
}

/// Defines the interface for a window manager, abstracting OS-specific operations.
#[automock]
pub trait WindowManagerApi: Send + Sync {
    /// Submits a system overview request. Completion is observed through Dock events.
    fn perform_system_overview(
        &self,
        overview: crate::platform::mission_control::SystemOverview,
    ) -> Result<()>;
    /// Capabilities available without Dock injection or disabling SIP.
    fn native_space_capabilities(&self) -> NativeSpaceCapabilities;
    /// Submits a Space operation. Success means accepted by macOS, not
    /// yet reconciled; callers must wait for events and verify OS membership.
    fn perform_native_space_intent(&self, intent: &NativeSpaceIntent) -> Result<()>;
    /// Creates a new `Application` instance from a given `ProcessApi`.
    ///
    /// # Arguments
    ///
    /// * `process` - A reference to the `ProcessApi` trait object representing the application's process.
    ///
    /// # Returns
    ///
    /// `Ok(Application)` if the application is successfully created, otherwise `Err(Error)`.
    fn new_application(&self, process: &dyn ProcessApi) -> Result<Application>;
    /// Retrieves a list of window IDs associated with a parent window.
    ///
    /// # Arguments
    ///
    /// * `window_id` - The `WinID` of the parent window.
    ///
    /// # Returns
    ///
    /// A `Vec<WinID>` containing the IDs of associated child windows.
    ///
    /// # Contract
    ///
    /// There is no error channel. The platform call returns `NULL` both for a
    /// window with no associated windows and on a failed read, and does not
    /// separate the two, so both arrive as an empty list. A caller must not
    /// read an empty result as proof that the window has no children.
    fn get_associated_windows(&self, window_id: WinID) -> Vec<WinID>;
    /// Retrieves every physical display together with its own Space read.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<DisplayObservation>)` for all present displays, otherwise
    /// `Err(Error)` when the physical inventory itself could not be read.
    ///
    /// # Contract
    ///
    /// A failed physical inventory is not an empty inventory, and a failed
    /// per-display Space read retains its physical display in the successful
    /// observation. Callers must distinguish the outer `Err` (no inventory)
    /// from an `Ok` list that is empty or whose entries carry `Err` Spaces;
    /// collapsing either into "no displays" loses evidence the native sources
    /// actually provided.
    fn observe_displays(&self) -> Result<Vec<DisplayObservation>>;
    /// Retrieves the `CGDirectDisplayID` of the active menu bar display.
    ///
    /// # Returns
    ///
    /// `Ok(u32)` with the display ID if successful, otherwise `Err(Error)`.
    fn active_display_id(&self) -> Result<u32>;
    /// Retrieves the ID of the current active space on a given display.
    ///
    /// # Arguments
    ///
    /// * `display_id` - The `CGDirectDisplayID` of the display.
    ///
    /// # Returns
    ///
    /// `Ok(u64)` with the space ID if successful, otherwise `Err(Error)`.
    ///
    /// # Contract
    ///
    /// `0` is the platform's "no answer" sentinel rather than a Space id: a
    /// probe against a live session saw it returned for a display UUID that
    /// names no display. It is reported as an error, so a read that did not
    /// happen cannot arrive as a Space that names nothing.
    fn active_display_space(&self, display_id: CGDirectDisplayID) -> Result<WorkspaceId>;
    /// Returns `true` when `space_id` is a native macOS fullscreen Space.
    fn workspace_is_fullscreen(&self, space_id: WorkspaceId) -> bool;
    /// Centers the mouse cursor on a given window within its display bounds if it's not already within the window.
    ///
    /// # Arguments
    ///
    /// * `window` - A reference to the `Window` to center the mouse on.
    /// * `display_bounds` - The `CGRect` representing the bounds of the display the window is on.
    fn warp_mouse(&self, origin: Origin);
    /// Adds existing windows for a given application, potentially resolving unresolved windows.
    ///
    /// # Arguments
    ///
    /// * `app` - A mutable reference to the `Application` whose windows are to be added.
    /// * `spaces` - A slice of space IDs to query for windows.
    /// * `config` - The current Spool configuration, used to evaluate window rules.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<Window>)` containing the found and added windows, otherwise `Err(Error)`.
    ///
    /// # Contract
    ///
    /// The `Err` covers the application's own `AX` window list only. The
    /// supplementary `WindowServer` inventory is best effort: when it cannot be
    /// read the call still succeeds with an empty second element, logged at
    /// debug, so an unreadable inventory is indistinguishable from one with no
    /// off-screen windows.
    fn find_existing_application_windows(
        &self,
        app: &mut Application,
        spaces: &[WorkspaceId],
        config: &Config,
    ) -> Result<(Vec<Window>, Vec<WinID>)>;
    /// Finds the `WinID` of a window at a given screen point.
    ///
    /// # Arguments
    ///
    /// * `point` - A reference to the `CGPoint` representing the screen coordinate.
    ///
    /// # Returns
    ///
    /// `Ok(WinID)` with the found window's ID if successful, otherwise `Err(Error)`.
    fn find_window_at_point(&self, point: &CGPoint) -> Result<WinID>;
    /// Returns a list of `WinID`s for all windows in a given workspace (space).
    ///
    /// # Arguments
    ///
    /// * `space_id` - The ID of the space to query.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<WinID>)` containing the list of window IDs, otherwise `Err(Error)`.
    ///
    /// # Contract
    ///
    /// A Space that holds no windows is `Ok(vec![])`, never an error. `Err` means
    /// the read could not be performed and says nothing about the Space's
    /// contents. Callers must not treat `Err` as an empty membership list, and
    /// must not treat `Ok(vec![])` as an unavailable Space.
    fn windows_in_workspace(&self, space_id: WorkspaceId) -> Result<Vec<WinID>>;

    /// Native presentation candidates, excluding ordered-out retained surfaces.
    /// This is Space-local, not an on-screen-only query; inactive Spaces remain eligible.
    ///
    /// # Contract
    ///
    /// As [`Self::windows_in_workspace`]: an empty result is `Ok(vec![])`, and
    /// `Err` carries no membership information.
    fn presentation_windows_in_workspace(&self, space_id: WorkspaceId) -> Result<Vec<WinID>>;

    /// Sends an `Event::Exit` to the event loop, signaling the application to quit.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the exit event is sent successfully, otherwise `Err(Error)`.
    fn quit(&self) -> Result<()>;

    #[cfg(feature = "lua")]
    fn setup_config_watcher(&self, path: &Path) -> Result<Box<dyn Watcher>>;

    /// Returns the current cursor position in absolute CG coordinates,
    /// or `None` if the position cannot be determined.
    fn cursor_position(&self) -> Option<CGPoint>;

    /// Sets a brightness level per window: `0.0` normal, `1.0` bright,
    /// `-1.0` dark.
    ///
    /// # Contract
    ///
    /// Best effort with no error channel. A request that the platform rejects,
    /// or one whose length cannot be represented for the platform call, is
    /// dropped with at most a debug log; callers cannot tell a dimmed window
    /// from an ignored request.
    fn dim_windows(&self, windows: &[WinID], level: f32);

    /// Returns every `WindowServer` window in the current GUI session with
    /// its owning process, including off-screen and minimized windows.
    ///
    /// `None` means the `WindowServer` list was unavailable, which is not an
    /// empty session.
    fn window_owners_in_session(&self) -> Option<HashMap<WinID, Pid>>;

    /// Current GUI session windows in `WindowServer` front-to-back order.
    ///
    /// Off-screen windows keep their relative position only loosely, so callers
    /// may compare ranks of presented windows but must not treat an invisible
    /// window as frontmost. `None` means the list is unavailable.
    fn window_order_in_session(&self) -> Option<Vec<(WinID, Pid)>>;

    /// Refreshes the per-window `WindowServer` notification subscription.
    ///
    /// # Contract
    ///
    /// `Ok(())` also covers "not applicable here": on macOS versions before the
    /// notification API exists this returns success without subscribing to
    /// anything. A caller must not read `Ok` as confirmation that it is now
    /// subscribed.
    fn request_window_notifications(&self, window_ids: &[WinID]) -> Result<()>;
}

/// `WindowManager` is a Bevy resource that holds a boxed `WindowManagerApi` trait object.
/// It allows for dynamic dispatch to the OS-specific window management implementation.
#[derive(Deref, DerefMut, Resource)]
pub struct WindowManager(pub Box<dyn WindowManagerApi>);

/// `WindowManagerOS` is the macOS-specific implementation of the `WindowManagerApi` trait.
/// It directly interacts with the macOS `SkyLight` and Accessibility APIs to manage windows.
pub struct WindowManagerOS {
    main_cid: ConnID,
    event_sender: EventSender,
}

const AX_MESSAGING_TIMEOUT_SEC: f32 = 0.25;

fn initialize_ax_timeout_with<T>(
    system_wide: impl FnOnce() -> Result<T>,
    set_timeout: impl FnOnce(&T, f32) -> i32,
) -> Result<()> {
    let element = system_wide()?;
    set_timeout(&element, AX_MESSAGING_TIMEOUT_SEC)
        .to_result("AXUIElementSetMessagingTimeout(system-wide)")
}

impl WindowManagerOS {
    /// Creates a new `WindowManagerOS` instance.
    /// It initializes the main connection ID to the macOS `SkyLight` API.
    ///
    /// # Arguments
    ///
    /// * `event_sender` - The `EventSender` to dispatch events from the window manager.
    ///
    /// # Returns
    ///
    /// A new `WindowManagerOS`, or an error before platform setup can proceed.
    pub fn new(event_sender: EventSender) -> Result<Self> {
        let _main_thread = MainThreadMarker::new().ok_or_else(|| {
            Error::Generic("window manager initialization requires the main thread".to_string())
        })?;
        // AXUIElement.h: the system-wide object sets the default for this
        // process only. An application object's timeout does not propagate to
        // its windows, parents, or remote-token elements.
        initialize_ax_timeout_with(
            || AXUIWrapper::from_retained(unsafe { AXUIElementCreateSystemWide() }),
            |element, timeout| unsafe { AXUIElementSetMessagingTimeout(element.as_ptr(), timeout) },
        )?;
        let main_cid = unsafe { SLSMainConnectionID() };
        debug!("My connection id: {main_cid}");

        Ok(Self {
            main_cid,
            event_sender,
        })
    }

    /// Retrieves a list of space IDs for a given display UUID.
    /// It queries the `SkyLight` API for managed display spaces and filters by the provided UUID.
    ///
    /// # Arguments
    ///
    /// * `uuid` - A reference to the `CFString` representing the display's UUID.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<u64>)` with the list of space IDs if successful, otherwise `Err(Error)` if the spaces cannot be retrieved or the display is not found.
    fn display_space_list(&self, uuid: &CFString) -> Result<Vec<WorkspaceId>> {
        let display_spaces = NonNull::new(unsafe { SLSCopyManagedDisplaySpaces(self.main_cid) })
            .map(|ptr| unsafe { CFRetained::from_raw(ptr) })
            .ok_or(Error::PermissionDenied(format!(
                "can not copy managed display spaces for {}.",
                self.main_cid
            )))?;
        let uuid = uuid.to_string();

        let display = display_spaces.iter().find(|display| {
            let identifier = display
                .get(&CFString::from_static_str("Display Identifier"))
                .map(|name| name.to_string());
            identifier.is_some_and(|identifier| {
                // FIXME: Sometimes the main display simply has the name 'Main'.
                identifier == "Main" || identifier == uuid
            })
        });
        let Some(display) = display else {
            return Err(Error::PermissionDenied(format!(
                "could not get any displays for {}",
                self.main_cid
            )));
        };
        debug!("found display with uuid '{uuid}'");

        let display = unsafe {
            display.cast_unchecked::<CFString, CFArray<CFDictionary<CFString, CFNumber>>>()
        };
        let Some(spaces) = display.get(&CFString::from_static_str("Spaces")) else {
            return Err(Error::PermissionDenied(format!(
                "could not get any spaces for display '{uuid}'",
            )));
        };

        let spaces = spaces
            .iter()
            .filter_map(|space| {
                space
                    .get(&CFString::from_static_str("id64"))
                    .and_then(|id| id.as_i64().and_then(|value| u64::try_from(value).ok()))
            })
            .collect::<Vec<WorkspaceId>>();
        debug!(
            "spaces [{}]",
            spaces
                .iter()
                .map(|id| format!("{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(spaces)
    }

    /// Retrieves the UUID of the active menu bar display.
    /// This typically corresponds to the display where the primary menu bar is located.
    ///
    /// # Returns
    ///
    /// `Ok(CFRetained<CFString>)` with the UUID if successful, otherwise `Err(Error)` if the active display cannot be determined.
    fn active_display_uuid(&self) -> Result<CFRetained<CFString>> {
        unsafe {
            let ptr = SLSCopyActiveMenuBarDisplayIdentifier(self.main_cid);
            let ptr = NonNull::new(ptr.cast_mut()).ok_or(Error::NotFound(format!(
                "can not find active display for connection {}.",
                self.main_cid
            )))?;
            Ok(CFRetained::from_raw(ptr))
        }
    }

    /// Returns the connection ID (`ConnID`) for a given process serial number (`PSN`).
    ///
    /// # Arguments
    ///
    /// * `psn` - The `ProcessSerialNumber` of the process.
    ///
    /// # Returns
    ///
    /// `Some(ConnID)` if the connection ID is found, otherwise `None`.
    fn connection_for_process(&self, psn: ProcessSerialNumber) -> Option<ConnID> {
        let mut connection: ConnID = 0;
        unsafe { SLSGetConnectionIDForPSN(self.main_cid, &psn, &mut connection) };
        (connection != 0).then_some(connection)
    }

    fn focus_native_space(&self, target_space_id: WorkspaceId, animate: bool) -> Result<()> {
        let active_display_id = self.active_display_id()?;
        let displays = self.observe_displays()?;
        let spaces = active_display_spaces(&displays, active_display_id, target_space_id)?;
        let current_space_id = self.active_display_space(active_display_id)?;
        let delta = native_space_gesture_delta(&spaces, current_space_id, target_space_id)?;
        if animate {
            post_animated_space_shortcut(delta)
        } else {
            post_native_space_gesture(delta)
        }
    }
}

impl WindowManagerApi for WindowManagerOS {
    fn perform_system_overview(
        &self,
        overview: crate::platform::mission_control::SystemOverview,
    ) -> Result<()> {
        overview.launch()?;
        Ok(())
    }

    fn native_space_capabilities(&self) -> NativeSpaceCapabilities {
        NativeSpaceCapabilities {
            move_windows: bridged_window_move_class().is_some(),
            focus: true,
            ..NativeSpaceCapabilities::default()
        }
    }

    fn perform_native_space_intent(&self, intent: &NativeSpaceIntent) -> Result<()> {
        if let NativeSpaceIntent::Focus { space_id, animate } = intent {
            return self.focus_native_space(*space_id, *animate);
        }
        let NativeSpaceIntent::MoveWindows {
            window_ids,
            space_id,
        } = intent
        else {
            return Err(Error::Generic(
                "Space capability unavailable without Dock automation".to_string(),
            ));
        };
        if window_ids.is_empty() {
            return Err(Error::InvalidInput("window list is empty".to_string()));
        }
        let class = bridged_window_move_class().ok_or_else(|| {
            Error::Generic("native window-to-Space capability unavailable".to_string())
        })?;
        let windows = create_array(window_ids, CFNumberType::SInt32Type)?;
        unsafe {
            let operation = create_bridged_window_move_operation(class, &windows, *space_id)?;
            let _: () = msg_send![operation, performWithWMBridgeDelegate];
            let _: () = msg_send![operation, release];
        }
        Ok(())
    }

    fn new_application(&self, process: &dyn ProcessApi) -> Result<Application> {
        let connection = self.connection_for_process(process.psn());
        ApplicationOS::new(connection, process, &self.event_sender)
            .map(|app| Application::new(Box::new(app)))
    }

    /// Returns child windows of the main window.
    ///
    /// The private API returns `NULL` both for a window with no associated
    /// windows and on a failed read, and its own documentation does not
    /// separate the two. `NULL` is therefore read as "no associated windows",
    /// which is what both callers already do with the result; the point of the
    /// guard is that the outcome is a defined empty list rather than an
    /// invalid `NonNull` handed to `CFRetain`.
    #[instrument(level = Level::TRACE, skip(self), ret)]
    fn get_associated_windows(&self, window_id: WinID) -> Vec<WinID> {
        trace!("for window {window_id}");
        let Some(ptr) = NonNull::new(unsafe { SLSCopyAssociatedWindows(self.main_cid, window_id) })
        else {
            return Vec::new();
        };
        let windows = unsafe { CFRetained::retain(ptr) };
        windows.into_iter().filter_map(|id| id.as_i32()).collect()
    }

    /// Retrieves a list of all currently present displays, along with their associated spaces.
    ///
    /// # Returns
    ///
    /// A `Vec<Self>` containing `Display` objects for all present displays.
    #[instrument(level = Level::DEBUG, skip_all, ret)]
    fn observe_displays(&self) -> Result<Vec<DisplayObservation>> {
        let mut count = 0u32;
        unsafe {
            CGGetActiveDisplayList(0, null_mut(), &raw mut count)
                .to_result("count active displays")?;
        }
        if count < 1 {
            return Ok(vec![]);
        }
        let mut displays = vec![0; count.try_into()?];
        unsafe {
            CGGetActiveDisplayList(count, displays.as_mut_ptr(), &raw mut count)
                .to_result("read active displays")?;
        }
        displays.truncate(count.try_into()?);
        Ok(displays
            .into_iter()
            .map(|id| {
                let bounds = CGDisplayBounds(id);
                let mut menubar_height: u32 = 0;
                unsafe { SLSGetDisplayMenubarHeight(id, &raw mut menubar_height) };
                debug!("menubar height: {menubar_height}");
                let spaces = Display::uuid_from_id(id)
                    .and_then(|uuid| self.display_space_list(uuid.as_ref()))
                    .inspect_err(|error| {
                        warn!(display_id = id, %error, "unable to read native Space topology for display");
                    });
                DisplayObservation {
                    display: Display::new(id, irect_from(bounds), menubar_height.cast_signed()),
                    spaces,
                }
            })
            .collect())
    }

    /// Retrieves the `CGDirectDisplayID` of the active menu bar display.
    ///
    /// # Returns
    ///
    /// `Ok(u32)` with the display ID if successful, otherwise `Err(Error)`.
    #[instrument(level = Level::TRACE, skip_all, ret)]
    fn active_display_id(&self) -> Result<u32> {
        let uuid = self.active_display_uuid()?;
        Display::id_from_uuid(&uuid)
    }

    /// Retrieves the ID of the current active space on this display.
    ///
    /// # Returns
    ///
    /// `Ok(u64)` with the space ID if successful, otherwise `Err(Error)`.
    fn active_display_space(&self, display_id: CGDirectDisplayID) -> Result<WorkspaceId> {
        let uuid = Display::uuid_from_id(display_id)?;
        let space_id = unsafe { SLSManagedDisplayGetCurrentSpace(self.main_cid, &raw const *uuid) };
        // The platform answers 0 when it cannot name a current Space, and 0
        // names no Space. Returning it would make an unread display look like
        // one showing a Space that does not exist.
        if space_id == 0 {
            return Err(Error::NotFound(format!(
                "display {display_id} reports no current Space"
            )));
        }
        Ok(space_id)
    }

    fn workspace_is_fullscreen(&self, space_id: WorkspaceId) -> bool {
        unsafe { SLSSpaceGetType(self.main_cid, space_id) == 4 }
    }

    /// Centers the mouse cursor on the window if it's not already within the window's bounds.
    #[instrument(level = Level::DEBUG, skip_all, fields(window))]
    fn warp_mouse(&self, origin: Origin) {
        // Drop the local-event suppression interval to zero so HID mouse
        // events resume immediately after the warp. The default 250ms
        // interval drops physical mouse motion in that window, making the
        // cursor feel "stuck" at the warped position.
        #[allow(deprecated)]
        objc2_core_graphics::CGSetLocalEventsSuppressionInterval(0.0);
        CGWarpMouseCursorPosition(origin_to(origin));
        // Re-associate the mouse and cursor so HID input continues to drive
        // the cursor immediately after the warp.
        CGAssociateMouseAndMouseCursorPosition(true);
    }

    /// Adds existing windows for a given application, attempting to resolve any that are not yet found.
    /// It compares the application's reported window list with the global window list and uses brute-forcing if necessary.
    ///
    /// # Arguments
    ///
    /// * `app` - A mutable reference to the `Application` whose windows are to be added.
    /// * `spaces` - A slice of space IDs to query.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<Window>)` containing the found windows, otherwise `Err(Error)`.
    fn find_existing_application_windows(
        &self,
        app: &mut Application,
        spaces: &[WorkspaceId],
        config: &Config,
    ) -> Result<(Vec<Window>, Vec<WinID>)> {
        let found_windows = app.window_list(config);
        let global_window_list = existing_application_window_list(self.main_cid, app, spaces)
            .inspect_err(|error| debug!(%error, "supplementary WindowServer inventory unavailable"))
            .unwrap_or_default();
        debug!("{app} has global windows: {global_window_list:?}");

        let find_window = |window_id| found_windows.iter().find(|window| window.id() == window_id);
        let offscreen_windows = global_window_list
            .into_iter()
            .filter(|&window_id| find_window(window_id).is_none())
            .collect::<Vec<_>>();
        debug!(
            "{:?} has {} windows that are not yet resolved",
            app.psn(),
            offscreen_windows.len()
        );
        Ok((found_windows, offscreen_windows))
    }

    /// Finds a window at a given screen point using `SkyLight` API.
    ///
    /// # Arguments
    ///
    /// * `point` - A reference to the `CGPoint` representing the screen coordinate.
    ///
    /// # Returns
    ///
    /// `Ok(WinID)` with the found window's ID if successful, otherwise `Err(Error)`.
    fn find_window_at_point(&self, point: &CGPoint) -> Result<WinID> {
        let mut window_id: WinID = 0;
        let mut window_conn_id: ConnID = 0;
        let mut window_point = CGPoint { x: 0f64, y: 0f64 };
        unsafe {
            SLSFindWindowAndOwner(
                self.main_cid,
                0, // filter window id
                1,
                0,
                point,
                &mut window_point,
                &mut window_id,
                &mut window_conn_id,
            )
        }
        .to_result(function_name!())?;
        if self.main_cid == window_conn_id {
            unsafe {
                SLSFindWindowAndOwner(
                    self.main_cid,
                    window_id,
                    -1,
                    0,
                    point,
                    &mut window_point,
                    &mut window_id,
                    &mut window_conn_id,
                )
            }
            .to_result(function_name!())?;
        }
        if window_id == 0 {
            Err(Error::invalid_window(&format!(
                "could not find a window at {point:?}",
            )))
        } else {
            Ok(window_id)
        }
    }

    /// Returns a list of windows in a given workspace.
    fn windows_in_workspace(&self, space_id: WorkspaceId) -> Result<Vec<WinID>> {
        space_window_list_for_connection(self.main_cid, &[space_id], None, true)
    }

    fn presentation_windows_in_workspace(&self, space_id: WorkspaceId) -> Result<Vec<WinID>> {
        space_window_list_for_connection(self.main_cid, &[space_id], None, false)
    }

    fn quit(&self) -> Result<()> {
        self.event_sender.send(Event::Exit)
    }

    fn cursor_position(&self) -> Option<CGPoint> {
        let mut cursor = CGPoint::default();
        unsafe { SLSGetCurrentCursorLocation(self.main_cid, &mut cursor) }
            .to_result(function_name!())
            .ok()?;
        Some(cursor)
    }

    #[cfg(feature = "lua")]
    fn setup_config_watcher(&self, path: &Path) -> Result<Box<dyn Watcher>> {
        let setup = notify::Config::default()
            .with_poll_interval(Duration::from_secs(3))
            .with_follow_symlinks(false);
        let config_handler = ConfigHandler(self.event_sender.clone());
        let symlink = symlink_target(path);

        let mut watcher = if let Some(symlink) = symlink {
            setup.with_follow_symlinks(true);
            let mut watcher = notify::PollWatcher::new(config_handler, setup)?;
            debug!("watching symlink target {} for changes.", symlink.display());
            watcher.watch(&symlink, RecursiveMode::NonRecursive)?;

            Ok::<Box<dyn Watcher>, Error>(Box::new(watcher))
        } else {
            Ok::<Box<dyn Watcher>, Error>(Box::new(notify::RecommendedWatcher::new(
                config_handler,
                setup,
            )?))
        }?;
        // Watch the parent as well: a removed file must be discoverable when
        // an editor recreates it, even after its original inode is gone.
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        if path.exists() {
            debug!("watching config file {} for changes.", path.display());
            watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        Ok(watcher)
    }

    /// level: 0.0 = normal, 1.0 = bright, -1.0 = dark
    fn dim_windows(&self, windows: &[WinID], level: f32) {
        let Ok(count) = isize::try_from(windows.len()) else {
            return;
        };
        let levels = vec![level; windows.len()];

        _ = unsafe {
            SLSSetWindowListBrightness(self.main_cid, windows.as_ptr(), levels.as_ptr(), count)
        }
        .to_result(function_name!())
        .inspect_err(|err| debug!("{err}"));
    }

    fn window_owners_in_session(&self) -> Option<HashMap<WinID, Pid>> {
        window_owners_matching(
            CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
        )
    }

    fn window_order_in_session(&self) -> Option<Vec<(WinID, Pid)>> {
        // Only presented windows have a trustworthy relative position. `OptionAll`
        // keeps minimized and off-Space windows in the list, where the measurement
        // on macOS 26.6.2 places them ahead of every visible window.
        window_order_matching(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        )
    }

    fn request_window_notifications(&self, window_ids: &[WinID]) -> Result<()> {
        if crate::platform::macos_major_version() < 15 {
            return Ok(());
        }
        let count = i32::try_from(window_ids.len()).map_err(|_| {
            Error::InvalidInput(
                "too many windows for WindowServer notification subscription".into(),
            )
        })?;
        unsafe {
            SLSRequestNotificationsForWindows(
                self.main_cid,
                window_ids.as_ptr().cast::<u32>(),
                count,
            )
        }
        .to_result(function_name!())
    }
}

fn window_owners_matching(options: CGWindowListOption) -> Option<HashMap<WinID, Pid>> {
    window_order_matching(options).map(|windows| windows.into_iter().collect())
}

fn window_order_matching(options: CGWindowListOption) -> Option<Vec<(WinID, Pid)>> {
    CGWindowListCopyWindowInfo(options, kCGNullWindowID).map(|window_info| {
        let array = unsafe { window_info.cast_unchecked::<CFDictionary<CFString, CFNumber>>() };
        array
            .iter()
            .filter_map(|description| window_owner_from_description(&description))
            .collect()
    })
}

fn window_owner_from_description(
    description: &CFDictionary<CFString, CFNumber>,
) -> Option<(WinID, Pid)> {
    let window_id = description.get(unsafe { kCGWindowNumber })?.as_i32()?;
    let owner_pid = description.get(unsafe { kCGWindowOwnerPID })?.as_i32()?;
    let layer = description
        .get(unsafe { kCGWindowLayer })
        .and_then(|layer| layer.as_i32());
    let alpha = description
        .get(unsafe { kCGWindowAlpha })
        .and_then(|alpha| alpha.as_f64());

    // Some applications retain the closed window's WindowServer ID as an
    // invisible, non-normal-level surface after removing it from their AX
    // window inventory. It is not evidence that the tracked window remains
    // alive. Missing metadata stays fail-open so an incomplete CoreGraphics
    // dictionary cannot cause a destructive lifecycle decision.
    if matches!((layer, alpha), (Some(layer), Some(alpha)) if layer != 0 && alpha <= 0.0) {
        return None;
    }
    Some((window_id, owner_pid))
}

fn interactive_owner_from_description(
    description: &CFDictionary<CFString, CFNumber>,
    allow_floating: bool,
) -> Option<(WinID, Pid)> {
    let layer = description.get(unsafe { kCGWindowLayer })?.as_i32()?;
    let alpha = description.get(unsafe { kCGWindowAlpha })?.as_f64()?;
    let admitted_layer =
        layer == 0 || (allow_floating && layer == objc2_core_graphics::kCGFloatingWindowLevel);
    if !admitted_layer || !alpha.is_finite() || alpha <= 0.0 {
        return None;
    }
    let (id, pid) = window_owner_from_description(description)?;
    (id > 0 && pid > 0).then_some((id, pid))
}

/// Retrieves a list of window IDs for specified spaces and connection, with an option to include minimized windows.
/// This function uses `SkyLight` API calls to query windows based on their space, connection, and visibility tags.
///
/// # Arguments
///
/// * `main_cid` - The main connection ID.
/// * `spaces` - A slice of space IDs to query windows from.
/// * `cid` - An optional connection ID. If `None`, the main connection ID is used.
/// * `also_minimized` - A boolean indicating whether to include minimized windows in the result.
///
/// # Returns
///
/// `Ok(Vec<WinID>)` containing the list of window IDs if successful, otherwise `Err(Error)`.
///
/// A Space that holds no windows yields `Ok(vec![])`. Only a read that could not
/// be performed yields `Err`.
fn space_window_list_for_connection(
    main_cid: ConnID,
    spaces: &[WorkspaceId],
    cid: Option<ConnID>,
    also_minimized: bool,
) -> Result<Vec<WinID>> {
    let Some(iterator) = window_iterator_for_connection(main_cid, spaces, cid, also_minimized)?
    else {
        return Ok(Vec::new());
    };
    let count = spaces.len();
    let mut window_list = Vec::with_capacity(count);
    let mut floating_candidates = Vec::new();
    while unsafe { SLSWindowIteratorAdvance(&raw const *iterator) } {
        let tags = unsafe { SLSWindowIteratorGetTags(&raw const *iterator) };
        let attributes = unsafe { SLSWindowIteratorGetAttributes(&raw const *iterator) };
        let parent_wid: WinID = unsafe { SLSWindowIteratorGetParentID(&raw const *iterator) };
        let window_id: WinID = unsafe { SLSWindowIteratorGetWindowID(&raw const *iterator) };

        trace!(
            "id: {window_id} parent: {parent_wid} tags: 0x{tags:x} attributes: 0x{attributes:x}",
        );
        if found_valid_window(parent_wid, attributes, tags) {
            window_list.push(window_id);
        } else if floating_membership_candidate(parent_wid, attributes, tags) {
            floating_candidates.push(window_id);
        }
    }
    // Native floating panels can lack the ordinary-window tag. This only
    // supplements Space membership; AX admission still controls tracking.
    if !floating_candidates.is_empty() {
        let options = CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements;
        if let Some(info) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) {
            let array = unsafe { info.cast_unchecked::<CFDictionary<CFString, CFNumber>>() };
            let floating_ids = array
                .iter()
                .filter_map(|description| {
                    let layer = description.get(unsafe { kCGWindowLayer })?.as_i32()?;
                    (layer == objc2_core_graphics::kCGFloatingWindowLevel)
                        .then(|| interactive_owner_from_description(&description, true))
                        .flatten()
                        .map(|(id, _)| id)
                })
                .collect::<HashSet<_>>();
            window_list.extend(
                floating_candidates
                    .into_iter()
                    .filter(|id| floating_ids.contains(id)),
            );
        }
    }
    Ok(window_list)
}

fn floating_membership_candidate(parent_wid: WinID, attributes: i64, tags: i64) -> bool {
    parent_wid == 0 && attributes & 0x2 != 0 && tags & 0x2 != 0
}

/// Builds the platform window iterator for `spaces`.
///
/// `Ok(None)` means the platform listed no windows for these Spaces. That is a
/// successful empty observation, not a read failure: a Space with no windows
/// and an unreadable Space must stay distinguishable at the interface, so this
/// never reports the empty case as an error.
fn window_iterator_for_connection(
    main_cid: ConnID,
    spaces: &[WorkspaceId],
    cid: Option<ConnID>,
    also_minimized: bool,
) -> Result<Option<CFRetained<CFType>>> {
    let space_list_ref = create_array(spaces, CFNumberType::SInt64Type)?;

    let mut set_tags = 0i64;
    let mut clear_tags = 0i64;
    let options = if also_minimized { 0x7 } else { 0x2 };
    let ptr = NonNull::new(unsafe {
        SLSCopyWindowsWithOptionsAndTags(
            main_cid,
            cid.unwrap_or(0),
            &raw const *space_list_ref,
            options,
            &mut set_tags,
            &mut clear_tags,
        )
    })
    .ok_or(Error::InvalidInput(format!(
        "{}: nullptr returned from SLSCopyWindowsWithOptionsAndTags.",
        function_name!()
    )))?;
    let window_list_ref = unsafe { CFRetained::from_raw(ptr) };

    let count = window_list_ref.count();
    if count == 0 {
        return Ok(None);
    }

    let query = unsafe {
        CFRetained::from_raw(SLSWindowQueryWindows(
            main_cid,
            &raw const *window_list_ref,
            count,
        ))
    };
    Ok(Some(unsafe {
        CFRetained::from_raw(SLSWindowQueryResultCopyWindows(query.deref().into()))
    }))
}

pub fn window_iterator_for_id(window_id: WinID) -> Option<CFRetained<CFType>> {
    let cid = unsafe { SLSMainConnectionID() };
    let windows = create_array(&[window_id], CFNumberType::SInt32Type).ok()?;
    let query = unsafe { CFRetained::from_raw(SLSWindowQueryWindows(cid, &raw const *windows, 1)) };
    Some(unsafe { CFRetained::from_raw(SLSWindowQueryResultCopyWindows(query.deref().into())) })
}

/// Determines if a window is valid based on its parent ID, attributes, and tags.
/// This function implements complex logic to filter out irrelevant or invalid windows.
///
/// # Arguments
///
/// * `parent_wid` - The parent window ID.
/// * `attributes` - The attributes of the window.
/// * `tags` - The tags associated with the window.
///
/// # Returns
///
/// `true` if the window is considered valid, `false` otherwise.
fn found_valid_window(parent_wid: WinID, attributes: i64, tags: i64) -> bool {
    parent_wid == 0
        && ((0 != (attributes & 0x2) || 0 != (tags & 0x0400_0000_0000_0000))
            && (0 != (tags & 0x1) || (0 != (tags & 0x2) && 0 != (tags & 0x8000_0000))))
        || ((attributes == 0x0 || attributes == 0x1)
            && (0 != (tags & 0x1000_0000_0000_0000) || 0 != (tags & 0x0300_0000_0000_0000))
            && (0 != (tags & 0x1) || (0 != (tags & 0x2) && 0 != (tags & 0x8000_0000))))
}

/// Retrieves a list of existing application window IDs for a given application.
/// It queries windows across all active displays and spaces associated with the application's connection.
///
/// # Arguments
///
/// * `cid` - The main connection ID.
/// * `app` - A reference to the `Application` for which to retrieve window IDs.
/// * `spaces` - A slice of space IDs to query.
///
/// # Returns
///
/// `Ok(Vec<WinID>)` containing the list of window IDs if successful, otherwise `Err(Error)`.
fn existing_application_window_list(
    cid: ConnID,
    app: &Application,
    spaces: &[WorkspaceId],
) -> Result<Vec<WinID>> {
    if spaces.is_empty() {
        return Err(Error::NotFound(format!(
            "{}: no spaces returned",
            function_name!()
        )));
    }
    space_window_list_for_connection(cid, spaces, app.connection(), true)
}

/// Checks if the application has Accessibility privileges without showing UI.
///
/// # Returns
///
/// `true` if Accessibility privileges are granted, `false` otherwise.
pub fn check_ax_privilege() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Requests Accessibility privileges once through the native macOS prompt.
///
/// Subsequent permission polling must use [`check_ax_privilege`] so the system
/// dialog is not requested repeatedly while the application waits.
pub fn request_ax_privilege() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt
            .cast::<CFString>()
            .as_ref()
            .unwrap()];
        let values = [kCFBooleanTrue.unwrap()];
        let opts = CFDictionary::from_slices(&keys, &values);
        AXIsProcessTrustedWithOptions((&raw const *opts).cast())
    }
}

/// Checks if the macOS "Displays have separate Spaces" option is enabled.
/// This is crucial for the window manager's functionality, as Spool relies on independent spaces per display.
///
/// # Returns
///
/// `true` if separate spaces are enabled, `false` otherwise.
pub fn check_separate_spaces() -> bool {
    unsafe {
        let cid = SLSMainConnectionID();
        SLSGetSpaceManagementMode(cid) == 1
    }
}

/// `ConfigHandler` is an implementation of `notify::EventHandler` that reloads the application configuration
/// when the configuration file changes. It also dispatches a `ConfigRefresh` event.
#[cfg(feature = "lua")]
struct ConfigHandler(EventSender);

#[cfg(feature = "lua")]
impl notify::EventHandler for ConfigHandler {
    /// Handles file system events for the configuration file. When the content changes, it reloads the configuration.
    /// Specifically, it responds to `ModifyKind::Data(DataChange::Content)` events.
    ///
    /// # Arguments
    ///
    /// * `event` - The result of a file system event.
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        if let Ok(event) = event {
            _ = self.0.send(Event::ConfigRefresh(event)).inspect_err(|err| {
                warn!("error sending config refresh: {err}");
            });
        }
    }
}

#[cfg(test)]
mod native_space_runtime_tests {
    use super::*;

    fn window_description(
        window_id: WinID,
        owner_pid: Pid,
        layer: i32,
        alpha: f64,
    ) -> CFRetained<CFDictionary<CFString, CFNumber>> {
        let window_id = CFNumber::new_i32(window_id);
        let owner_pid = CFNumber::new_i32(owner_pid);
        let layer = CFNumber::new_i32(layer);
        let alpha = CFNumber::new_f64(alpha);
        CFDictionary::from_slices(
            &[
                unsafe { kCGWindowNumber },
                unsafe { kCGWindowOwnerPID },
                unsafe { kCGWindowLayer },
                unsafe { kCGWindowAlpha },
            ],
            &[
                window_id.as_ref(),
                owner_pid.as_ref(),
                layer.as_ref(),
                alpha.as_ref(),
            ],
        )
    }

    #[test]
    fn lifecycle_inventory_excludes_transparent_non_normal_surface() {
        let description = window_description(695, 1505, 101, 0.0);

        assert_eq!(window_owner_from_description(&description), None);
    }

    #[test]
    fn lifecycle_inventory_keeps_transparent_normal_surface() {
        let description = window_description(42, 1000, 0, 0.0);

        assert_eq!(
            window_owner_from_description(&description),
            Some((42, 1000))
        );
    }

    #[test]
    fn lifecycle_inventory_keeps_visible_non_normal_surface() {
        let description = window_description(42, 1000, 3, 1.0);

        assert_eq!(
            window_owner_from_description(&description),
            Some((42, 1000))
        );
    }

    #[test]
    fn native_floating_membership_does_not_require_ordinary_window_tags() {
        // Finder Quick Look sample from the native read-only inspector.
        let tags = 0x0001_000c_2802;
        assert!(!found_valid_window(0, 0x2, tags));
        assert!(floating_membership_candidate(0, 0x2, tags));
        assert!(!floating_membership_candidate(100, 0x2, tags));
        assert!(!floating_membership_candidate(0, 0, tags));
        assert!(!floating_membership_candidate(0, 0x2, 0));
    }

    #[test]
    fn fallback_surface_accepts_native_float_but_not_system_or_retained_surfaces() {
        for layer in [0, objc2_core_graphics::kCGFloatingWindowLevel] {
            assert_eq!(
                interactive_owner_from_description(&window_description(42, 1000, layer, 1.0), true),
                Some((42, 1000))
            );
        }
        for (layer, alpha) in [(8, 1.0), (25, 1.0), (101, 0.0), (3, 0.0), (0, f64::NAN)] {
            assert_eq!(
                interactive_owner_from_description(
                    &window_description(42, 1000, layer, alpha),
                    true
                ),
                None
            );
        }
    }

    #[test]
    fn missing_presentation_metadata_is_not_destructive_lifecycle_evidence() {
        let id = CFNumber::new_i32(42);
        let pid = CFNumber::new_i32(1000);
        let description = CFDictionary::from_slices(
            &[unsafe { kCGWindowNumber }, unsafe { kCGWindowOwnerPID }],
            &[id.as_ref(), pid.as_ref()],
        );
        assert_eq!(interactive_owner_from_description(&description, true), None);
        assert_eq!(
            window_owner_from_description(&description),
            Some((42, 1000))
        );
    }

    #[test]
    #[ignore = "requires macOS 26.4+ private SkyLight runtime"]
    fn bridged_window_move_capability_is_discoverable() {
        assert!(bridged_window_move_class().is_some());
    }

    #[test]
    #[ignore = "requires macOS 26.4+ private SkyLight runtime"]
    fn cf_window_list_is_bridged_as_an_object() {
        let class = bridged_window_move_class().expect("bridged move class");
        let windows = create_array(&[0], CFNumberType::SInt32Type).expect("window array");
        let operation = create_bridged_window_move_operation(class, &windows, 0)
            .expect("bridged move operation");
        unsafe {
            let _: () = msg_send![operation, release];
        }
    }

    #[test]
    fn native_space_gesture_delta_uses_display_order() {
        let spaces = [10, 20, 30, 40];
        assert_eq!(native_space_gesture_delta(&spaces, 10, 40).unwrap(), 3);
        assert_eq!(native_space_gesture_delta(&spaces, 40, 20).unwrap(), -2);
        assert_eq!(native_space_gesture_delta(&spaces, 20, 20).unwrap(), 0);
        assert!(native_space_gesture_delta(&spaces, 99, 20).is_err());
        assert!(native_space_gesture_delta(&spaces, 20, 99).is_err());
    }

    fn observed_display(
        id: CGDirectDisplayID,
        spaces: Result<Vec<WorkspaceId>>,
    ) -> DisplayObservation {
        DisplayObservation {
            display: Display::new(id, IRect::new(0, 0, 1000, 800), 0),
            spaces,
        }
    }

    #[test]
    fn active_display_spaces_requires_the_active_display_to_list_the_target() {
        let displays = [
            observed_display(1, Ok(vec![10, 20])),
            observed_display(2, Ok(vec![30, 40])),
        ];
        assert_eq!(
            active_display_spaces(&displays, 1, 20).unwrap(),
            vec![10, 20]
        );
        // The target lives on the other display.
        assert!(active_display_spaces(&displays, 1, 30).is_err());
        // No display lists the target at all.
        assert!(active_display_spaces(&displays, 1, 99).is_err());
    }

    /// A display whose Space list could not be read is not evidence that the
    /// target Space is absent, so it must not be reported as a refusal about
    /// the target. This is the distinction the previous inline version lost.
    #[test]
    fn active_display_spaces_separates_an_unreadable_list_from_an_absent_target() {
        let displays = [
            observed_display(1, Err(Error::Generic("topology unavailable".into()))),
            observed_display(2, Ok(vec![30])),
        ];
        let unreadable = active_display_spaces(&displays, 1, 30).unwrap_err();
        assert!(
            !matches!(unreadable, Error::InvalidInput(_)),
            "an unreadable Space list was reported as a refusal about the target: {unreadable:?}"
        );

        let absent = active_display_spaces(&displays, 2, 99).unwrap_err();
        assert!(
            matches!(absent, Error::InvalidInput(_)),
            "a readable list without the target is a refusal: {absent:?}"
        );
    }

    /// The active display need not appear in the inventory at all.
    #[test]
    fn active_display_spaces_reports_a_display_missing_from_the_inventory() {
        let displays = [observed_display(2, Ok(vec![30]))];
        let error = active_display_spaces(&displays, 1, 30).unwrap_err();
        assert!(
            !matches!(error, Error::InvalidInput(_)),
            "a display that was never observed is not a refusal about the target: {error:?}"
        );
    }
}

#[cfg(test)]
mod ax_timeout_tests {
    use super::*;

    #[test]
    fn ax_timeout_initialization_must_not_ignore_setter_failure() {
        let result = initialize_ax_timeout_with(
            || Ok("system-wide"),
            |element, timeout| {
                assert_eq!(*element, "system-wide");
                assert!((timeout - 0.25).abs() < f32::EPSILON);
                accessibility_sys::kAXErrorInvalidUIElement
            },
        );
        assert_eq!(
            result.unwrap_err().macos_code(),
            Some(accessibility_sys::kAXErrorInvalidUIElement)
        );
    }

    #[test]
    fn ax_timeout_initialization_stops_before_setter_when_creation_fails() {
        let result = initialize_ax_timeout_with::<()>(
            || Err(Error::Generic("creation failed".to_string())),
            |(), _| panic!("no element was created"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn ax_timeout_initialization_accepts_a_successful_setter() {
        assert!(initialize_ax_timeout_with(|| Ok(()), |(), _| 0).is_ok());
    }
}

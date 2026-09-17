use accessibility_sys::{
    AXUIElementCreateApplication, AXUIElementIsAttributeSettable, AXUIElementRef, AXValueCreate,
    AXValueGetValue, kAXCloseButtonAttribute, kAXErrorAttributeUnsupported, kAXErrorNoValue,
    kAXMinimizeButtonAttribute, kAXParentAttribute, kAXPositionAttribute, kAXRaiseAction,
    kAXSizeAttribute, kAXValueTypeCGPoint, kAXValueTypeCGSize,
};
use bevy::ecs::component::Component;
use bevy::math::IRect;
use core::ptr::NonNull;
use derive_more::{DerefMut, with_trait::Deref};
use mockall::automock;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFHash, CFNumber, CFRetained, CFString, CFType, CGPoint,
    CGRect, CGSize, kCFBooleanFalse, kCFBooleanTrue,
};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::Duration;
use stdext::function_name;
use stdext::sync::rw_lock::RwLockExt;
use tracing::{Level, debug, instrument, trace, warn};

use super::skylight::{
    _AXUIElementGetWindow, _SLPSSetFrontProcessWithOptions, AXUIElementPerformAction,
    AXUIElementSetAttributeValue, SLPSPostEventRecordTo, SLSWindowIteratorAdvance,
};
use crate::config::Config;
use crate::errors::{Error, Result};
use crate::manager::{Origin, Size, irect_from};
use crate::platform::{Pid, ProcessSerialNumber, WinID, WindowIncarnation, macos_major_version};
use crate::util::{AXUIAttributes, AXUIWrapper, MacResult};
use crate::window_policy::{self, Admission, LayoutDecision};
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EnhancedUiKey {
    pid: Pid,
    application_incarnation: WindowIncarnation,
}

impl EnhancedUiKey {
    fn new(pid: Pid, application_incarnation: WindowIncarnation) -> Self {
        Self {
            pid,
            application_incarnation,
        }
    }
}

#[derive(Debug)]
struct EnhancedUiState {
    depth: usize,
    restore: bool,
}

impl EnhancedUiState {
    fn new(restore: bool) -> Self {
        Self { depth: 1, restore }
    }

    fn acquire(&mut self) {
        self.depth += 1;
    }

    /// Returns whether the attribute must be restored when this was the final lease.
    fn release(&mut self) -> Option<bool> {
        self.depth = self.depth.saturating_sub(1);
        (self.depth == 0).then_some(self.restore)
    }
}

/// Active `AXEnhancedUserInterface` leases, keyed by the exact AX application
/// identity. The mutex deliberately covers the first AX read/write and the last
/// restore so two concurrent first users cannot both toggle the same app, and a
/// reused PID cannot inherit state from an earlier application object.
static ENHANCED_UI_STATES: LazyLock<Mutex<HashMap<EnhancedUiKey, EnhancedUiState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn acquire_enhanced_ui_state(
    states: &Mutex<HashMap<EnhancedUiKey, EnhancedUiState>>,
    key: EnhancedUiKey,
    first_acquire: impl FnOnce() -> bool,
) {
    let mut states = states
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match states.entry(key) {
        std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().acquire(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(EnhancedUiState::new(first_acquire()));
        }
    }
}

fn release_enhanced_ui_state(
    states: &Mutex<HashMap<EnhancedUiKey, EnhancedUiState>>,
    key: EnhancedUiKey,
    restore: impl FnOnce(),
) {
    let mut states = states
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(state) = states.get_mut(&key) else {
        return;
    };
    let Some(should_restore) = state.release() else {
        return;
    };
    if should_restore {
        restore();
    }
    states.remove(&key);
}

struct EnhancedUiLease {
    key: EnhancedUiKey,
    application: CFRetained<AXUIWrapper>,
}

/// macOS may partially apply an AX width increase when the requested right edge
/// would be far outside the display. Moving the partial result left by the
/// missing width before retrying gives `WindowServer` enough offscreen room.
///
/// Only retry when the first attempt actually grew the window. Fixed-size apps
/// otherwise look identical to this failure mode and must not be moved offscreen.
fn resize_staging_origin(
    previous_frame: IRect,
    actual_frame: IRect,
    target_width: i32,
) -> Option<Origin> {
    let actual_width = actual_frame.width();
    (actual_width > previous_frame.width() && actual_width < target_width).then(|| {
        actual_frame
            .min
            .with_x(actual_frame.min.x - (target_width - actual_width))
    })
}

fn observe_geometry_write_result(
    operation: &'static str,
    write_result: Result<()>,
    observe: impl FnOnce() -> Result<IRect>,
) -> Result<IRect> {
    let observed_result = observe();
    match (write_result, observed_result) {
        (Ok(()), observed) => observed,
        (Err(write_error), Ok(observed)) => {
            warn!(%write_error, operation, ?observed, "AX geometry request only partially succeeded");
            Err(write_error)
        }
        (Err(write_error), Err(observe_error)) => {
            warn!(%observe_error, operation, "unable to read back failed AX geometry request");
            Err(write_error)
        }
    }
}

#[derive(Debug)]
pub enum WindowPadding {
    Vertical(i32),
    Horizontal(i32),
}

#[automock]
pub trait WindowApi: Send + Sync {
    fn default_floating(&self) -> bool;
    fn id(&self) -> WinID;
    fn incarnation(&self) -> WindowIncarnation;
    /// Current native control target for this independently presented window.
    /// The backend may retain shared chrome while the application changes its AX root.
    fn represented_window_id(&self) -> Result<WinID>;
    fn frame(&self) -> IRect;
    fn element(&self) -> Option<CFRetained<AXUIWrapper>>;
    fn title(&self) -> Result<String>;
    /// Reads only the retained title. Never fetches AX data on a cache miss.
    fn retained_title(&self) -> Option<String>;
    /// Drops the cached title so the next [`Self::title`] reads it afresh.
    /// Called when the app reports the title changed.
    fn invalidate_title(&self);
    fn child_role(&self) -> Result<bool>;
    fn role(&self) -> Result<String>;
    fn subrole(&self) -> Result<String>;
    fn is_minimized(&self) -> bool;
    /// Reads the native fullscreen attribute without collapsing an AX failure
    /// into `false`.
    fn try_is_full_screen(&self) -> Result<bool>;
    fn is_movable(&self) -> Result<bool>;
    fn is_resizable(&self) -> Result<bool>;
    fn is_full_screen(&self) -> bool;
    /// Requests a new origin and returns the frame read back from AX.
    fn reposition(&mut self, origin: Origin) -> Result<IRect>;
    /// Resizes a frame whose origin is intended to stay fixed. The common
    /// path performs one size write; if AX moves the window as a side effect,
    /// the origin is restored before the final confirmed frame is returned.
    fn resize_preserving_origin(&mut self, frame: IRect) -> Result<IRect>;
    /// Applies a complete target frame and returns the frame confirmed by AX.
    fn set_frame(&mut self, frame: IRect) -> Result<IRect>;
    fn update_frame(&mut self) -> Result<IRect>;
    fn focus_without_raise(
        &self,
        psn: ProcessSerialNumber,
        currently_focused: &Window,
        focused_psn: ProcessSerialNumber,
    ) -> Result<()>;
    fn focus_with_raise(&self, psn: ProcessSerialNumber) -> Result<()>;
    /// Raises the window in the OS z-order without changing focus. Used to
    /// shuffle the floating-vs-tiled tier order. Best-effort: AX raise can't
    /// lift a window above another app's frontmost window.
    fn raise_without_focus(&self);
    fn pid(&self) -> Result<Pid>;
    fn set_padding(&mut self, padding: WindowPadding);
    fn horizontal_padding(&self) -> i32;
    fn vertical_padding(&self) -> i32;
    fn border_radius(&self) -> Option<f64>;
}

#[derive(Component, Deref, DerefMut)]
pub struct Window(Box<dyn WindowApi>);

impl Window {
    pub fn new(window: Box<dyn WindowApi>) -> Self {
        Window(window)
    }

    pub(crate) fn layout_decision(&self, floating: bool) -> LayoutDecision {
        if floating {
            return window_policy::layout(None, None, true);
        }
        window_policy::layout(self.is_movable().ok(), self.is_resizable().ok(), false)
    }
}

/// Retrieves the window ID (`WinID`) from an `AXUIElementRef`.
///
/// # Arguments
///
/// * `element_ref` - The `AXUIElementRef` to extract the window ID from.
///
/// # Returns
///
/// `Ok(WinID)` with the window ID if successful, otherwise `Err(Error)`.
pub fn ax_window_id(element_ref: AXUIElementRef) -> Result<WinID> {
    try_ax_window_id(element_ref).ok_or_else(|| {
        Error::InvalidInput(format!(
            "{}: Unable to get window id from element {element_ref:?}.",
            function_name!()
        ))
    })
}

/// Allocation-free variant of [`ax_window_id`].
///
/// Incremental remote-token discovery discards nearly every result, so the
/// error path must not format a message it will only drop.
pub fn try_ax_window_id(element_ref: AXUIElementRef) -> Option<WinID> {
    let ptr = NonNull::new(element_ref)?;
    let mut window_id: WinID = 0;
    if unsafe { _AXUIElementGetWindow(ptr.as_ptr(), &mut window_id) } != 0 || window_id == 0 {
        return None;
    }
    Some(window_id)
}

pub(crate) fn ax_window_incarnation(element: &AXUIWrapper) -> WindowIncarnation {
    let element: &CFType = element.as_ref();
    CFHash(Some(element)) as WindowIncarnation
}

// const CPS_ALL_WINDOWS: u32 = 0x100;
const CPS_USER_GENERATED: u32 = 0x200;
// const CPS_NO_WINDOWS: u32 = 0x400;

#[derive(Debug)]
pub struct WindowOS {
    default_floating: bool,
    id: WinID,
    ax_element: CFRetained<AXUIWrapper>,
    frame: IRect,
    vertical_padding: i32,
    horizontal_padding: i32,
    border_radius: OnceLock<Option<f64>>,
    pid: OnceLock<Result<Pid>>,
    /// The application this window belongs to, for the census label. The window's
    /// own element has no bundle id, and the caller already knows it.
    app_bundle: Option<String>,
    app_reference: OnceLock<Option<CFRetained<AXUIWrapper>>>,
    presentation_anchor: OnceLock<Option<CFRetained<AXUIWrapper>>>,

    /// The last title read off the element, cached because reading one is a
    /// synchronous cross-process call and many callers want it for every
    /// window at once.
    ///
    /// An `RwLock` rather than a `OnceLock` like its neighbours: a title can
    /// change, and [`Self::invalidate_title`] clears it when the app reports
    /// `kAXTitleChangedNotification`. Missing that notification is the one
    /// way this can go stale.
    title: RwLock<Option<String>>,
}

#[derive(Default)]
pub(super) struct WindowEvidence {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub parent_role: Option<String>,
    pub fallback: window_policy::FallbackEvidence,
    pub position_valid: Option<bool>,
}

impl WindowEvidence {
    pub(super) fn needs_fallback(&self, config: &Config, bundle_id: Option<&str>) -> bool {
        self.role.as_deref() == Some("AXWindow")
            && self.subrole.is_some()
            && self.parent_role.as_deref() == Some("AXApplication")
            && self.admission(config, bundle_id) == Admission::Defer
    }
    pub(super) fn admission(&self, config: &Config, bundle_id: Option<&str>) -> Admission {
        let rules = config.match_window_rules(
            self.title.as_deref(),
            Some(bundle_id.unwrap_or_default()),
            self.role.as_deref(),
            self.subrole.as_deref(),
        );
        // An unsupported AXParent is not proof of an attached child window.
        if rules.pending {
            Admission::Defer
        } else {
            window_policy::admission_with_fallback(
                self.role.as_deref(),
                self.subrole.as_deref(),
                self.parent_role.as_deref(),
                rules.params.iter().find_map(|rule| rule.track),
                self.fallback,
            )
        }
    }
}

impl WindowOS {
    /// Who this window is, for the accessibility census. The bundle id is filled
    /// in where the caller knows the application; the window number and the pid
    /// are always known here.
    fn census_context(&self) -> super::ax_census::Context<'_> {
        super::ax_census::Context {
            window: Some(self.id),
            pid: self.pid.get().and_then(|pid| pid.as_ref().ok().copied()),
            bundle_id: self.app_bundle.as_deref(),
        }
    }

    /// Tells this window which application owns it, before any evidence is read:
    /// without it every window-level census row is labelled with an unknown
    /// application, which is the one column a per-application question needs.
    pub(super) fn set_census_application(&mut self, pid: Option<Pid>, bundle_id: Option<&str>) {
        // Only a caller that knows the owning application seeds the pid; the
        // lazy element read stays the fallback for everyone else.
        if let Some(pid) = pid {
            let _ = self.pid.set(Ok(pid));
        }
        self.app_bundle = bundle_id.map(str::to_owned);
    }

    /// Creates a new `Window` instance using an empty configuration.
    /// Non-standard windows require an explicit rule or independent-window evidence.
    ///
    /// # Arguments
    ///
    /// * `element` - A `CFRetained<AXUIWrapper>` reference to the Accessibility UI element.
    ///
    /// # Returns
    ///
    /// `Ok(Window)` if the window is created successfully, otherwise `Err(Error)`.
    #[instrument(level = Level::TRACE, ret)]
    pub fn new(element: &CFRetained<AXUIWrapper>) -> Result<Self> {
        // The caller here is a test or a one-off probe: it owns no application
        // context, so the census labels the window with an unknown one.
        Self::new_with_config(element, &Config::default(), None, None)
    }

    /// Creates a new `Window` instance.
    ///
    /// # Arguments
    ///
    /// * `element` - A `CFRetained<AXUIWrapper>` reference to the Accessibility UI element.
    /// * `config` - The current Spool configuration, used to evaluate window rules.
    /// * `bundle_id` - The bundle identifier of the owning application, if known.
    ///
    /// # Returns
    ///
    /// `Ok(Window)` if the window is created successfully, otherwise `Err(Error)`.
    #[instrument(level = Level::TRACE, ret)]
    pub fn new_with_config(
        element: &CFRetained<AXUIWrapper>,
        config: &Config,
        bundle_id: Option<&str>,
        owner_pid: Option<Pid>,
    ) -> Result<Self> {
        let context = super::ax_census::Context {
            window: None,
            pid: owner_pid,
            bundle_id,
        };
        // The first call that can drop a candidate: without a window number there
        // is nothing to admit, and that fact was invisible until it was counted.
        let id = super::ax_census::ax_read(&context, "AXWindowId", ax_window_id(element.as_ptr()))?;
        let mut window = Self::from_element(id, element);
        // Labelled before the first read, so a census row names the application.
        window.set_census_application(owner_pid, bundle_id);
        let mut evidence = WindowEvidence {
            role: window.role().ok(),
            subrole: window.subrole().ok(),
            title: window.title().ok(),
            parent_role: window
                .parent_element()
                .and_then(|parent| parent.role())
                .ok(),
            ..Default::default()
        };
        // Window chrome and movable/resizable are read for every candidate: the
        // policy needs them to tell a window from a system surface, not only when
        // a subrole is unknown. Geometry and the on-screen surface stay
        // fallback-only, where they are the whole evidence.
        for step in [2, 3, 5, 6] {
            window.read_fallback_step(&mut evidence, step);
        }
        if evidence.needs_fallback(config, bundle_id) {
            for step in [0, 1, 4] {
                window.read_fallback_step(&mut evidence, step);
            }
        }
        window.admit(evidence, config, bundle_id)
    }

    /// Construction without AX reads; discovery gathers evidence across ticks.
    pub(super) fn from_element(id: WinID, element: &CFRetained<AXUIWrapper>) -> Self {
        Self {
            default_floating: false,
            id,
            ax_element: element.clone(),
            frame: IRect::default(),
            vertical_padding: 0,
            horizontal_padding: 0,
            border_radius: OnceLock::new(),
            pid: OnceLock::new(),
            app_bundle: None,
            app_reference: OnceLock::new(),
            presentation_anchor: OnceLock::new(),
            title: RwLock::new(None),
        }
    }

    pub(super) fn parent_element(&self) -> Result<CFRetained<AXUIWrapper>> {
        self.ax_element
            .get_attribute(&CFString::from_static_str(kAXParentAttribute))
    }

    /// Each stage issues at most one cross-process AX read.
    pub(super) fn read_fallback_step(&self, evidence: &mut WindowEvidence, step: u8) {
        match step {
            0 => {
                evidence.position_valid = super::ax_census::ax_read(
                    &self.census_context(),
                    "AXPosition",
                    self.ax_element
                        .get_attribute::<AXUIWrapper>(&CFString::from_static_str(
                            kAXPositionAttribute,
                        )),
                )
                .ok()
                .and_then(|value| {
                    let mut point = CGPoint::default();
                    unsafe {
                        AXValueGetValue(
                            value.as_ptr(),
                            kAXValueTypeCGPoint,
                            NonNull::from(&mut point).as_ptr().cast(),
                        )
                    }
                    .then_some(point.x.is_finite() && point.y.is_finite())
                });
            }
            1 => {
                let size_valid = super::ax_census::ax_read(
                    &self.census_context(),
                    "AXSize",
                    self.ax_element
                        .get_attribute::<AXUIWrapper>(&CFString::from_static_str(kAXSizeAttribute)),
                )
                .ok()
                .and_then(|value| {
                    let mut size = CGSize::default();
                    unsafe {
                        AXValueGetValue(
                            value.as_ptr(),
                            kAXValueTypeCGSize,
                            NonNull::from(&mut size).as_ptr().cast(),
                        )
                    }
                    .then_some(
                        size.width.is_finite()
                            && size.height.is_finite()
                            && size.width > 0.0
                            && size.height > 0.0,
                    )
                });
                evidence.fallback.geometry = evidence
                    .position_valid
                    .zip(size_valid)
                    .map(|(position, size)| position && size);
            }
            2 => {
                evidence.fallback.close_button = self.read_button(kAXCloseButtonAttribute);
            }
            3 => {
                evidence.fallback.minimize_button = self.read_button(kAXMinimizeButtonAttribute);
            }
            5 => {
                evidence.fallback.movable = self.is_movable().ok();
            }
            6 => {
                evidence.fallback.resizable = self.is_resizable().ok();
            }
            _ => {
                let options = CGWindowListOption::OptionOnScreenOnly
                    | CGWindowListOption::ExcludeDesktopElements;
                let info = CGWindowListCopyWindowInfo(options, kCGNullWindowID);
                if info.is_none() {
                    super::ax_census::cg(
                        "CGWindowListCopyWindowInfo",
                        super::ax_census::Kind::Failure,
                        &self.census_context(),
                    );
                }
                evidence.fallback.surface = info.map(|info| {
                    let array =
                        unsafe { info.cast_unchecked::<CFDictionary<CFString, CFNumber>>() };
                    array.iter().any(|description| {
                        super::interactive_owner_from_description(&description, true)
                            .is_some_and(|(id, _)| id == self.id)
                    })
                });
            }
        }
    }

    /// Whether one chrome button exists on this window: `Some(true)` when AX
    /// answers with an element, `Some(false)` when it says there is none, and
    /// `None` when the read itself failed. The three are different facts.
    fn read_button(&self, attribute: &'static str) -> Option<bool> {
        match super::ax_census::ax_read(
            &self.census_context(),
            attribute,
            self.ax_element
                .get_attribute::<AXUIWrapper>(&CFString::from_static_str(attribute)),
        ) {
            Ok(_) => Some(true),
            Err(error)
                if error.macos_code() == Some(kAXErrorNoValue)
                    || error.macos_code() == Some(kAXErrorAttributeUnsupported) =>
            {
                Some(false)
            }
            Err(_) => None,
        }
    }

    pub(super) fn set_admission_preference(&mut self, decision: Admission) {
        self.default_floating = decision == Admission::TrackFloating;
    }

    /// Pure admission shared by `AXWindows` inventory and staged discovery.
    pub(super) fn admit(
        mut self,
        evidence: WindowEvidence,
        config: &Config,
        bundle_id: Option<&str>,
    ) -> Result<Self> {
        let decision = evidence.admission(config, bundle_id);
        debug!(window_id = self.id, ?decision, fallback = ?evidence.fallback,
            "window admission");
        // The admission outcome is a fact about a candidate, not a call result: it
        // is what distinguishes "this application is not read" from "its windows
        // are read and refused", which look identical from the outside.
        super::ax_census::record(
            super::ax_census::Source::Ax,
            "admit",
            match decision {
                Admission::Track => "track",
                Admission::TrackFloating => "track_floating",
                Admission::Ignore => "ignore",
                Admission::Defer => "defer",
            },
            super::ax_census::Kind::Success,
            &self.census_context(),
        );
        let WindowEvidence {
            role,
            subrole,
            title,
            ..
        } = evidence;
        if !matches!(decision, Admission::Track | Admission::TrackFloating) {
            return Err(Error::invalid_window(&format!(
                "Window admission {decision:?}, id: {}, role {role:?}, subrole {subrole:?}",
                self.id()
            )));
        }
        self.set_admission_preference(decision);

        trace!(
            "created {} title: {} role: {} subrole: {}",
            self.id(),
            title.unwrap_or_default(),
            role.unwrap_or_default(),
            subrole.unwrap_or_default(),
        );
        Ok(self)
    }

    fn attribute_is_settable(&self, attribute: &'static str) -> Result<bool> {
        let mut settable = 0;
        let attribute = CFString::from_static_str(attribute);
        let status = unsafe {
            AXUIElementIsAttributeSettable(
                self.ax_element.as_ptr(),
                CFRetained::as_ptr(&attribute).as_ptr().cast(),
                &raw mut settable,
            )
        };
        if status == kAXErrorAttributeUnsupported {
            return Ok(false);
        }
        status.to_result(function_name!())?;
        Ok(settable != 0)
    }

    fn app_reference(&self) -> Option<CFRetained<AXUIWrapper>> {
        self.app_reference
            .get_or_init(|| {
                self.pid()
                    .map(|pid| unsafe { AXUIElementCreateApplication(pid) })
                    .and_then(AXUIWrapper::from_retained)
                    .inspect_err(|err| warn!("error getting app reference: {err}"))
                    .ok()
            })
            .clone()
    }

    /// Disables `AXEnhancedUserInterface` on this window's app if it is currently enabled.
    ///
    /// The returned lease must be passed to [`Self::reenable_enhanced_ui`]. The
    /// first AX probe/write and ref-count acquisition happen under one lock, so
    /// concurrent windows cannot race through two independent first-acquire paths.
    fn disable_enhanced_ui(&self) -> Option<EnhancedUiLease> {
        let pid = self.pid().ok()?;
        let application = self.app_reference()?;
        let key = EnhancedUiKey::new(pid, ax_window_incarnation(&application));
        let attr = CFString::from_static_str("AXEnhancedUserInterface");

        acquire_enhanced_ui_state(&ENHANCED_UI_STATES, key, || {
            let enabled = application
                .get_attribute::<CFBoolean>(&attr)
                .is_ok_and(|value| CFBoolean::value(&value));
            if !enabled {
                return false;
            }

            let result = unsafe {
                AXUIElementSetAttributeValue(
                    application.as_ptr(),
                    attr.as_ref(),
                    kCFBooleanFalse.unwrap(),
                )
            }
            .to_result("disable AXEnhancedUserInterface");
            if let Err(error) = result {
                warn!(%error, "unable to disable AXEnhancedUserInterface");
                return false;
            }
            true
        });

        Some(EnhancedUiLease { key, application })
    }

    /// Releases an `AXEnhancedUserInterface` lease and restores the attribute
    /// after the final concurrent operation on this exact application object.
    fn reenable_enhanced_ui(lease: Option<EnhancedUiLease>) {
        let Some(lease) = lease else { return };
        release_enhanced_ui_state(&ENHANCED_UI_STATES, lease.key, || {
            let attr = CFString::from_static_str("AXEnhancedUserInterface");
            let result = unsafe {
                AXUIElementSetAttributeValue(
                    lease.application.as_ptr(),
                    attr.as_ref(),
                    kCFBooleanTrue.unwrap(),
                )
            }
            .to_result("reenable AXEnhancedUserInterface");
            if let Err(error) = result {
                warn!(%error, "unable to reenable AXEnhancedUserInterface");
            }
        });
    }

    fn set_ax_position(&mut self, origin: Origin) -> Result<()> {
        let mut point = CGPoint::new(
            f64::from(origin.x + self.horizontal_padding),
            f64::from(origin.y + self.vertical_padding),
        );
        let position_ref = unsafe {
            AXValueCreate(
                kAXValueTypeCGPoint,
                NonNull::from(&mut point).as_ptr().cast(),
            )
        };
        let position = AXUIWrapper::from_retained(position_ref)?;
        super::ax_census::ax_write(
            &self.census_context(),
            "AXPosition",
            unsafe {
                AXUIElementSetAttributeValue(
                    self.ax_element.as_ptr(),
                    CFString::from_static_str(kAXPositionAttribute).as_ref(),
                    position.as_ref(),
                )
            }
            .to_result(function_name!()),
        )?;

        let size = self.frame.size();
        self.frame.min = origin;
        self.frame.max = origin + size;
        Ok(())
    }

    fn set_ax_size(&mut self, size: Size) -> Result<()> {
        let width_padding = 2 * self.horizontal_padding;
        let height_padding = 2 * self.vertical_padding;
        let mut cgsize = CGSize::new(
            f64::from(size.x - width_padding),
            f64::from(size.y - height_padding),
        );
        let size_ref = unsafe {
            AXValueCreate(
                kAXValueTypeCGSize,
                NonNull::from(&mut cgsize).as_ptr().cast(),
            )
        };
        let size_value = AXUIWrapper::from_retained(size_ref)?;
        super::ax_census::ax_write(
            &self.census_context(),
            "AXSize",
            unsafe {
                AXUIElementSetAttributeValue(
                    self.ax_element.as_ptr(),
                    CFString::from_static_str(kAXSizeAttribute).as_ref(),
                    size_value.as_ref(),
                )
            }
            .to_result(function_name!()),
        )?;

        self.frame.max = self.frame.min + size;
        Ok(())
    }

    /// Writes a complete target frame without trusting the cached frame to
    /// decide whether any AX call can be skipped.
    fn write_ax_frame(&mut self, target_frame: IRect, previous_frame: IRect) -> Result<()> {
        let size = target_frame.size();
        self.set_ax_size(size)?;

        let mut previous_observed_frame = previous_frame;
        let mut staged = false;
        for attempt in 1..=3 {
            let Ok(actual_frame) = self.update_frame() else {
                break;
            };
            let Some(staging_origin) =
                resize_staging_origin(previous_observed_frame, actual_frame, size.x)
            else {
                break;
            };
            debug!(
                attempt,
                requested_width = size.x,
                actual_width = actual_frame.width(),
                staging_x = staging_origin.x,
                "retrying partially constrained AX resize from an offscreen origin"
            );
            staged = true;
            previous_observed_frame = actual_frame;
            self.set_ax_position(staging_origin)?;
            self.set_ax_size(size)?;
        }

        if staged && let Ok(final_frame) = self.update_frame() {
            debug!(
                requested_width = size.x,
                actual_width = final_frame.width(),
                "completed staged AX resize"
            );
        }

        // AX commonly moves a window while changing its size. Reassert the
        // intended origin between two size writes, matching the sequence used
        // by mature macOS window managers.
        self.set_ax_position(target_frame.min)?;
        self.set_ax_size(size)
    }

    /// Always refreshes the frame cache after a geometry request. AX setters
    /// are not transactional, so a failed sequence may still have moved or
    /// resized the window. The original write error still reaches the caller,
    /// while the readback keeps subsequent geometry decisions synchronized.
    fn observe_geometry_write(
        &mut self,
        operation: &'static str,
        write_result: Result<()>,
    ) -> Result<IRect> {
        observe_geometry_write_result(operation, write_result, || self.update_frame())
    }

    /// Makes the window the key window for its application by sending synthesized events.
    ///
    /// # Arguments
    ///
    /// * `psn` - The process serial number of the application.
    fn make_key_window(&self, psn: &ProcessSerialNumber) -> Result<()> {
        // Reason: On macOS 14 (Sonoma), CGSEncodeEventRecord serializes the raw event
        // buffer via NSKeyedArchiver, misinterpreting 0xFF fill as an ObjC class pointer,
        // causing SIGABRT. See https://github.com/karinushka/paneru/issues/123
        if macos_major_version() == 14 {
            debug!("make_key_window: skipped on macOS 14 (Sonoma) to prevent crash");
            return Ok(());
        }
        let window_id = self.id();
        let mut event_bytes = [0u8; 0xf8];
        event_bytes[0x04] = 0xf8;
        event_bytes[0x3a] = 0x10;
        event_bytes[0x3c..0x40].copy_from_slice(&window_id.to_ne_bytes());
        event_bytes[0x20..0x30].fill(0xff);

        event_bytes[0x08] = 0x01;
        unsafe { SLPSPostEventRecordTo(psn, event_bytes.as_ptr().cast()) }
            .to_result(function_name!())?;

        event_bytes[0x08] = 0x02;
        unsafe { SLPSPostEventRecordTo(psn, event_bytes.as_ptr().cast()) }
            .to_result(function_name!())?;
        Ok(())
    }

    /// This window's direct children, each paired with its accessibility role.
    ///
    /// One read: [`WindowApi::represented_window_id`] picks the retained anchor
    /// out of these handles, so the child list is never enumerated twice.
    fn children_with_roles(&self) -> Result<Vec<(String, CFRetained<AXUIWrapper>)>> {
        let children = self
            .ax_element
            .get_attribute::<CFArray<AXUIWrapper>>(&CFString::from_static_str("AXChildren"))?;
        children
            .to_vec()
            .into_iter()
            .map(|child| child.role().map(|role| (role, child)))
            .collect()
    }
}

impl WindowApi for WindowOS {
    fn default_floating(&self) -> bool {
        self.default_floating
    }
    /// Returns the ID of the window.
    ///
    /// # Returns
    ///
    /// The window ID as `WinID`.
    fn id(&self) -> WinID {
        self.id
    }

    fn incarnation(&self) -> WindowIncarnation {
        ax_window_incarnation(&self.ax_element)
    }

    fn represented_window_id(&self) -> Result<WinID> {
        if self.presentation_anchor.get().is_none() {
            let children = self.children_with_roles();
            if let Err(error) = children.as_ref() {
                // Failing to enumerate this window's children is not evidence
                // that another window owns this element. Some applications
                // advertise `AXChildren` and then always fail the read
                // (Telegram 12.8 answers `kAXErrorFailure` in about a
                // millisecond), and treating that as an unknown control target
                // suspends every geometry commit for the window indefinitely.
                // Resolve to "no retained anchor" once, which also stops paying
                // the failing synchronous read on every commit attempt.
                warn!(
                    window_id = self.id,
                    %error,
                    "window child list unreadable; treating the element as its own control target"
                );
            }
            let anchor = direct_presentation_anchor(children)?;
            let _ = self.presentation_anchor.set(anchor);
        }
        let Some(Some(anchor)) = self.presentation_anchor.get() else {
            return Ok(self.id);
        };
        // Retain the chrome object, not a hash or a list of application tabs.
        // AppKit reparents this object when the independently visible AX root changes.
        let root = anchor.get_attribute::<AXUIWrapper>(&CFString::from_static_str("AXWindow"))?;
        let mut pid = 0;
        unsafe { accessibility_sys::AXUIElementGetPid(root.as_ptr(), &raw mut pid) }
            .to_result(function_name!())?;
        if pid != self.pid()? || root.role()? != "AXWindow" {
            return Err(Error::InvalidWindow);
        }
        ax_window_id(root.as_ptr())
    }

    /// Returns the current frame (`CGRect`) of the window.
    ///
    /// # Returns
    ///
    /// The window's frame as `CGRect`.
    fn frame(&self) -> IRect {
        self.frame
    }

    /// Returns the accessibility element of the window.
    ///
    /// # Returns
    ///
    /// A `CFRetained<AXUIWrapper>` representing the accessibility element.
    fn element(&self) -> Option<CFRetained<AXUIWrapper>> {
        Some(self.ax_element.clone())
    }

    /// Retrieves the title of the window.
    ///
    /// # Returns
    ///
    /// `Ok(String)` with the window title if successful, otherwise `Err(Error)`.
    fn title(&self) -> Result<String> {
        if let Some(cached) = self.title.force_read().clone() {
            return Ok(cached);
        }
        let title = super::ax_census::ax_read(
            &self.census_context(),
            "AXTitle",
            known_window_title(self.ax_element.title()),
        )?;
        *self.title.force_write() = Some(title.clone());
        Ok(title)
    }

    fn invalidate_title(&self) {
        self.title.force_write().take();
    }

    fn retained_title(&self) -> Option<String> {
        self.title.force_read().clone()
    }

    /// Returns true if the window has a child role.
    fn child_role(&self) -> Result<bool> {
        let role = self.role()?;
        Ok(["AXSheet", "AXDrawer"]
            .iter()
            .any(|axrole| axrole.eq(&role)))
    }

    /// Retrieves the role of the window (e.g., "`AXWindow`").
    ///
    /// # Returns
    ///
    /// `Ok(String)` with the window role if successful, otherwise `Err(Error)`.
    fn role(&self) -> Result<String> {
        super::ax_census::ax_read(&self.census_context(), "AXRole", self.ax_element.role())
    }

    /// Retrieves the subrole of the window (e.g., "`AXStandardWindow`").
    ///
    /// # Returns
    ///
    /// `Ok(String)` with the window subrole if successful, otherwise `Err(Error)`.
    fn subrole(&self) -> Result<String> {
        super::ax_census::ax_read(
            &self.census_context(),
            "AXSubrole",
            self.ax_element.subrole(),
        )
    }

    #[instrument(level = Level::DEBUG, ret)]
    fn is_minimized(&self) -> bool {
        self.ax_element.minimized().is_ok_and(|minimized| minimized)
    }

    fn is_full_screen(&self) -> bool {
        self.try_is_full_screen().unwrap_or(false)
    }

    fn try_is_full_screen(&self) -> Result<bool> {
        super::ax_census::ax_read(
            &self.census_context(),
            "AXFullScreen",
            self.ax_element.full_screen(),
        )
    }

    fn is_movable(&self) -> Result<bool> {
        super::ax_census::ax_settable(
            &self.census_context(),
            "AXPosition.settable",
            self.attribute_is_settable(kAXPositionAttribute),
        )
    }

    fn is_resizable(&self) -> Result<bool> {
        super::ax_census::ax_settable(
            &self.census_context(),
            "AXSize.settable",
            self.attribute_is_settable(kAXSizeAttribute),
        )
    }

    #[instrument(level = Level::TRACE)]
    fn reposition(&mut self, origin: Origin) -> Result<IRect> {
        let enhanced_ui_lease = self.disable_enhanced_ui();
        let write_result = self.set_ax_position(origin);
        let result = self.observe_geometry_write("reposition", write_result);
        Self::reenable_enhanced_ui(enhanced_ui_lease);
        result
    }

    #[instrument(level = Level::TRACE)]
    fn resize_preserving_origin(&mut self, target: IRect) -> Result<IRect> {
        let enhanced_ui_lease = self.disable_enhanced_ui();
        let write_result = self.set_ax_size(target.size());
        let mut result = self.observe_geometry_write("resize_preserving_origin", write_result);

        if result
            .as_ref()
            .is_ok_and(|observed| observed.min != target.min)
        {
            let write_result = self.set_ax_position(target.min);
            result = self.observe_geometry_write("restore_resize_origin", write_result);
        }

        Self::reenable_enhanced_ui(enhanced_ui_lease);
        result
    }

    #[instrument(level = Level::TRACE)]
    fn set_frame(&mut self, frame: IRect) -> Result<IRect> {
        let previous_frame = self.frame;
        let enhanced_ui_lease = self.disable_enhanced_ui();
        let write_result = self.write_ax_frame(frame, previous_frame);
        let result = self.observe_geometry_write("set_frame", write_result);
        Self::reenable_enhanced_ui(enhanced_ui_lease);
        result
    }

    /// Updates the internal `frame` of the window by querying its current position and size from the Accessibility API.
    /// It also updates the `width_ratio`.
    ///
    /// # Arguments
    ///
    /// * `display_bounds` - An optional `CGRect` representing the bounds of the display the window is on.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the frame is updated successfully, otherwise `Err(Error)`.
    fn update_frame(&mut self) -> Result<IRect> {
        let position = self
            .ax_element
            .get_attribute::<AXUIWrapper>(&CFString::from_static_str(kAXPositionAttribute))?;
        let size = self
            .ax_element
            .get_attribute::<AXUIWrapper>(&CFString::from_static_str(kAXSizeAttribute))?;

        let mut frame = CGRect::default();
        let (position_ok, size_ok) = unsafe {
            let position_ok = AXValueGetValue(
                position.as_ptr(),
                kAXValueTypeCGPoint,
                NonNull::from(&mut frame.origin).as_ptr().cast(),
            );
            let size_ok = AXValueGetValue(
                size.as_ptr(),
                kAXValueTypeCGSize,
                NonNull::from(&mut frame.size).as_ptr().cast(),
            );
            (position_ok, size_ok)
        };
        if !position_ok || !size_ok {
            return Err(Error::invalid_window(&format!(
                "unable to decode AX frame for window {} (position: {position_ok}, size: {size_ok})",
                self.id
            )));
        }
        // if (CGRectEqualToRect(new_frame, window->frame)) {
        //     debug("%s:DEBOUNCED %s %d\n", __FUNCTION__, window->application->name, window->id);
        // }
        self.frame = irect_from(frame);

        self.frame.min.x -= self.horizontal_padding;
        self.frame.min.y -= self.vertical_padding;
        self.frame.max.x += self.horizontal_padding;
        self.frame.max.y += self.vertical_padding;

        Ok(self.frame)
    }

    /// Focuses the window without raising it. This involves sending specific events to the process.
    ///
    /// # Arguments
    ///
    /// * `currently_focused` - A reference to the currently focused window.
    #[instrument(level = Level::DEBUG, skip(currently_focused))]
    fn focus_without_raise(
        &self,
        psn: ProcessSerialNumber,
        currently_focused: &Window,
        focused_psn: ProcessSerialNumber,
    ) -> Result<()> {
        let window_id = self.id();
        debug!("{window_id}");
        if focused_psn == psn {
            let mut event_bytes = [0u8; 0xf8];
            event_bytes[0x04] = 0xf8;
            event_bytes[0x08] = 0x0d;

            event_bytes[0x8a] = 0x02;
            event_bytes[0x3c..0x40].copy_from_slice(&currently_focused.id().to_ne_bytes());
            unsafe {
                SLPSPostEventRecordTo(&focused_psn, event_bytes.as_ptr().cast())
                    .to_result(function_name!())?;
            }

            // Artificially delay the activation. This is necessary because some
            // applications appear to be confused if both of the events appear instantaneously.
            thread::sleep(Duration::from_millis(20));

            event_bytes[0x8a] = 0x01;
            event_bytes[0x3c..0x40].copy_from_slice(&window_id.to_ne_bytes());
            unsafe {
                SLPSPostEventRecordTo(&psn, event_bytes.as_ptr().cast())
                    .to_result(function_name!())?;
            }
        }

        unsafe {
            _SLPSSetFrontProcessWithOptions(&psn, window_id, CPS_USER_GENERATED)
                .to_result(function_name!())?;
        }
        self.make_key_window(&psn)
    }

    /// Focuses the window and raises it to the front.
    #[instrument(level = Level::DEBUG)]
    fn focus_with_raise(&self, psn: ProcessSerialNumber) -> Result<()> {
        let window_id = self.id();
        unsafe {
            _SLPSSetFrontProcessWithOptions(&psn, window_id, CPS_USER_GENERATED)
                .to_result(function_name!())?;
        }
        self.make_key_window(&psn)?;
        let element_ref = self.ax_element.as_ptr();
        let action = CFString::from_static_str(kAXRaiseAction);
        unsafe { AXUIElementPerformAction(element_ref, &action) }.to_result(function_name!())
    }

    #[instrument(level = Level::DEBUG)]
    fn raise_without_focus(&self) {
        let element_ref = self.ax_element.as_ptr();
        let action = CFString::from_static_str(kAXRaiseAction);
        let _ = unsafe { AXUIElementPerformAction(element_ref, &action) }
            .to_result(function_name!())
            .inspect_err(|error| {
                warn!(window_id = self.id(), %error, "unable to raise window without focus");
            });
    }

    fn pid(&self) -> Result<Pid> {
        self.pid
            .get_or_init(|| {
                let pid: Pid = unsafe {
                    NonNull::new_unchecked(self.ax_element.as_ptr::<Pid>())
                        .byte_add(0x10)
                        .read()
                };
                (pid != 0).then_some(pid).ok_or(Error::InvalidInput(format!(
                    "can not get pid from {:?}.",
                    self.ax_element
                )))
            })
            .clone()
    }

    fn set_padding(&mut self, padding: WindowPadding) {
        match padding {
            WindowPadding::Vertical(padding) => self.vertical_padding = padding,
            WindowPadding::Horizontal(padding) => self.horizontal_padding = padding,
        }
    }

    fn horizontal_padding(&self) -> i32 {
        self.horizontal_padding
    }

    fn vertical_padding(&self) -> i32 {
        self.vertical_padding
    }

    // Based on:
    // - https://github.com/y3owk1n/rift/blob/cca067145f0282b532e848bb63d26a38c61f3c14/src/sys/window_server.rs#L175
    // - https://github.com/FelixKratz/JankyBorders/blob/a56a76a8a6ed77325f03655b23fcf525144d120b/src/windows.c#L67
    #[allow(clippy::cast_precision_loss)]
    fn border_radius(&self) -> Option<f64> {
        *self.border_radius.get_or_init(|| {
            let iterator = super::window_iterator_for_id(self.id)?;
            if !unsafe { SLSWindowIteratorAdvance(&raw const *iterator) } {
                return None;
            }

            let radii_ref = unsafe {
                // Load the function dynamicaly, because it exists only on macOS 26.x
                let s = c"SLSWindowIteratorGetCornerRadii";
                let p = libc::dlsym(libc::RTLD_DEFAULT, s.as_ptr());
                if p.is_null() {
                    return None;
                }
                let f: unsafe extern "C" fn(*const CFType) -> *mut CFArray<CFNumber> =
                    std::mem::transmute(p);
                f(&raw const *iterator)
            };
            let radii: CFRetained<CFArray<CFNumber>> =
                unsafe { CFRetained::from_raw(NonNull::new(radii_ref)?) };
            if radii.is_empty() {
                return None;
            }
            // Get first corner radius (usually all corners are the same)
            radii.get(0)?.as_i64().map(|v| v as f64)
        })
    }
}

/// Resolves a window's retained native chrome anchor from its enumerated direct
/// children.
///
/// `children` is the enumeration outcome, each child paired with its
/// accessibility role: `Err` means the list could not be read at all. An
/// unreadable list resolves to "no direct anchor" rather than an unknown control
/// target. A failing read is not evidence that another window owns the element,
/// and rejecting the window on it parked every geometry commit for as long as
/// the application kept failing the read (Telegram 12.8 advertises
/// `AXChildren` and always answers `kAXErrorFailure`). A window that enumerates
/// more than one direct `AXTabGroup` stays an ambiguity the caller rejects
/// instead of guessing.
fn direct_presentation_anchor<T>(children: Result<Vec<(String, T)>>) -> Result<Option<T>> {
    let Ok(children) = children else {
        return Ok(None);
    };
    let mut anchors = children
        .into_iter()
        .filter(|(role, _)| role == "AXTabGroup")
        .map(|(_, child)| child);
    match (anchors.next(), anchors.next()) {
        (None, _) => Ok(None),
        (Some(anchor), None) => Ok(Some(anchor)),
        (Some(_), Some(_)) => Err(Error::InvalidWindow),
    }
}

fn known_window_title(result: Result<String>) -> Result<String> {
    match result {
        Err(error)
            if error.macos_code().is_some_and(|code| {
                code == kAXErrorNoValue || code == kAXErrorAttributeUnsupported
            }) =>
        {
            Ok(String::new())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn an_unreadable_child_list_keeps_the_window_as_its_own_control_target() {
        // Telegram 12.8 advertises AXChildren and always fails the read. That is
        // not evidence that another window owns the element, so it must resolve
        // to "no anchor" instead of an unknown control target: rejecting it
        // suspends every geometry commit for the window indefinitely.
        assert_eq!(
            direct_presentation_anchor::<u8>(Err(Error::macos(
                "AXChildren",
                accessibility_sys::kAXErrorFailure
            )))
            .unwrap(),
            None
        );
        assert_eq!(
            direct_presentation_anchor(Ok(vec![("AXWindow".to_string(), 7u8)])).unwrap(),
            None
        );
        // Exactly one direct tab group keeps that chrome as the anchor.
        assert_eq!(
            direct_presentation_anchor(Ok(vec![
                ("AXWindow".to_string(), 7u8),
                ("AXTabGroup".to_string(), 8u8),
            ]))
            .unwrap(),
            Some(8)
        );
        // Two tab groups stay ambiguous: the caller must reject, not guess.
        assert!(
            direct_presentation_anchor(Ok(vec![
                ("AXTabGroup".to_string(), 8u8),
                ("AXTabGroup".to_string(), 9u8),
            ]))
            .is_err()
        );
    }

    #[test]
    fn window_policy_fallback_evidence_obeys_rules_and_skips_standard_windows() {
        let mut evidence = WindowEvidence {
            role: Some("AXWindow".into()),
            subrole: Some("Quick Look".into()),
            title: Some(String::new()),
            parent_role: Some("AXApplication".into()),
            ..Default::default()
        };
        let config = Config::default();
        assert!(evidence.needs_fallback(&config, Some("test")));
        assert_eq!(evidence.admission(&config, Some("test")), Admission::Defer);
        evidence.fallback = window_policy::FallbackEvidence {
            movable: None,
            resizable: None,
            geometry: Some(true),
            surface: Some(true),
            close_button: Some(true),
            minimize_button: Some(false),
        };
        assert_eq!(
            evidence.admission(&config, Some("test")),
            Admission::TrackFloating
        );
        for (track, expected) in [(false, Admission::Ignore), (true, Admission::Track)] {
            let config = Config::try_from(
                format!(r#"{{"windows":{{"rule":{{"track":{track}}}}}}}"#).as_str(),
            )
            .unwrap();
            assert_eq!(evidence.admission(&config, Some("test")), expected);
            assert!(!evidence.needs_fallback(&config, Some("test")));
        }
        // A standard window is admitted on its chrome, which the construction path
        // reads for every candidate; without any window evidence it is a surface.
        // A standard window is admitted on its chrome, which the construction path
        // reads for every candidate. While that evidence is unknown the decision
        // defers — and the fallback's geometry and surface are gathered with it —
        // rather than admitting a surface on its subrole alone.
        evidence.subrole = Some("AXStandardWindow".into());
        evidence.fallback = window_policy::FallbackEvidence::default();
        assert_eq!(evidence.admission(&config, Some("test")), Admission::Defer);
        assert!(evidence.needs_fallback(&config, Some("test")));
        evidence.fallback = window_policy::FallbackEvidence {
            close_button: Some(true),
            minimize_button: Some(false),
            ..Default::default()
        };
        assert_eq!(evidence.admission(&config, Some("test")), Admission::Track);
        assert!(!evidence.needs_fallback(&config, Some("test")));
    }

    #[test]
    fn window_policy_distinguishes_absent_titles_from_failed_title_reads() {
        for code in [kAXErrorNoValue, kAXErrorAttributeUnsupported] {
            assert_eq!(
                known_window_title(Err(Error::macos("AXTitle", code))).unwrap(),
                ""
            );
        }
        assert!(known_window_title(Err(Error::InvalidWindow)).is_err());
        assert_eq!(
            known_window_title(Ok("Settings".into())).unwrap(),
            "Settings"
        );
    }

    #[test]
    fn enhanced_ui_first_acquire_and_final_restore_are_atomic() {
        let states = Arc::new(Mutex::new(HashMap::new()));
        let probes = Arc::new(AtomicUsize::new(0));
        let restores = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(2));
        let acquired = Arc::new(Barrier::new(2));
        let key = EnhancedUiKey::new(42, 7);

        let handles = (0..2)
            .map(|_| {
                let states = Arc::clone(&states);
                let probes = Arc::clone(&probes);
                let restores = Arc::clone(&restores);
                let start = Arc::clone(&start);
                let acquired = Arc::clone(&acquired);
                std::thread::spawn(move || {
                    start.wait();
                    acquire_enhanced_ui_state(&states, key, || {
                        probes.fetch_add(1, Ordering::SeqCst);
                        true
                    });
                    acquired.wait();
                    release_enhanced_ui_state(&states, key, || {
                        restores.fetch_add(1, Ordering::SeqCst);
                    });
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(restores.load(Ordering::SeqCst), 1);
        assert!(states.lock().unwrap().is_empty());
    }

    #[test]
    fn enhanced_ui_state_distinguishes_pid_reuse_by_ax_incarnation() {
        let states = Mutex::new(HashMap::new());
        let probes = AtomicUsize::new(0);
        let restores = AtomicUsize::new(0);
        let old_application = EnhancedUiKey::new(42, 7);
        let new_application = EnhancedUiKey::new(42, 8);

        acquire_enhanced_ui_state(&states, old_application, || {
            probes.fetch_add(1, Ordering::SeqCst);
            true
        });
        acquire_enhanced_ui_state(&states, new_application, || {
            probes.fetch_add(1, Ordering::SeqCst);
            true
        });

        assert_ne!(old_application, new_application);
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(states.lock().unwrap().len(), 2);

        for key in [old_application, new_application] {
            release_enhanced_ui_state(&states, key, || {
                restores.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(restores.load(Ordering::SeqCst), 2);
        assert!(states.lock().unwrap().is_empty());
    }

    #[test]
    fn geometry_readback_refreshes_observation_but_preserves_write_error() {
        let observed = Cell::new(false);
        let actual_frame = IRect::new(10, 20, 310, 220);

        let result = observe_geometry_write_result(
            "test",
            Err(Error::Generic("setter failed".to_string())),
            || {
                observed.set(true);
                Ok(actual_frame)
            },
        );

        assert!(observed.get());
        assert!(matches!(
            result,
            Err(Error::Generic(ref message)) if message == "setter failed"
        ));
    }

    #[test]
    fn stages_partially_applied_width_growth() {
        let previous = IRect::new(-400, 40, 400, 640);
        let actual = IRect::new(-400, 40, 2416, 640);

        assert_eq!(
            resize_staging_origin(previous, actual, 4112),
            Some(Origin::new(-1696, 40))
        );

        let nearly_complete = IRect::new(-2056, 40, 2016, 640);
        assert_eq!(
            resize_staging_origin(actual, nearly_complete, 4112),
            Some(Origin::new(-2096, 40))
        );
    }

    #[test]
    fn does_not_stage_fixed_size_or_completed_resizes() {
        let fixed = IRect::new(0, 40, 230, 448);
        assert_eq!(resize_staging_origin(fixed, fixed, 4112), None);

        let previous = IRect::new(0, 40, 800, 640);
        let completed = IRect::new(0, 40, 4112, 640);
        assert_eq!(resize_staging_origin(previous, completed, 4112), None);
    }
}

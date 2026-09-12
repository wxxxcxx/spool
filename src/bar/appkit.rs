use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle,
    NSBezierPath, NSButton, NSButtonType, NSColor, NSCompositingOperation, NSEvent, NSFont,
    NSGraphicsContext, NSImage, NSImageScaling, NSImageSymbolConfiguration, NSLineBreakMode,
    NSMainMenuWindowLevel, NSMutableParagraphStyle, NSPanel, NSRunningApplication, NSScreen,
    NSStatusBar, NSTextAlignment, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
    ns_string,
};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CAShapeLayer,
    kCAMediaTimingFunctionEaseInEaseOut,
};
use spool_shared_types::commands::Action;
use tracing::warn;

use crate::events::EventSender;

use super::drag::{BarDrag, DropTarget};
use super::layout::{BarLayout, BarMetrics, BarSurface, ItemKind, PlacedItem, Rect};
use super::model::{BarDisplay, BarSnapshot};
use super::motion::{self, BarMotion, VisualItem};
use super::preferences::BarPreferences;
use super::toolbar;

const DRAG_RELEASE_GRACE: Duration = Duration::from_millis(250);

/// One breath, in seconds. Slow enough to read as breathing rather than as a
/// blink, which is what makes a hovered control feel alive.
const BREATH_PERIOD: f64 = 1.8;

/// A breath has to be slow enough to read as breathing rather than as a blink,
/// and short enough that nobody waits for it. Pinned at compile time so a tuning
/// pass cannot quietly leave the range.
const _: () = assert!(BREATH_PERIOD >= 1.0 && BREATH_PERIOD <= 3.0);

/// The shape of the Bar's own collapse.
///
/// The content keeps `BarMotion`'s shared 240ms ease-out; this only shapes the
/// chrome, which is the transition the eye reads as the Bar folding away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Every shape is kept: the prototype picks between them.
enum Morph {
    /// 240ms ease-out, the policy the rest of the Bar already follows.
    Ease,
    /// 320ms with a slight overshoot, the way an `AppKit` panel settles.
    Spring,
    /// 420ms smoothstep: no overshoot, a softer start and stop.
    Smooth,
}

/// The morph the prototype settled on.
const MORPH: Morph = Morph::Spring;

impl Morph {
    fn duration(self) -> f64 {
        match self {
            Self::Ease => motion::DURATION,
            Self::Spring => 0.32,
            Self::Smooth => 0.42,
        }
    }

    fn ease(self, t: f64) -> f64 {
        match self {
            Self::Ease => motion::ease_out(t),
            Self::Spring => motion::spring(t),
            Self::Smooth => motion::smooth(t),
        }
    }
}

/// Which part of the pulse is animated. The animation's key is its key path, so
/// adding and removing a pulse can never disagree about what to look for.
const PULSE_SHADOW: &str = "shadowOpacity";
const PULSE_SHADOW_RADIUS: &str = "shadowRadius";
const PULSE_OPACITY: &str = "opacity";
const PULSE_SCALE: &str = "transform.scale";
const PULSE_SCALE_Y: &str = "transform.scale.y";

/// How bright the collapsed Bar's halo pulses, low to high. The prototype's
/// amplitude dial moves these; both stay below opaque so a hovered Bar never
/// looks like a second, brighter Bar.
const GLOW_PULSE: (f64, f64) = (0.30, 0.75);
/// How far the halo's bloom swells, as a multiple of its resting radius.
///
/// On a notched display the collapsed capsule fills the whole band and has no
/// room to lift, so this *is* its breath; on a plain display it adds to the
/// tab's lift. The range was cut back after rendering both ends of it — a
/// full-opacity rim with a 15pt bloom reads as neon, which is the opposite of a
/// restful hint.
const GLOW_SWELL: (f64, f64) = (0.60, 1.35);
/// The bloom's radius at rest, per display kind.
const GLOW_RADIUS: (f64, f64) = (6.0, 9.0);
/// The same for the highlight under the pointer's toolbar button. It stays
/// quieter than the halo: a button is a control, the collapsed Bar is the only
/// thing left on screen.
const BUTTON_PULSE: (f64, f64) = (0.07, 0.17);

/// How a hovered control breathes.
///
/// `Pulse` lifts the halo vertically as well as brightening it, so the
/// collapsed Bar looks like it is drawing breath; `Bloom` only brightens it.
/// Both are Core Animation, so neither costs a main-thread frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Both mechanisms are kept: the prototype picks between them.
enum Breath {
    Bloom,
    Pulse,
}
/// The mechanism the prototype settled on.
const BREATH: Breath = Breath::Pulse;

/// Width of the collapse handle at either end of the Bar.
const END_ZONE: f64 = 24.0;

/// One end of the Bar's own chrome. While expanded these are the collapse
/// handles; while collapsed they are the expand handles.
///
/// The handles are SF Symbols rather than text glyphs: the angle-quotation
/// characters `U+2039`/`U+203A` are missing from some system fonts, which left
/// the handles silently invisible even though their hit zones worked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarEdge {
    Left,
    Right,
}

impl BarEdge {
    fn symbol(self) -> &'static str {
        match self {
            Self::Left => "chevron.left",
            Self::Right => "chevron.right",
        }
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent capability/style flags.
struct ViewState {
    display: BarDisplay,
    layout: BarLayout,
    motion: BarMotion,
    surface: BarSurface,
    space_scroll: HashMap<u64, f64>,
    can_focus_spaces: bool,
    can_move_windows: bool,
    pressed: Option<BarDrag>,
    release_observed_at: Option<Instant>,
    floating_order: HashMap<u64, Vec<i32>>,
    icons: HashMap<String, Retained<NSImage>>,
    preferences: BarPreferences,
    /// Runtime-only collapse state: every Bar starts expanded and nothing
    /// persists this.
    collapsed: bool,
    /// Whether the pointer is anywhere on the Bar. Both handles appear
    /// together once it is, which is how they are discovered.
    hovered: bool,
    /// Which end of the Bar chrome the pointer is over, if any.
    hover: Option<BarEdge>,
    /// Which toolbar button the pointer is over, if any.
    hover_button: Option<Action>,
    /// Set when the panel's own rect must be recomputed: collapse and hover
    /// change the panel, not just its content.
    chrome_dirty: bool,
    /// Eased motion of the panel rect between expanded and collapsed.
    chrome: ChromeMotion,
}

impl ViewState {
    fn drag_release_expired(&mut self, mouse_down: bool, now: Instant) -> bool {
        if mouse_down || !self.pressed.as_ref().is_some_and(|drag| drag.active) {
            self.release_observed_at = None;
            return false;
        }
        // Global button state can lead the queued mouseUp. This is only a
        // missing-event watchdog; normal releases must finish via mouseUp.
        let released = self.release_observed_at.get_or_insert(now);
        now.saturating_duration_since(*released) >= DRAG_RELEASE_GRACE
    }

    /// The whole menu-bar band, in viewport coordinates.
    fn panel_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: self.surface.width,
            height: self.layout.height,
        }
    }

    /// The rect the pointer must be inside to count as hovering the Bar: the
    /// whole band while expanded, the grown collapsed shape while collapsed.
    ///
    /// Hover is judged against the grown shape rather than the panel as it
    /// currently is, so a tab growing under the pointer cannot drop the pointer
    /// out of hover and flip the Bar back and forth every frame.
    fn hover_box(&self) -> Rect {
        let panel = self.panel_rect();
        if !self.collapsed {
            return panel;
        }
        super::placement::collapsed_hover_rect((panel.width, panel.height), self.surface.notch)
    }

    /// The outline the chrome is drawn with at `progress` (1 = expanded).
    ///
    /// One description feeds both the drawn shape and the halo, so they cannot
    /// drift apart. Collapsed on a notched display the top edge overhangs the
    /// body and the shoulders are scooped; the bottom corners round the
    /// ordinary way. Both flatten out as the Bar expands.
    fn chrome_shape(&self, progress: f64) -> ChromeShape {
        let corner = self.preferences.corner_radius.clamp(0.0, 20.0);
        if self.surface.notch.is_some() {
            ChromeShape {
                bottom: motion::lerp(super::placement::CAPSULE_BOTTOM_RADIUS, corner, progress),
                top: motion::lerp(super::placement::CAPSULE_TOP_RADIUS, 0.0, progress),
                overhang: true,
                shoulder: SHOULDER,
            }
        } else {
            // A plain display's tab is a pill: a narrow top edge too, but with
            // the ordinary rounded corners rather than scooped ones.
            let tab = super::placement::PLAIN_TAB_HEIGHT / 2.0;
            ChromeShape {
                bottom: motion::lerp(tab, corner, progress),
                top: motion::lerp(tab, 0.0, progress),
                overhang: false,
                shoulder: Shoulder::Fillet,
            }
        }
    }

    /// Where the Bar's own chrome is drawn, in viewport coordinates.
    ///
    /// Expanded it is the whole band, collapsed the capsule or the tab, and
    /// every frame in between is the eased position between the two. The panel
    /// itself never moves, which is what keeps the transition smooth and the
    /// shape's top edge pinned to the top of the screen.
    fn chrome_rect(&self) -> Rect {
        let expanded = self.panel_rect();
        let collapsed = super::placement::collapsed_rect(
            (expanded.width, expanded.height),
            self.surface.notch,
            self.hovered,
        );
        let progress = self.chrome.progress();
        Rect {
            x: motion::lerp(collapsed.x, expanded.x, progress),
            y: motion::lerp(collapsed.y, expanded.y, progress),
            width: motion::lerp(collapsed.width, expanded.width, progress),
            height: motion::lerp(collapsed.height, expanded.height, progress),
        }
    }

    /// Which end of the chrome a point grabs, if any. The handle is the whole
    /// height of that end, and a collapsed plain-screen tab is grabbable
    /// anywhere because it has no room for a separate handle.
    fn edge_at(&self, point: (f64, f64)) -> Option<BarEdge> {
        let bounds = self.hover_box();
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return None;
        }
        if point.0 < bounds.x
            || point.0 > bounds.x + bounds.width
            || point.1 < bounds.y
            || point.1 > bounds.y + bounds.height
        {
            return None;
        }
        let zone = END_ZONE.min(bounds.width / 2.0);
        if point.0 <= bounds.x + zone {
            return Some(BarEdge::Left);
        }
        if point.0 >= bounds.x + bounds.width - zone {
            return Some(BarEdge::Right);
        }
        if self.collapsed && self.surface.notch.is_none() {
            // The tab is only a few points tall; everything on it expands.
            return Some(BarEdge::Right);
        }
        None
    }

    fn set_collapsed(&mut self, collapsed: bool) {
        if self.collapsed == collapsed {
            return;
        }
        self.collapsed = collapsed;
        self.chrome.set_target(!collapsed, Instant::now());
        self.chrome_dirty = true;
    }

    fn take_chrome_dirty(&mut self) -> bool {
        std::mem::take(&mut self.chrome_dirty)
    }

    fn press_at(&mut self, point: NSPoint) -> Option<Action> {
        self.pressed = None;
        self.release_observed_at = None;
        if self.edge_at((point.x, point.y)).is_some() {
            // The handle toggles either way, but while expanded it only reacts
            // once it has been revealed: the hidden strip sits where the menu
            // bar's own controls used to be, and clicking an invisible button
            // must not collapse the Bar by accident.
            if self.collapsed || self.hovered {
                self.set_collapsed(!self.collapsed);
            }
            return None;
        }
        if self.collapsed {
            // The rest of the band belongs to the menu bar underneath while the
            // Bar is collapsed, so nothing else here is live.
            return None;
        }
        let hit_layout = self.motion.presented.interaction_layout(&self.layout);
        if let Some(item) = window_at(&hit_layout, point)
            && let ItemKind::Window { space_id, .. } = item.kind
            && self.space_is_visible(space_id)
        {
            self.pressed = BarDrag::begin(&item, &self.motion.presented, (point.x, point.y));
            return None;
        }
        space_at(&hit_layout, point)
            .filter(|_| self.can_focus_spaces)
            .map(|space_id| Action::FocusSpace { space_id })
    }

    fn relayout(&mut self) {
        let metrics = display_metrics(&self.display, &self.preferences);
        let preview = self
            .pressed
            .as_ref()
            .map(|drag| drag.preview_display(&self.display));
        self.layout = BarLayout::resolve_surface(
            preview.as_ref().unwrap_or(&self.display),
            self.surface,
            &mut self.space_scroll,
            metrics,
        );
        self.motion.retarget(&self.layout, Instant::now());
    }

    fn drag_to(&mut self, point: NSPoint, inside_view: bool) {
        self.release_observed_at = None;
        let hit_layout = self.motion.presented.interaction_layout(&self.layout);
        if let Some(drag) = &mut self.pressed {
            drag.move_pointer((point.x, point.y));
            drag.update_target(
                &self.display,
                &hit_layout,
                &self.motion.presented,
                self.can_move_windows && inside_view,
            );
        }
        self.relayout();
    }

    fn release_drag(&mut self, point: NSPoint, inside_view: bool) -> Option<Action> {
        self.drag_to(point, inside_view);
        let pressed = self.pressed.take()?;
        let allowed = pressed.is_valid(&self.display)
            && (!pressed.active || self.can_move_windows && pressed.target_valid(&self.display));
        let action = if allowed {
            if pressed.active
                && let Some(DropTarget::Floating { anchor, before }) = pressed.target
            {
                self.reorder_floating(pressed.space_id, pressed.window_id, anchor, before);
            }
            pressed.action()
        } else {
            None
        };
        self.relayout();
        action
    }

    fn sync_floating_order(&mut self) {
        for space in &mut self.display.spaces {
            let observed = space
                .floating
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>();
            let order = self.floating_order.entry(space.id).or_default();
            order.retain(|window_id| observed.contains(window_id));
            for window_id in observed {
                if !order.contains(&window_id) {
                    order.push(window_id);
                }
            }
            space.floating.sort_by_key(|window| {
                order
                    .iter()
                    .position(|window_id| *window_id == window.id)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    fn reorder_floating(&mut self, space_id: u64, window_id: i32, anchor: i32, before: bool) {
        let Some(order) = self.floating_order.get_mut(&space_id) else {
            return;
        };
        let Some(source) = order.iter().position(|candidate| *candidate == window_id) else {
            return;
        };
        order.remove(source);
        let Some(anchor) = order.iter().position(|candidate| *candidate == anchor) else {
            return;
        };
        order.insert(if before { anchor } else { anchor + 1 }, window_id);
        self.sync_floating_order();
        self.relayout();
    }

    fn space_is_visible(&self, space_id: u64) -> bool {
        self.display
            .spaces
            .iter()
            .any(|space| space.id == space_id && space.visible)
    }
}

#[derive(Debug)]
struct BarViewIvars {
    events: EventSender,
    state: RefCell<ViewState>,
    ghost: RefCell<Option<DragGhost>>,
    toolbar_buttons: RefCell<Vec<(Action, Retained<NSButton>)>>,
    /// A soft halo around the collapsed Bar. Core Animation owns its pulse: the
    /// render server animates it without waking the frame loop, which matters
    /// because running the whole ECS continuously costs ~45% of a core.
    glow: RefCell<Option<Retained<CAShapeLayer>>>,
    /// One hover highlight per toolbar button, sitting under the button's own
    /// view so the symbol draws on top of it.
    highlights: RefCell<Vec<Retained<CALayer>>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolWorkspaceBarView"]
    #[ivars = BarViewIvars]
    #[derive(Debug)]
    struct BarView;

    impl BarView {
        #[unsafe(method(openMissionControl:))]
        fn open_mission_control(&self, _sender: Option<&AnyObject>) {
            self.cancel_drag();
            self.dispatch(Action::MissionControl);
        }

        #[unsafe(method(showDesktop:))]
        fn show_desktop(&self, _sender: Option<&AnyObject>) {
            self.cancel_drag();
            self.dispatch(Action::ShowDesktop);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            self.draw_bar();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let action = self.ivars().state.borrow_mut().press_at(point);
            if let Some(action) = action {
                self.dispatch(action);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut state = self.ivars().state.borrow_mut();
            state.drag_to(point, contains(view_rect(self.bounds()), point));
            drop(state);
            self.sync_drag_preview();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut state = self.ivars().state.borrow_mut();
            let action = state.release_drag(point, contains(view_rect(self.bounds()), point));
            drop(state);
            self.sync_drag_preview();
            self.setNeedsDisplay(true);
            if let Some(action) = action {
                self.dispatch(action);
            }
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            self.cancel_drag();
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut state = self.ivars().state.borrow_mut();
            // Scrolling belongs to the Space under the pointer: a slot narrower
            // than its content scrolls inside itself, and every other Space
            // keeps its place.
            let hit_layout = state.motion.presented.interaction_layout(&state.layout);
            let Some(space_id) = space_at(&hit_layout, point) else {
                return;
            };
            let max_scroll = state.layout.max_space_scroll(space_id);
            if max_scroll <= 0.0 {
                return;
            }
            let delta = if event.scrollingDeltaX().abs() > event.scrollingDeltaY().abs() {
                event.scrollingDeltaX()
            } else {
                event.scrollingDeltaY()
            };
            let offset = state.space_scroll.entry(space_id).or_insert(0.0);
            *offset = (*offset + delta).clamp(0.0, max_scroll);
            state.relayout();
            drop(state);
            self.setNeedsDisplay(true);
        }
    }
);

impl BarView {
    fn install_toolbar(&self) {
        let mut buttons = self.ivars().toolbar_buttons.borrow_mut();
        for control in toolbar::buttons(self.bounds().size.height) {
            let tooltip = NSString::from_str(control.tooltip);
            let Some(symbol) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(control.symbol),
                Some(&tooltip),
            ) else {
                warn!(symbol = control.symbol, "Bar toolbar symbol unavailable");
                continue;
            };
            // Menu-bar metrics: configure the symbol's own point size and
            // weight instead of stretching one image into the whole button,
            // which is what made the old buttons look coarse.
            let point_size = (control.rect.width * 0.62).clamp(11.0, 16.0);
            let configuration = NSImageSymbolConfiguration::configurationWithPointSize_weight(
                point_size,
                // NSFontWeightMedium; the constant needs a feature we do not enable.
                0.23,
            );
            let image = symbol
                .imageWithSymbolConfiguration(&configuration)
                .unwrap_or(symbol);
            let action = match control.action {
                Action::MissionControl => sel!(openMissionControl:),
                Action::ShowDesktop => sel!(showDesktop:),
                _ => unreachable!("only system overview actions have toolbar buttons"),
            };
            // Both selectors are defined on this view; NSButton owns click tracking.
            let button = unsafe {
                NSButton::buttonWithImage_target_action(
                    &image,
                    Some(self),
                    Some(action),
                    self.mtm(),
                )
            };
            button.setButtonType(NSButtonType::MomentaryPushIn);
            button.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            button.setShowsBorderOnlyWhileMouseInside(true);
            button.setImageScaling(NSImageScaling::ScaleProportionallyDown);
            button.setContentTintColor(Some(&color(1.0, 1.0, 1.0, 0.92)));
            // The hover highlight is drawn by the Bar itself, so the button
            // contributes no bezel of its own.
            button.setBordered(false);
            button.setRefusesFirstResponder(true);
            button.setToolTip(Some(&tooltip));
            button.setFrame(ns_rect(control.rect));
            self.addSubview(&button);
            buttons.push((control.action, button));
        }
    }

    /// The collapsed Bar's hover halo.
    ///
    /// A shape layer carrying the collapsed outline twice over: a hairline
    /// rim, and the bloom its shadow casts from the same path. Both pulse
    /// together while the pointer is on the collapsed Bar.
    fn sync_glow(&self) {
        let (collapsed, hovered, rect, shape, notched) = {
            let state = self.ivars().state.borrow();
            let panel = state.panel_rect();
            let notched = state.surface.notch.is_some();
            let rect = super::placement::collapsed_rect(
                (panel.width, panel.height),
                state.surface.notch,
                state.hovered,
            );
            (
                state.chrome.progress() < 0.01,
                state.hovered,
                rect,
                state.chrome_shape(0.0),
                notched,
            )
        };
        let view_height = self.bounds().size.height;
        let parent = self.layer();
        // The layer spans the whole view so the shape's path — which is in
        // viewport coordinates, like everything else the Bar draws — lands
        // where it is drawn rather than an origin away from it.
        let Some(parent) = parent else {
            return;
        };
        if !parent.isGeometryFlipped() {
            // A flipped view flips its layer too, which is what the viewport
            // coordinates assume. Anything else needs the path mirrored, and a
            // mirrored halo is worse than none, so say so once and sit it out.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                warn!("Bar view layer is not geometry flipped; collapsed halo disabled");
            });
            if let Some(layer) = self.ivars().glow.borrow().as_ref() {
                settle(layer);
            }
            return;
        }
        let path = chrome_path(rect, shape);
        let mut glow = self.ivars().glow.borrow_mut();
        let layer = glow.get_or_insert_with(|| {
            let layer = CAShapeLayer::new();
            // Half-opaque white, so the pulse reads on a black shape without
            // ever looking like a second, brighter Bar.
            layer.setStrokeColor(Some(&color(1.0, 1.0, 1.0, 0.55).CGColor()));
            layer.setFillColor(Some(&NSColor::clearColor().CGColor()));
            layer.setLineWidth(1.2);
            layer.setShadowColor(Some(&NSColor::whiteColor().CGColor()));
            layer.setShadowOffset(NSSize::new(0.0, 0.0));
            layer.setShadowRadius(if notched {
                GLOW_RADIUS.1
            } else {
                GLOW_RADIUS.0
            });
            // The anchor is the band's top edge, which is also the shape's, so
            // a pulse grows downwards and never lifts the shape off the screen.
            layer.setAnchorPoint(NSPoint::new(0.5, 0.0));
            layer.setOpacity(0.0);
            parent.addSublayer(&layer);
            layer
        });
        layer.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), self.bounds().size));
        let cg_path = path.CGPath();
        layer.setPath(Some(&cg_path));
        layer.setShadowPath(Some(&cg_path));
        if collapsed && hovered {
            breathe(
                layer,
                PULSE_OPACITY,
                GLOW_PULSE.0,
                GLOW_PULSE.1,
                BREATH_PERIOD,
            );
            // The bloom swells at every breath, which is what the collapsed
            // capsule on a notched display has instead of room to lift.
            let resting = if notched {
                GLOW_RADIUS.1
            } else {
                GLOW_RADIUS.0
            };
            breathe(
                layer,
                PULSE_SHADOW_RADIUS,
                resting * GLOW_SWELL.0,
                resting * GLOW_SWELL.1,
                BREATH_PERIOD,
            );
            let reach = breath_reach(rect.height, view_height);
            if BREATH == Breath::Pulse && reach > 1.0 {
                breathe(layer, PULSE_SCALE_Y, 1.0, reach, BREATH_PERIOD);
            }
            layer.setOpacity(1.0);
        } else {
            settle(layer);
        }
    }

    /// Places the per-button hover highlights and breathes the one under the
    /// pointer. They are sublayers, so the button's own view still draws its
    /// symbol on top.
    fn sync_highlights(&self, controls: &[toolbar::ToolbarButton], origin: f64) {
        let hover_button = self.ivars().state.borrow().hover_button.clone();
        let view_height = self.bounds().size.height;
        let parent = self.layer();
        let mut highlights = self.ivars().highlights.borrow_mut();
        while highlights.len() < controls.len() {
            let layer = CALayer::new();
            layer.setBackgroundColor(Some(&NSColor::whiteColor().CGColor()));
            layer.setCornerRadius(6.0);
            layer.setOpacity(0.0);
            if let Some(parent) = &parent {
                parent.addSublayer(&layer);
            }
            highlights.push(layer);
        }
        for (index, layer) in highlights.iter().enumerate() {
            let Some(control) = controls.get(index) else {
                // A configuration change can take a button away; its highlight
                // must not be left behind.
                settle(layer);
                continue;
            };
            let rect = Rect {
                x: control.rect.x + origin,
                ..control.rect
            };
            if let Some(parent) = &parent {
                layer.setFrame(sublayer_rect(parent, view_height, rect));
            }
            if hover_button.as_ref() == Some(&control.action) {
                breathe(
                    layer,
                    PULSE_OPACITY,
                    BUTTON_PULSE.0,
                    BUTTON_PULSE.1,
                    BREATH_PERIOD,
                );
                breathe(layer, PULSE_SCALE, 1.0, 1.04, BREATH_PERIOD);
                layer.setOpacity(0.07);
            } else {
                settle(layer);
            }
        }
    }

    fn layout_toolbar(&self) {
        let (preferences, origin, collapsed) = {
            let state = self.ivars().state.borrow();
            (
                state.preferences.clone(),
                state.motion.presented.toolbar_origin,
                state.collapsed,
            )
        };
        let controls = toolbar::configured_buttons(
            self.bounds().size.height,
            preferences.show_mission_control,
            preferences.show_desktop,
        );
        let tint = foreground_color(&preferences, 1.0);
        for (action, button) in self.ivars().toolbar_buttons.borrow().iter() {
            let control = controls.iter().find(|control| *action == control.action);
            // Buttons are subviews, so a shrinking panel would leave them
            // floating outside it; the collapsed Bar has none.
            button.setHidden(control.is_none() || collapsed);
            if let Some(control) = control {
                // The buttons lead the Bar's group, so they move with it.
                let mut rect = control.rect;
                rect.x += origin;
                button.setFrame(ns_rect(rect));
                button.setContentTintColor(Some(&tint));
            }
        }
        self.sync_highlights(&controls, origin);
        self.sync_glow();
    }

    fn cancel_drag(&self) {
        let mut state = self.ivars().state.borrow_mut();
        state.pressed = None;
        state.relayout();
        drop(state);
        self.sync_drag_preview();
        self.setNeedsDisplay(true);
    }

    fn new(
        mtm: MainThreadMarker,
        events: EventSender,
        display: BarDisplay,
        surface: BarSurface,
        capabilities: BarCapabilities,
        preferences: BarPreferences,
    ) -> Retained<Self> {
        let metrics = display_metrics(&display, &preferences);
        let layout = BarLayout::resolve_surface(&display, surface, &mut HashMap::new(), metrics);
        let frame = ns_rect(Rect {
            width: layout.width,
            height: layout.height,
            ..Rect::default()
        });
        let this = Self::alloc(mtm).set_ivars(BarViewIvars {
            events,
            ghost: RefCell::new(None),
            toolbar_buttons: RefCell::new(Vec::new()),
            glow: RefCell::new(None),
            highlights: RefCell::new(Vec::new()),
            state: RefCell::new(ViewState {
                display,
                motion: BarMotion::new(&layout, Instant::now()),
                layout,
                surface,
                space_scroll: HashMap::new(),
                can_focus_spaces: capabilities.focus_spaces,
                can_move_windows: capabilities.move_windows,
                pressed: None,
                release_observed_at: None,
                floating_order: HashMap::new(),
                icons: HashMap::new(),
                preferences,
                collapsed: false,
                hovered: false,
                hover: None,
                hover_button: None,
                chrome_dirty: false,
                chrome: ChromeMotion::new(true, Instant::now()),
            }),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        this.install_toolbar();
        this
    }

    fn update(
        &self,
        display: BarDisplay,
        surface: BarSurface,
        capabilities: BarCapabilities,
        preferences: BarPreferences,
    ) {
        let mut state = self.ivars().state.borrow_mut();
        state.display = display;
        state.can_focus_spaces = capabilities.focus_spaces;
        state.can_move_windows = capabilities.move_windows;
        state.surface = surface;
        state.preferences = preferences;
        if state
            .pressed
            .as_ref()
            .is_some_and(|drag| !drag.is_valid(&state.display))
        {
            state.pressed = None;
        }
        if !capabilities.move_windows
            && let Some(drag) = &mut state.pressed
        {
            drag.target = None;
        }
        let target_missing = state
            .pressed
            .as_ref()
            .is_some_and(|drag| !drag.target_valid(&state.display));
        if target_missing && let Some(drag) = &mut state.pressed {
            drag.target = None;
        }
        state.sync_floating_order();
        state.relayout();
        drop(state);
        self.sync_drag_preview();
        self.setNeedsDisplay(true);
    }

    fn dispatch(&self, action: Action) {
        if let Err(error) = self.ivars().events.dispatch(action) {
            warn!(%error, "unable to dispatch Bar action");
        }
    }

    fn draw_bar(&self) {
        let bounds = self.bounds();
        let (hovered, hover, preferences, chrome_rect, progress, notched, split, shape) = {
            let state = self.ivars().state.borrow();
            (
                state.hovered,
                state.hover,
                state.preferences.clone(),
                state.chrome_rect(),
                state.chrome.progress(),
                state.surface.notch.is_some(),
                state.motion.presented.split.clone(),
                state.chrome_shape(state.chrome.progress()),
            )
        };
        clear(bounds);

        // One shape morphs from the menu-bar band to the collapsed capsule or
        // tab. Nothing here moves the window, so the blur behind it is only
        // ever recomputed at the panel's fixed size.
        let radius = shape.bottom;
        let path = chrome_path(chrome_rect, shape);
        // The expanded Bar's look is the blur behind it, and the default
        // background colour is deliberately transparent so that blur shows
        // through. The blur fades out as the Bar collapses, so the collapsed
        // shape has to be opaque in its own right — the black of the camera
        // housing it merges with — or nothing would be left on screen. This
        // fills black in proportion to how much of the blur has gone, then lays
        // the configured colour over it.
        let [red, green, blue, alpha] =
            BarPreferences::rgba(&preferences.background_color, [0.08, 0.08, 0.09, 0.88]);
        color(0.0, 0.0, 0.0, 1.0 - progress).setFill();
        path.fill();
        color(red, green, blue, alpha).setFill();
        path.fill();
        if preferences.border_width > 0.0 {
            let border = rgba(BarPreferences::rgba(
                &preferences.border_color,
                [1.0, 1.0, 1.0, 0.16],
            ));
            border.setStroke();
            path.setLineWidth(preferences.border_width.clamp(0.0, 4.0));
            path.stroke();
        }

        if progress > 0.01 {
            let (frame, drag) = {
                let state = self.ivars().state.borrow();
                (
                    state.motion.presented.clone(),
                    state
                        .pressed
                        .clone()
                        .filter(|drag| drag.active && state.can_move_windows),
                )
            };
            // The content wipes away with the shape rather than being switched
            // off, and stays inside it the whole way.
            NSGraphicsContext::saveGraphicsState_class();
            path.addClip();
            if let Some(split) = &split {
                clip_notch(bounds, split.gap);
            }
            set_alpha(progress.clamp(0.0, 1.0));
            self.draw_strip(&frame, drag.as_ref(), &preferences, bounds, radius);
            NSGraphicsContext::restoreGraphicsState_class();
        }

        if progress < 1.0 {
            // The collapsed capsule keeps an expand handle at each end; the
            // pointer only brightens one of them. Without a notch the tab has
            // no room for them and the whole tab is the target.
            if notched {
                Self::draw_handles(ns_rect(chrome_rect), hover, true, 1.0 - progress);
            }
        } else if hovered {
            // Both handles appear together as soon as the pointer is on the
            // Bar, so collapsing is discoverable without hunting for an edge.
            Self::draw_handles(bounds, hover, false, 1.0);
        }
    }

    fn draw_strip(
        &self,
        frame: &super::motion::Presentation,
        drag: Option<&BarDrag>,
        preferences: &BarPreferences,
        bounds: NSRect,
        radius: f64,
    ) {
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, radius, radius).addClip();
        // Scrolling and dragging belong only to the Space viewport, never the fixed buttons.
        NSBezierPath::bezierPathWithRect(ns_rect(Rect {
            x: frame.content_left,
            width: (frame.width - frame.content_left).max(0.0),
            height: frame.height,
            ..Rect::default()
        }))
        .addClip();
        for visual in frame
            .items
            .iter()
            .filter(|visual| matches!(visual.item.kind, ItemKind::Space { .. }))
        {
            self.draw_space(visual, frame.viewport(visual.space_id()));
        }
        // Keep the moving fill below icons and its indicator above opaque decks.
        for pass in 0..3 {
            for visual in &frame.items {
                if drag.is_some_and(|drag| drag.hides(visual)) {
                    continue;
                }
                if matches!(visual.item.kind, ItemKind::Space { .. })
                    || matches!(visual.item.kind, ItemKind::Focus { .. }) != (pass != 1)
                {
                    continue;
                }
                let viewport = frame.viewport(visual.space_id());
                if viewport.width <= 0.0 || viewport.height <= 0.0 {
                    continue;
                }
                NSGraphicsContext::saveGraphicsState_class();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    ns_rect(viewport),
                    4.0,
                    4.0,
                )
                .addClip();
                match visual.item.kind {
                    ItemKind::Window { .. } => self.draw_window(visual),
                    ItemKind::Label { ordinal, .. } => draw_label(ordinal, visual, preferences),
                    ItemKind::Placeholder { .. } => Self::draw_placeholder(visual),
                    ItemKind::Focus { .. } if pass == 0 => self.draw_focus(visual),
                    ItemKind::Focus { .. } => self.draw_focus_indicator(visual),
                    _ => {}
                }
                NSGraphicsContext::restoreGraphicsState_class();
            }
        }
        if let Some(drag) = drag
            && let Some(gap) = drag.gap_rect(frame)
        {
            let gap = gap.intersection(frame.viewport(drag.target_space()));
            if gap.width > 0.0 {
                rounded_fill(ns_rect(gap.inset(0.5)), 4.0, color(0.2, 0.65, 1.0, 0.12));
                rounded_stroke(
                    ns_rect(gap.inset(0.5)),
                    4.0,
                    1.0,
                    color(0.35, 0.72, 1.0, 0.7),
                );
            }
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }

    /// The collapse/expand handles at both ends of the Bar's chrome.
    ///
    /// `always` keeps them drawn without a pointer, which a collapsed capsule
    /// needs because it has no other affordance; the expanded Bar draws them
    /// only while it is hovered.
    fn draw_handles(bounds: NSRect, hover: Option<BarEdge>, always: bool, fade: f64) {
        // Square, so the symbol keeps its own aspect ratio, and centred in the
        // end zone that also holds its hit target.
        let side = (bounds.size.height - 4.0).clamp(8.0, 14.0);
        for edge in [BarEdge::Left, BarEdge::Right] {
            let inset = (END_ZONE - side) / 2.0;
            let rect = NSRect::new(
                NSPoint::new(
                    if edge == BarEdge::Left {
                        inset
                    } else {
                        bounds.size.width - END_ZONE + inset
                    },
                    (bounds.size.height - side) / 2.0,
                ),
                NSSize::new(side, side),
            );
            let opacity = if hover == Some(edge) {
                1.0
            } else if always {
                0.55
            } else {
                0.45
            };
            draw_symbol(edge.symbol(), rect, opacity * fade);
        }
    }

    fn draw_space(&self, visual: &VisualItem, viewport: Rect) {
        let ItemKind::Space { .. } = visual.item.kind else {
            return;
        };
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::bezierPathWithRect(ns_rect(viewport)).addClip();
        let preferences = self.ivars().state.borrow().preferences.clone();
        let active =
            BarPreferences::rgba(&preferences.active_workspace_color, [0.04, 0.52, 1.0, 0.2]);
        let rect = ns_rect(visual.item.space_background_rect());
        let radius = preferences.workspace_corner_radius.clamp(0.0, 8.0);
        let mut inactive =
            BarPreferences::rgba(&preferences.inactive_workspace_color, [0.5, 0.5, 0.5, 0.15]);
        inactive[3] *= visual.opacity;
        rounded_fill(rect, radius, rgba(inactive));
        let mut fill = active;
        fill[3] *= visual.emphasis * visual.opacity;
        rounded_fill(rect, radius, rgba(fill));
        NSGraphicsContext::restoreGraphicsState_class();
    }

    fn draw_window(&self, visual: &VisualItem) {
        let ItemKind::Window { bundle_id, .. } = &visual.item.kind else {
            return;
        };
        draw_window_visual(visual, self.icon(bundle_id).as_deref());
    }

    fn sync_drag_preview(&self) {
        let drag = {
            let state = self.ivars().state.borrow();
            state
                .pressed
                .clone()
                .filter(|drag| drag.active && state.can_move_windows)
        };
        let Some(drag) = drag else {
            self.ivars().ghost.borrow_mut().take();
            return;
        };
        let mut ghost = self.ivars().ghost.borrow_mut();
        if ghost.is_none() {
            let Some(screen) = self.window().and_then(|window| window.screen()) else {
                return;
            };
            *ghost = Some(DragGhost::new(self.mtm(), screen.frame()));
        }
        let Some(ghost) = ghost.as_ref() else {
            return;
        };
        let pointer = NSEvent::mouseLocation();
        let screen = ghost.screen;
        let (rect, items) = drag.ghost_geometry(
            (pointer.x, pointer.y),
            Rect {
                x: screen.origin.x,
                y: screen.origin.y,
                width: screen.size.width,
                height: screen.size.height,
            },
        );
        *ghost.view.ivars().borrow_mut() = items
            .into_iter()
            .map(|visual| {
                let icon = match &visual.item.kind {
                    ItemKind::Window { bundle_id, .. } => self.icon(bundle_id),
                    _ => None,
                };
                (visual, icon)
            })
            .collect();
        ghost
            .view
            .setFrameSize(NSSize::new(rect.width, rect.height));
        ghost.window.setFrame_display(ns_rect(rect), false);
        ghost.view.setNeedsDisplay(true);
        ghost.window.orderFrontRegardless();
    }

    fn draw_placeholder(visual: &VisualItem) {
        let rect = ns_rect(visual.item.rect.inset(2.0));
        rounded_stroke(rect, 4.0, 1.0, color(1.0, 1.0, 1.0, 0.22 * visual.opacity));
        draw_symbol(
            "rectangle.dashed",
            ns_rect(visual.item.rect.inset(5.0)),
            0.65 * visual.opacity,
        );
    }

    fn draw_focus(&self, visual: &VisualItem) {
        let preferences = self.ivars().state.borrow().preferences.clone();
        let mut selection =
            BarPreferences::rgba(&preferences.selection_color, [0.04, 0.52, 1.0, 1.0]);
        selection[3] *= visual.opacity;
        let mut fill = selection;
        fill[3] *= 0.35;
        let rect = ns_rect(visual.item.rect);
        rounded_fill(rect, 5.0, rgba(fill));
    }

    fn draw_focus_indicator(&self, visual: &VisualItem) {
        let preferences = self.ivars().state.borrow().preferences.clone();
        let mut selection =
            BarPreferences::rgba(&preferences.selection_color, [0.04, 0.52, 1.0, 1.0]);
        selection[3] *= visual.opacity;
        let rect = ns_rect(visual.item.rect);
        if preferences.show_focus_ring {
            let thickness = preferences.focus_ring_width.clamp(0.5, 4.0);
            rounded_stroke(
                ns_rect(visual.item.rect.inset(0.5)),
                4.0,
                1.0,
                rgba(selection),
            );
            let indicator = NSRect::new(
                NSPoint::new(
                    rect.origin.x + 4.0,
                    rect.origin.y + rect.size.height - thickness,
                ),
                NSSize::new((rect.size.width - 8.0).max(1.0), thickness),
            );
            rounded_fill(indicator, 1.0, rgba(selection));
        }
    }

    fn icon(&self, bundle_id: &str) -> Option<Retained<NSImage>> {
        if bundle_id.is_empty() {
            return None;
        }
        let mut state = self.ivars().state.borrow_mut();
        if let Some(icon) = state.icons.get(bundle_id) {
            return Some(icon.clone());
        }
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        let icon = apps.firstObject()?.icon()?;
        state.icons.insert(bundle_id.to_owned(), icon.clone());
        Some(icon)
    }
}

type GhostIcons = RefCell<Vec<(VisualItem, Option<Retained<NSImage>>)>>;

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolBarDragPreviewView"]
    #[ivars = GhostIcons]
    #[derive(Debug)]
    struct DragPreviewView;

    impl DragPreviewView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            clear(self.bounds());
            for (visual, icon) in self.ivars().borrow().iter() {
                draw_window_visual(visual, icon.as_deref());
            }
        }
    }
);

#[derive(Debug)]
struct DragGhost {
    window: Retained<NSPanel>,
    view: Retained<DragPreviewView>,
    screen: NSRect,
}

impl DragGhost {
    fn new(mtm: MainThreadMarker, screen: NSRect) -> Self {
        let view = DragPreviewView::alloc(mtm).set_ivars(RefCell::new(Vec::new()));
        let view: Retained<DragPreviewView> = unsafe {
            msg_send![super(view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0))]
        };
        let window = make_bar_window(mtm, &view);
        // Retained owns the preview lifetime; closing must not release it a second time.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setIgnoresMouseEvents(true);
        window.setLevel(NSMainMenuWindowLevel + 2);
        Self {
            window,
            view,
            screen,
        }
    }
}

impl Drop for DragGhost {
    fn drop(&mut self) {
        self.window.close();
    }
}

/// Which interactions this Bar's capabilities allow.
#[derive(Clone, Copy, Debug)]
struct BarCapabilities {
    focus_spaces: bool,
    move_windows: bool,
}

/// How far the panel is between expanded and collapsed, eased over the same
/// 240ms as Bar content motion.
///
/// The panel itself never moves: it always covers the menu-bar band, and this
/// is what the chrome morphs against. Moving and resizing a blurred window
/// every frame is what made the old transition stutter.
#[derive(Debug)]
struct ChromeMotion {
    /// 1 = expanded, 0 = collapsed.
    progress: f64,
    from: f64,
    to: f64,
    started: Instant,
    active: bool,
}

impl ChromeMotion {
    fn new(expanded: bool, now: Instant) -> Self {
        let progress = if expanded { 1.0 } else { 0.0 };
        Self {
            progress,
            from: progress,
            to: progress,
            started: now,
            active: false,
        }
    }

    fn progress(&self) -> f64 {
        self.progress
    }

    fn is_active(&self) -> bool {
        self.active
    }

    /// Starts moving toward the state `expanded` describes.
    ///
    /// An interrupted transition restarts from the frame on screen rather than
    /// from the other endpoint, so reversing mid-flight never jumps.
    fn set_target(&mut self, expanded: bool, now: Instant) {
        let target = if expanded { 1.0 } else { 0.0 };
        if (self.to - target).abs() < f64::EPSILON {
            return;
        }
        self.from = self.progress;
        self.to = target;
        self.started = now;
        self.active = true;
    }

    /// Advances the morph and returns whether the chrome still needs drawing.
    fn advance(&mut self, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.started).as_secs_f64() / MORPH.duration();
        if elapsed >= 1.0 {
            self.progress = self.to;
            self.active = false;
            return true;
        }
        self.progress = motion::lerp(self.from, self.to, MORPH.ease(elapsed));
        true
    }
}

/// How the top edge's overhang rounds into the walls below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Both are kept: this choice has flipped once already.
enum Shoulder {
    /// The ordinary convex fillet: the black rounds over the corner, so the
    /// overhang reads as a brim flowing into the body.
    Fillet,
    /// Scooped: the black is carved out of the corner instead, which leaves a
    /// small notch where the scoop meets the screen edge.
    Scoop,
}

/// The shoulder the prototype picked, rendered beside its alternative against a
/// photograph of the hardware.
const SHOULDER: Shoulder = Shoulder::Fillet;

/// The Bar's own outline: how far its corners round, and how the top ones meet
/// the screen edge.
///
/// Collapsed on a notched display the top edge *overhangs* the body — the black
/// is widest along the screen edge — and rounds back down into vertical walls;
/// the bottom corners round outwards too. Expanded, and on a plain display where
/// the tab is a pill, the top edge is inset instead.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChromeShape {
    bottom: f64,
    top: f64,
    /// Overhang the top edge past the body rather than insetting it.
    overhang: bool,
    shoulder: Shoulder,
}

/// The Bar's own outline, in viewport coordinates.
///
/// The Bar's own outline, in viewport coordinates.
///
/// Collapsed on a notched display this is the notch's silhouette: the black is
/// widest along the screen edge, a convex fillet carries each end of that
/// overhang back down to a vertical wall, and the bottom corners round outwards
/// too. The `MacBook`'s own hardware is the reference — the prototype renders
/// this beside a photograph of it.
///
/// A plain display's tab insets its top edge instead and rounds the same way,
/// so it is a pill; expanded, `top` reaches zero and the outline is the menu-bar
/// band itself, with square top corners.
fn chrome_path(rect: Rect, shape: ChromeShape) -> Retained<NSBezierPath> {
    // Circular-arc approximation for a cubic Bezier quadrant.
    const KAPPA: f64 = 0.552_284_749_8;
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let limit = (rect.width / 2.0).min(rect.height);
    let b = shape.bottom.clamp(0.0, limit);
    let t = shape.top.clamp(0.0, limit);
    let path = NSBezierPath::bezierPath();
    if t <= 0.0 {
        // Expanded: the Bar is the band, so its top corners are the screen's.
        path.moveToPoint(NSPoint::new(left, top));
        path.lineToPoint(NSPoint::new(right, top));
    } else if shape.overhang {
        // Collapsed: the top edge is the widest part and the shoulders carry it
        // back down to the walls. A fillet leaves the edge tangentially and
        // arrives at the wall tangentially, so there is no notch and no kink.
        path.moveToPoint(NSPoint::new(left - t, top));
        path.lineToPoint(NSPoint::new(right + t, top));
        let (c1, c2) = match shape.shoulder {
            Shoulder::Fillet => (
                NSPoint::new(right + t - t * KAPPA, top),
                NSPoint::new(right, top + t - t * KAPPA),
            ),
            Shoulder::Scoop => (
                NSPoint::new(right + t, top + t * KAPPA),
                NSPoint::new(right - t * KAPPA, top + t),
            ),
        };
        path.curveToPoint_controlPoint1_controlPoint2(NSPoint::new(right, top + t), c1, c2);
    } else {
        // A plain display's tab is a pill: an inset top edge, rounded the
        // ordinary way.
        path.moveToPoint(NSPoint::new(left + t, top));
        path.lineToPoint(NSPoint::new(right - t, top));
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(right, top + t),
            NSPoint::new(right - t + t * KAPPA, top),
            NSPoint::new(right, top + t - t * KAPPA),
        );
    }
    if b > 0.0 {
        path.lineToPoint(NSPoint::new(right, bottom - b));
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(right - b, bottom),
            NSPoint::new(right, bottom - b + b * KAPPA),
            NSPoint::new(right - b + b * KAPPA, bottom),
        );
        path.lineToPoint(NSPoint::new(left + b, bottom));
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(left, bottom - b),
            NSPoint::new(left + b - b * KAPPA, bottom),
            NSPoint::new(left, bottom - b + b * KAPPA),
        );
    } else {
        path.lineToPoint(NSPoint::new(right, bottom));
        path.lineToPoint(NSPoint::new(left, bottom));
    }
    path.lineToPoint(NSPoint::new(left, top + t));
    if t <= 0.0 {
        path.lineToPoint(NSPoint::new(left, top));
    } else if shape.overhang {
        let (c1, c2) = match shape.shoulder {
            Shoulder::Fillet => (
                NSPoint::new(left, top + t - t * KAPPA),
                NSPoint::new(left - t + t * KAPPA, top),
            ),
            Shoulder::Scoop => (
                NSPoint::new(left + t * KAPPA, top + t),
                NSPoint::new(left - t, top + t * KAPPA),
            ),
        };
        path.curveToPoint_controlPoint1_controlPoint2(NSPoint::new(left - t, top), c1, c2);
    } else {
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(left + t, top),
            NSPoint::new(left, top + t - t * KAPPA),
            NSPoint::new(left + t - t * KAPPA, top),
        );
    }
    path.closePath();
    path
}

/// Starts a repeating ease-in-out pulse on one animatable key, if it is not
/// already running.
///
/// The animation is the whole effect: nothing recomputes it per frame, and the
/// layer's model value stays at rest so removing the animation calms it.
fn breathe(layer: &CALayer, key_path: &str, from: f64, to: f64, period: f64) {
    let key = NSString::from_str(key_path);
    if unsafe { layer.animationForKey(&key) }.is_some() {
        return;
    }
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    unsafe {
        animation.setFromValue(Some(&NSNumber::new_f64(from)));
        animation.setToValue(Some(&NSNumber::new_f64(to)));
        animation.setDuration(period / 2.0);
        animation.setAutoreverses(true);
        animation.setRepeatCount(f32::INFINITY);
        animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(
            kCAMediaTimingFunctionEaseInEaseOut,
        )));
        layer.addAnimation_forKey(&animation, Some(&key));
    }
}

/// Stops every pulse on a layer and rests it.
fn settle(layer: &CALayer) {
    for key in [
        PULSE_SHADOW,
        PULSE_SHADOW_RADIUS,
        PULSE_OPACITY,
        PULSE_SCALE,
        PULSE_SCALE_Y,
    ] {
        layer.removeAnimationForKey(&NSString::from_str(key));
    }
    layer.setOpacity(0.0);
}

/// Maps a viewport rect into a sublayer's own coordinate space.
///
/// A flipped view's layer is normally flipped with it, but that is the layer's
/// business rather than the view's, so this asks instead of assuming.
fn sublayer_rect(parent: &CALayer, height: f64, rect: Rect) -> NSRect {
    if parent.isGeometryFlipped() {
        ns_rect(rect)
    } else {
        NSRect::new(
            NSPoint::new(rect.x, height - rect.y - rect.height),
            NSSize::new(rect.width, rect.height),
        )
    }
}

/// How far a halo of this height can stretch before it leaves the band.
///
/// A six-point tab has most of the menu bar to grow into; a capsule merged with
/// the notch fills the band already and can only brighten, not grow. A few
/// points is plenty either way: this is a breath, not a bounce.
fn breath_reach(height: f64, band: f64) -> f64 {
    let room = (band - height).clamp(0.0, 3.0);
    (1.0 + room / height.max(1.0)).clamp(1.0, 1.35)
}

/// Sets the alpha every later drawing operation is composited with.
fn set_alpha(alpha: f64) {
    if let Some(context) = objc2_app_kit::NSGraphicsContext::currentContext() {
        objc2_core_graphics::CGContext::set_alpha(Some(&context.CGContext()), alpha);
    }
}

struct PanelRecord {
    window: Retained<NSPanel>,
    view: Retained<BarView>,
    /// The menu-material blur behind the content. It fades out as the Bar
    /// collapses, but keeps its geometry: re-blurring a moving window every
    /// frame is what used to stutter.
    backdrop: Retained<NSVisualEffectView>,
    /// The panel: exactly the menu-bar band, and only ever that.
    expanded: Rect,
}

impl PanelRecord {
    /// Presents the current state: the panel keeps its rect, the chrome inside
    /// it morphs.
    fn present(&self) {
        let progress = self.view.ivars().state.borrow().chrome.progress();
        let rect = ns_rect(self.expanded);
        if self.window.frame() != rect {
            self.window.setFrame_display(rect, false);
        }
        // The blur fades out with the collapse; the content does not, or the
        // capsule would fade out with it.
        self.backdrop.setAlphaValue(progress);
        self.backdrop.setHidden(progress <= 0.01);
        self.view.layout_toolbar();
        self.view.setNeedsDisplay(true);
    }

    /// The menu bar underneath is only ours to block where the Bar is actually
    /// drawn. While collapsed the panel still covers the band, so it lets
    /// clicks through everywhere except the tab itself.
    fn set_interactive(&self, interactive: bool) {
        if self.window.ignoresMouseEvents() == interactive {
            self.window.setIgnoresMouseEvents(!interactive);
        }
    }
}

pub struct BarManager {
    mtm: MainThreadMarker,
    events: EventSender,
    panels: HashMap<u32, PanelRecord>,
    preferences: BarPreferences,
}

impl BarManager {
    pub fn new(mtm: MainThreadMarker, events: EventSender) -> Self {
        Self {
            mtm,
            events,
            panels: HashMap::new(),
            preferences: BarPreferences::default(),
        }
    }

    pub fn update(&mut self, snapshot: BarSnapshot, preferences: BarPreferences) {
        self.preferences = preferences;
        let screens = screens_by_id(self.mtm);
        let mut retained = HashSet::new();
        for display in snapshot.displays {
            let display_id = display.id;
            let Some(screen) = screens.get(&display.id) else {
                continue;
            };
            retained.insert(display.id);
            let capabilities = BarCapabilities {
                focus_spaces: snapshot.can_focus_spaces,
                move_windows: snapshot.can_move_windows,
            };
            let (expanded, surface, preferences) = screen_placement(screen, &self.preferences);
            let (window, view) = if let Some(record) = self.panels.get(&display.id) {
                (record.window.clone(), record.view.clone())
            } else {
                let view = BarView::new(
                    self.mtm,
                    self.events.clone(),
                    display.clone(),
                    surface,
                    capabilities,
                    preferences.clone(),
                );
                let (window, backdrop) = make_bar_panel(self.mtm, &view);
                self.panels.insert(
                    display.id,
                    PanelRecord {
                        window: window.clone(),
                        view: view.clone(),
                        backdrop,
                        expanded,
                    },
                );
                (window, view)
            };
            view.update(display, surface, capabilities, preferences.clone());
            window.setHasShadow(preferences.show_shadow);
            if let Some(record) = self.panels.get_mut(&display_id) {
                record.expanded = expanded;
                record.present();
            }
            window.orderFrontRegardless();
        }

        self.panels.retain(|display_id, record| {
            if retained.contains(display_id) {
                true
            } else {
                record.window.close();
                false
            }
        });
    }

    /// Collapses or expands the Bar on the display the user is working on.
    ///
    /// Runtime-only: nothing persists this and every Bar starts expanded.
    pub fn toggle_collapse(&mut self) {
        let Some(display_id) = self.active_display() else {
            return;
        };
        let Some(record) = self.panels.get_mut(&display_id) else {
            return;
        };
        {
            let mut state = record.view.ivars().state.borrow_mut();
            let collapsed = !state.collapsed;
            state.set_collapsed(collapsed);
        }
        record.present();
    }

    /// The display the active Space lives on, or the only Bar there is.
    fn active_display(&self) -> Option<u32> {
        self.panels
            .iter()
            .find(|(_, record)| record.view.ivars().state.borrow().display.active)
            .map(|(display_id, _)| *display_id)
            .or_else(|| self.panels.keys().copied().min())
    }

    /// Reveals the end handles under the pointer.
    ///
    /// Pointer position comes from the system rather than from tracking areas:
    /// the panel is a small strip above the menu bar and can be hovered
    /// without ever becoming the key window.
    fn update_hover(record: &PanelRecord) {
        let location = NSEvent::mouseLocation();
        let frame = record.window.frame();
        // The view is flipped and shares the panel's origin, so this is already
        // the viewport space the chrome is laid out in.
        let local = (
            location.x - frame.origin.x,
            frame.origin.y + frame.size.height - location.y,
        );
        let mut state = record.view.ivars().state.borrow_mut();
        // While collapsed this is the rect the tab grows into, not the panel,
        // so a panel that grows under the pointer cannot drop out of hover.
        let box_ = state.hover_box();
        let hovered = local.0 >= box_.x
            && local.1 >= box_.y
            && local.0 <= box_.x + box_.width
            && local.1 <= box_.y + box_.height
            && box_.width > 0.0;
        let hover = if hovered { state.edge_at(local) } else { None };
        // The toolbar buttons only exist for the expanded Bar.
        let hover_button = if hovered && !state.collapsed {
            let controls = toolbar::configured_buttons(
                state.layout.height,
                state.preferences.show_mission_control,
                state.preferences.show_desktop,
            );
            controls
                .iter()
                .find(|control| {
                    let rect = Rect {
                        x: control.rect.x + state.motion.presented.toolbar_origin,
                        ..control.rect
                    };
                    local.0 >= rect.x
                        && local.0 <= rect.x + rect.width
                        && local.1 >= rect.y
                        && local.1 <= rect.y + rect.height
                })
                .map(|control| control.action.clone())
        } else {
            None
        };
        if hovered != state.hovered || hover != state.hover || hover_button != state.hover_button {
            state.hovered = hovered;
            state.hover = hover;
            state.hover_button = hover_button;
            state.chrome_dirty = true;
        }
    }

    pub fn is_animating(&self) -> bool {
        self.panels.values().any(|record| {
            let state = record.view.ivars().state.borrow();
            state.motion.is_active()
                || state.chrome.is_active()
                || state.pressed.as_ref().is_some_and(|drag| drag.active)
        })
    }

    pub fn animate(&mut self) {
        let now = Instant::now();
        for record in self.panels.values() {
            Self::update_hover(record);
            let dragging = record
                .view
                .ivars()
                .state
                .borrow()
                .pressed
                .as_ref()
                .is_some_and(|drag| drag.active);
            if dragging {
                let release_expired = record
                    .view
                    .ivars()
                    .state
                    .borrow_mut()
                    .drag_release_expired(NSEvent::pressedMouseButtons() & 1 != 0, now);
                if release_expired {
                    record.view.cancel_drag();
                } else {
                    record.view.sync_drag_preview();
                }
            }
            let (changed, chrome_dirty, chrome_moving) = {
                let mut state = record.view.ivars().state.borrow_mut();
                (
                    state.motion.advance(now),
                    state.take_chrome_dirty(),
                    state.chrome.advance(now),
                )
            };
            if changed || chrome_dirty || chrome_moving {
                record.present();
            }
            // Only the collapsed tab is ours; the rest of the band stays the
            // menu bar's, so clicks there must reach it. The hover box is a
            // little larger than the tab, which clears the flag before the
            // pointer can reach it.
            let interactive = {
                let state = record.view.ivars().state.borrow();
                !state.collapsed || state.hovered
            };
            record.set_interactive(interactive);
        }
    }
}

impl Drop for BarManager {
    fn drop(&mut self) {
        for record in self.panels.values() {
            record.window.close();
        }
    }
}

fn clip_notch(bounds: NSRect, gap: Rect) {
    let path = NSBezierPath::bezierPath();
    path.appendBezierPathWithRect(ns_rect(Rect {
        width: gap.x,
        height: bounds.size.height,
        ..Rect::default()
    }));
    path.appendBezierPathWithRect(ns_rect(Rect {
        x: gap.x + gap.width,
        width: (bounds.size.width - gap.x - gap.width).max(0.0),
        height: bounds.size.height,
        ..Rect::default()
    }));
    path.addClip();
}

fn screen_placement(
    screen: &NSScreen,
    preferences: &BarPreferences,
) -> (Rect, BarSurface, BarPreferences) {
    let frame = view_rect(screen.frame());
    let menu_height = super::placement::menu_height(
        frame,
        view_rect(screen.visibleFrame()),
        screen.safeAreaInsets().top,
        NSStatusBar::systemStatusBar().thickness(),
    );
    let preferences = preferences.for_menu_height(menu_height);
    let panel = super::placement::panel_rect(frame, menu_height);
    let gap = super::placement::notch_gap(
        view_rect(screen.auxiliaryTopLeftArea()),
        view_rect(screen.auxiliaryTopRightArea()),
        menu_height,
    );
    let bias = preferences.notch_side;
    (
        panel,
        super::placement::surface(panel, gap, bias),
        preferences,
    )
}

/// A menu-material backdrop the content draws on top of, so the Bar blurs what
/// is behind it exactly the way the menu bar it replaces does.
fn make_backdrop(mtm: MainThreadMarker, size: NSSize) -> Retained<NSVisualEffectView> {
    let backdrop: Retained<NSVisualEffectView> = unsafe {
        msg_send![
            NSVisualEffectView::alloc(mtm),
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)
        ]
    };
    backdrop.setMaterial(NSVisualEffectMaterial::Menu);
    backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    // The menu bar always looks active, and this panel is never the key window.
    backdrop.setState(NSVisualEffectState::Active);
    backdrop
}

fn make_bar_window(mtm: MainThreadMarker, view: &NSView) -> Retained<NSPanel> {
    let size = view.frame().size;
    let window: Retained<NSPanel> = unsafe {
        msg_send![
            NSPanel::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), size),
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(true);
    window.setHidesOnDeactivate(false);
    window.setMovableByWindowBackground(false);
    window.setAcceptsMouseMovedEvents(true);
    window.setFloatingPanel(true);
    window.setBecomesKeyOnlyIfNeeded(true);
    window.setLevel(NSMainMenuWindowLevel + 1);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    window.setContentView(Some(view));
    unsafe { window.setReleasedWhenClosed(true) };
    window
}

/// The Bar's own panel: a Bar window whose content view is a menu-material
/// backdrop, with the content riding on top of it and following on resize.
/// The Bar's own panel: a transparent container holding the blur and the
/// content as *siblings*, blur first.
///
/// They cannot be nested, even though nesting is the obvious way to put content
/// on blur: the blur is faded out as the Bar collapses, and AppKit propagates a
/// view's alpha to its subviews, so a nested content view faded to nothing along
/// with it and the collapsed Bar became invisible.
fn make_bar_panel(
    mtm: MainThreadMarker,
    view: &NSView,
) -> (Retained<NSPanel>, Retained<NSVisualEffectView>) {
    let size = view.frame().size;
    let container: Retained<NSView> = unsafe {
        msg_send![
            NSView::alloc(mtm),
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)
        ]
    };
    let backdrop = make_backdrop(mtm, size);
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    view.setClipsToBounds(true);
    container.addSubview(&backdrop);
    container.addSubview(view);
    (make_bar_window(mtm, &container), backdrop)
}

fn screens_by_id(mtm: MainThreadMarker) -> HashMap<u32, Retained<NSScreen>> {
    NSScreen::screens(mtm)
        .iter()
        .filter_map(|screen| {
            let description = screen.deviceDescription();
            let numbers = unsafe { description.cast_unchecked::<NSString, NSNumber>() };
            let id = numbers.objectForKey(ns_string!("NSScreenNumber"))?.as_u32();
            Some((id, screen))
        })
        .collect()
}

fn window_at(layout: &BarLayout, point: NSPoint) -> Option<PlacedItem> {
    layout
        .items
        .iter()
        .rev()
        .find(|item| {
            matches!(item.kind, ItemKind::Window { .. })
                && contains(item.rect, point)
                && inside_space_slot(layout, item, point)
        })
        .cloned()
}

/// Icons scrolled out of their Space are clipped on screen, so a point in a
/// neighbouring slot must not hit them through their raw geometry.
fn inside_space_slot(layout: &BarLayout, item: &PlacedItem, point: NSPoint) -> bool {
    let space_id = item.kind.space_id();
    layout
        .items
        .iter()
        .find_map(|candidate| match candidate.kind {
            ItemKind::Space { space_id: id, .. } if id == space_id => Some(candidate.rect),
            _ => None,
        })
        .is_none_or(|slot| contains(slot, point))
}

fn space_at(layout: &BarLayout, point: NSPoint) -> Option<u64> {
    layout.items.iter().find_map(|item| match item.kind {
        ItemKind::Space { space_id, .. } if contains(item.rect, point) => Some(space_id),
        _ => None,
    })
}

fn contains(rect: Rect, point: NSPoint) -> bool {
    rect.contains(point.x, point.y)
}

fn draw_window_visual(visual: &VisualItem, icon: Option<&NSImage>) {
    let ItemKind::Window { floating, .. } = visual.item.kind else {
        return;
    };
    let rect = visual.item.rect;
    let decoration = visual.icon_decoration_opacity();
    if decoration > 0.0 {
        rounded_fill(
            ns_rect(rect.inset(0.5)),
            4.0,
            color(0.12, 0.12, 0.14, decoration),
        );
        rounded_stroke(
            ns_rect(rect.inset(0.5)),
            4.0,
            1.0,
            color(1.0, 1.0, 1.0, 0.45 * decoration),
        );
    }
    let image_rect = ns_rect(rect.inset(2.0));
    if let Some(icon) = icon {
        draw_image(icon, image_rect, visual.opacity);
    } else {
        draw_symbol("app", image_rect, visual.opacity);
    }
    if floating {
        let badge = NSRect::new(
            NSPoint::new(rect.x + rect.width - 7.0, rect.y + 1.0),
            NSSize::new(7.0, 7.0),
        );
        rounded_fill(badge, 2.0, color(0.12, 0.12, 0.14, visual.opacity));
        draw_symbol("square.on.square", badge, visual.opacity);
    }
    if let Some(badge) = visual.item.fullscreen_badge_rect() {
        rounded_fill(
            ns_rect(badge),
            2.0,
            color(0.12, 0.12, 0.14, 0.72 * visual.opacity),
        );
        draw_symbol(
            "arrow.up.left.and.arrow.down.right",
            ns_rect(badge.inset(1.0)),
            0.85 * visual.opacity,
        );
    }
}

fn ns_rect(rect: Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width, rect.height),
    )
}

fn view_rect(rect: NSRect) -> Rect {
    Rect {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

fn color(red: f64, green: f64, blue: f64, alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha)
}

fn rgba([red, green, blue, alpha]: [f64; 4]) -> Retained<NSColor> {
    color(red, green, blue, alpha)
}

fn foreground_color(preferences: &BarPreferences, opacity: f64) -> Retained<NSColor> {
    if preferences.foreground_color == "auto" {
        NSColor::labelColor().colorWithAlphaComponent(0.85 * opacity)
    } else {
        let mut value = BarPreferences::rgba(&preferences.foreground_color, [1.0, 1.0, 1.0, 0.85]);
        value[3] *= opacity;
        rgba(value)
    }
}

fn clear(rect: NSRect) {
    if let Some(context) = objc2_app_kit::NSGraphicsContext::currentContext() {
        context.setCompositingOperation(NSCompositingOperation::Clear);
        NSBezierPath::fillRect(rect);
        context.setCompositingOperation(NSCompositingOperation::SourceOver);
    }
}

fn rounded_fill(rect: NSRect, radius: f64, color: Retained<NSColor>) {
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
}

fn rounded_stroke(rect: NSRect, radius: f64, width: f64, color: Retained<NSColor>) {
    color.setStroke();
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius);
    path.setLineWidth(width);
    path.stroke();
}

fn attributed_text(
    text: &str,
    size: f64,
    color: Retained<NSColor>,
) -> Retained<NSAttributedString> {
    let font = NSFont::boldSystemFontOfSize(size);
    let font_key = NSString::from_str("NSFont");
    let color_key = NSString::from_str("NSColor");
    let paragraph_key = NSString::from_str("NSParagraphStyle");
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(NSTextAlignment::Center);
    paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    let text_attributes = NSDictionary::from_slices(
        &[&*font_key, &*color_key, &*paragraph_key],
        &[
            &*font as &AnyObject,
            &*color as &AnyObject,
            &*paragraph as &AnyObject,
        ],
    );
    let string = NSString::from_str(text);
    unsafe {
        msg_send![NSAttributedString::alloc(), initWithString: &*string, attributes: &*text_attributes]
    }
}

fn centered_text_rect(rect: NSRect, measured_height: f64) -> NSRect {
    let height = measured_height.min(rect.size.height);
    NSRect::new(
        NSPoint::new(
            rect.origin.x,
            rect.origin.y + (rect.size.height - height) / 2.0,
        ),
        NSSize::new(rect.size.width, height),
    )
}

fn draw_text(text: &str, rect: NSRect, size: f64, color: Retained<NSColor>) {
    let text = attributed_text(text, size.min((rect.size.height - 2.0).max(1.0)), color);
    NSGraphicsContext::saveGraphicsState_class();
    NSBezierPath::clipRect(rect);
    text.drawInRect(centered_text_rect(rect, text.size().height));
    NSGraphicsContext::restoreGraphicsState_class();
}

fn draw_label(ordinal: u32, visual: &VisualItem, preferences: &BarPreferences) {
    draw_text(
        &BarPreferences::workspace_label(ordinal),
        ns_rect(visual.item.rect),
        preferences.label_font_size.clamp(8.0, 20.0),
        foreground_color(preferences, visual.opacity),
    );
}

fn display_metrics(display: &BarDisplay, preferences: &BarPreferences) -> BarMetrics {
    let mut metrics = preferences.metrics();
    if preferences.show_workspace_labels {
        for space in &display.spaces {
            let text = attributed_text(
                &BarPreferences::workspace_label(space.ordinal),
                preferences.label_font_size.clamp(8.0, 20.0),
                NSColor::whiteColor(),
            );
            let measured = (text.size().width + 12.0).clamp(24.0, 120.0);
            metrics.label_width = metrics
                .label_width
                .max(measured)
                .max(preferences.workspace_label_width(space.ordinal));
        }
    }
    metrics
}

fn draw_image(image: &NSImage, rect: NSRect, opacity: f64) {
    unsafe {
        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            rect,
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            opacity,
            true,
            None,
        );
    }
}

fn draw_symbol(name: &str, rect: NSRect, opacity: f64) {
    if let Some(image) =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
    {
        let palette = NSArray::from_slice(&[&*NSColor::whiteColor()]);
        let config = NSImageSymbolConfiguration::configurationWithPaletteColors(&palette);
        let tinted = image.imageWithSymbolConfiguration(&config).unwrap_or(image);
        draw_image(&tinted, rect, opacity);
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout::BarAlign;
    use super::super::preferences::NotchSide;
    use super::*;

    #[test]
    fn space_blank_padding_is_clickable_throughout_animation() {
        let display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 1200.0);
        let mut motion = BarMotion::new(&initial, now);
        let mut expanded = display.clone();
        expanded.spaces[2].visible = true;
        let target = BarLayout::resolve(&expanded, 1200.0);
        motion.retarget(&target, now);
        for millis in [0, 30, 120, 240] {
            motion.advance(now + std::time::Duration::from_millis(millis));
            let frame = &motion.presented;
            let hits = frame.interaction_layout(&target);
            for space in &display.spaces {
                let rect = frame.space_rect(space.id).unwrap();
                for y in [1.0, frame.height - 1.0] {
                    let point = NSPoint::new(rect.x + rect.width / 2.0, y);
                    assert!(window_at(&hits, point).is_none());
                    assert_eq!(
                        space_at(&hits, point),
                        Some(space.id),
                        "Space {} blank padding at y={y} must be clickable",
                        space.id
                    );
                }
            }
        }
    }

    #[test]
    fn space_background_defaults_are_visible_and_distinct() {
        let prefs = BarPreferences::default();
        let active = BarPreferences::rgba(&prefs.active_workspace_color, [0.0; 4]);
        let inactive = BarPreferences::rgba(&prefs.inactive_workspace_color, [0.0; 4]);
        assert!(active[3] > 0.0);
        assert!(
            inactive[3] > 0.0,
            "inactive Spaces must have a visible background"
        );
        assert!(
            active
                .into_iter()
                .zip(inactive)
                .any(|(a, b)| (a - b).abs() > 0.05)
        );
    }

    #[test]
    fn space_background_leaves_menu_bar_gaps_without_shrinking_click_regions() {
        let display = crate::bar::layout::tests::display();
        for height in [22.0, 24.0, 37.0] {
            let prefs = BarPreferences::default().for_menu_height(height);
            let layout = BarLayout::resolve_with_metrics(
                &display,
                1200.0,
                &mut HashMap::new(),
                prefs.metrics(),
                BarAlign::Center,
            );
            let motion = BarMotion::new(&layout, Instant::now());
            let hits = motion.presented.interaction_layout(&layout);
            for item in &layout.items {
                let ItemKind::Space { space_id, .. } = item.kind else {
                    continue;
                };
                let background = item.space_background_rect();
                assert!((background.y - 3.0).abs() < f64::EPSILON);
                assert!((height - background.y - background.height - 3.0).abs() < f64::EPSILON);
                for y in [1.0, height - 1.0] {
                    let point = NSPoint::new(item.rect.x + item.rect.width / 2.0, y);
                    assert!(!background.contains(point.x, point.y));
                    assert_eq!(space_at(&hits, point), Some(space_id));
                }
            }
            assert!(
                BarPreferences::rgba(&prefs.background_color, [1.0; 4])[3].abs() < f64::EPSILON
            );
        }
    }

    #[test]
    fn blank_space_clicks_dispatch_focus_without_changing_window_clicks() {
        let (mut state, _) = drag_state();
        for space_id in [10, 11, 12] {
            let rect = state.motion.presented.space_rect(space_id).unwrap();
            for y in [1.0, state.layout.height - 1.0] {
                let point = NSPoint::new(rect.x + rect.width / 2.0, y);
                assert_eq!(state.press_at(point), Some(Action::FocusSpace { space_id }));
                assert!(state.pressed.is_none());
                state.can_focus_spaces = false;
                assert!(state.press_at(point).is_none());
                state.can_focus_spaces = true;
            }
        }
        let icon = state
            .layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap()
            .rect;
        let point = NSPoint::new(icon.x + 2.0, icon.y + 2.0);
        assert!(state.press_at(point).is_none());
        assert_eq!(state.pressed.as_ref().unwrap().window_id, 1);
        assert_eq!(
            state.release_drag(point, true),
            Some(Action::FocusWindow { window_id: 1 })
        );
    }

    fn drag_state() -> (ViewState, NSPoint) {
        let display = crate::bar::layout::tests::display();
        // No AppKit font calls: this exercises the real event-state path offscreen.
        let preferences = BarPreferences {
            show_workspace_labels: false,
            ..BarPreferences::default()
        };
        let layout = BarLayout::resolve_with_metrics(
            &display,
            1200.0,
            &mut HashMap::new(),
            preferences.metrics(),
            BarAlign::Center,
        );
        let motion = BarMotion::new(&layout, Instant::now());
        let item = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap();
        let pressed = BarDrag::begin(
            item,
            &motion.presented,
            (item.rect.x + 2.0, item.rect.y + 2.0),
        );
        let target = layout
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    ItemKind::ColumnDrop {
                        anchor_window_id: 3,
                        placement: spool_shared_types::commands::Placement::After,
                        ..
                    }
                )
            })
            .unwrap();
        let point = NSPoint::new(target.rect.x + target.rect.width / 2.0, target.rect.y + 8.0);
        (
            ViewState {
                display,
                layout,
                motion,
                surface: BarSurface {
                    width: 1200.0,
                    notch: None,
                    bias: NotchSide::Balanced,
                },
                space_scroll: HashMap::new(),
                can_focus_spaces: true,
                can_move_windows: true,
                pressed,
                release_observed_at: None,
                floating_order: HashMap::new(),
                icons: HashMap::new(),
                preferences,
                collapsed: false,
                hovered: false,
                hover: None,
                hover_button: None,
                chrome_dirty: false,
                chrome: ChromeMotion::new(true, Instant::now()),
            },
            point,
        )
    }

    #[test]
    fn mouse_drag_path_animates_preview_and_only_returns_command_on_release() {
        let (mut state, point) = drag_state();
        let original = state.display.clone();
        let original_layout = state.layout.clone();
        state.drag_to(point, true);
        assert_ne!(state.layout, original_layout);
        assert!(state.motion.is_active());
        assert_eq!(state.display, original);
        state
            .motion
            .advance(Instant::now() + std::time::Duration::from_secs(1));
        assert!(
            state
                .pressed
                .as_ref()
                .unwrap()
                .gap_rect(&state.motion.presented)
                .is_some()
        );
        assert!(matches!(
            state.release_drag(point, true),
            Some(Action::ReorderColumn {
                window_id: 1,
                anchor_window_id: 3,
                ..
            })
        ));
        assert!(state.pressed.is_none());
        assert_eq!(state.display, original);
        assert_eq!(state.layout, original_layout);
    }

    #[test]
    fn drag_survives_button_release_before_the_native_mouse_up_arrives() {
        for cross_space in [false, true] {
            let (mut state, reorder_point) = drag_state();
            let source = state
                .layout
                .items
                .iter()
                .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
                .unwrap()
                .rect;
            state.press_at(NSPoint::new(source.x + 2.0, source.y + 2.0));
            let point = if cross_space {
                let target = state.motion.presented.space_rect(10).unwrap();
                NSPoint::new(target.x + target.width / 2.0, target.y + 2.0)
            } else {
                reorder_point
            };
            state.drag_to(point, true);
            assert!(state.pressed.as_ref().unwrap().target.is_some());
            let now = Instant::now();
            for millis in [0, 16, 100, 249] {
                assert!(
                    !state.drag_release_expired(false, now + Duration::from_millis(millis)),
                    "global button state must not cancel a native mouseUp that has not arrived yet"
                );
            }
            let expected = if cross_space {
                Action::MoveColumnToSpace {
                    window_id: 1,
                    space_id: 10,
                    move_focus: spool_shared_types::commands::MoveFocus::Follow,
                }
            } else {
                Action::ReorderColumn {
                    window_id: 1,
                    anchor_window_id: 3,
                    placement: spool_shared_types::commands::Placement::After,
                }
            };
            assert_eq!(state.release_drag(point, true), Some(expected));
            assert!(
                state.release_drag(point, true).is_none(),
                "release dispatches only once"
            );
        }
    }

    #[test]
    fn missing_mouse_up_expires_but_native_pointer_events_reset_the_watchdog() {
        let (mut state, point) = drag_state();
        state.drag_to(point, true);
        let now = Instant::now();
        assert!(!state.drag_release_expired(false, now));
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(249)));
        assert!(state.drag_release_expired(false, now + DRAG_RELEASE_GRACE));

        assert!(!state.drag_release_expired(true, now + Duration::from_millis(300)));
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(600)));
        state.drag_to(point, true);
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(900)));

        let source = state
            .motion
            .presented
            .items
            .iter()
            .find(|item| matches!(item.item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap()
            .item
            .rect;
        state.press_at(NSPoint::new(source.x + 2.0, source.y + 2.0));
        state.drag_to(point, true);
        assert!(!state.drag_release_expired(false, now + Duration::from_secs(2)));
    }

    #[test]
    fn toolbar_has_no_space_or_window_hit_targets_during_scroll_animation() {
        let display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 150.0);
        let target = BarLayout::resolve(&display, 150.0);
        let mut motion = BarMotion::new(&initial, now);
        motion.retarget(&target, now);
        for millis in [0, 30, 120, 240] {
            motion.advance(now + std::time::Duration::from_millis(millis));
            let frame = &motion.presented;
            let hits = frame.interaction_layout(&target);
            for button in toolbar::buttons(frame.height) {
                let point = NSPoint::new(
                    button.rect.x + button.rect.width / 2.0,
                    button.rect.y + button.rect.height / 2.0,
                );
                assert!(window_at(&hits, point).is_none());
                assert!(space_at(&hits, point).is_none());
            }
            for hit in hits.items.iter().filter(|item| item.rect.width > 0.0) {
                assert!(hit.rect.x >= frame.content_left);
            }
        }
    }

    #[test]
    fn notch_clips_each_space_and_has_no_hits_during_scroll_or_rebalance() {
        let mut display = crate::bar::layout::tests::display();
        let surface = BarSurface {
            width: 520.0,
            notch: Some(Rect {
                x: 180.0,
                y: 0.0,
                width: 140.0,
                height: 34.0,
            }),
            bias: NotchSide::Balanced,
        };
        let now = Instant::now();
        let initial = BarLayout::resolve_surface(
            &display,
            surface,
            &mut HashMap::new(),
            BarMetrics::default(),
        );
        let mut motion = BarMotion::new(&initial, now);
        // A Space on each side of the notch scrolls inside its own slot; the
        // notch itself stays dead either way.
        let left = initial
            .spans
            .iter()
            .find(|span| span.rect.x < 180.0)
            .map(|span| span.space_id);
        let right = initial
            .spans
            .iter()
            .rev()
            .find(|span| span.rect.x > 320.0)
            .map(|span| span.space_id);
        for scrolled in [left, right, None] {
            let mut scroll = HashMap::new();
            if let Some(space_id) = scrolled {
                scroll.insert(space_id, f64::MAX);
            }
            let target =
                BarLayout::resolve_surface(&display, surface, &mut scroll, BarMetrics::default());
            motion.retarget(&target, now);
            for millis in [0, 30, 120, 240] {
                motion.advance(now + std::time::Duration::from_millis(millis));
                let frame = &motion.presented;
                let hits = frame.interaction_layout(&target);
                for x in [181.0, 250.0, 319.0] {
                    assert!(!frame.over_space_strip((x, 17.0)));
                    assert!(window_at(&hits, NSPoint::new(x, 17.0)).is_none());
                    assert!(space_at(&hits, NSPoint::new(x, 17.0)).is_none());
                }
                for visual in &frame.items {
                    let viewport = frame.viewport(visual.space_id());
                    let lane = frame.split.as_ref().unwrap().viewport(
                        visual.space_id(),
                        frame.width,
                        frame.height,
                        frame.content_left,
                    );
                    if viewport.width > 0.0 {
                        assert_eq!(viewport, viewport.intersection(lane));
                    }
                }
            }
        }
        display.spaces.remove(0);
        let target = BarLayout::resolve_surface(
            &display,
            surface,
            &mut HashMap::new(),
            BarMetrics::default(),
        );
        motion.retarget(&target, now);
        assert!(
            !motion.is_active(),
            "membership changes must not animate across the notch"
        );
        assert_eq!(motion.presented.split, target.split);
    }

    #[test]
    fn releasing_over_notch_cancels_an_existing_drop_preview() {
        let (mut state, _) = drag_state();
        state.surface = BarSurface {
            width: 1000.0,
            notch: Some(Rect {
                x: 440.0,
                y: 0.0,
                width: 120.0,
                height: state.layout.height,
            }),
            bias: NotchSide::Balanced,
        };
        state.relayout();
        let target = state
            .layout
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    ItemKind::ColumnDrop {
                        anchor_window_id: 3,
                        placement: spool_shared_types::commands::Placement::After,
                        ..
                    }
                )
            })
            .unwrap()
            .rect;
        // Activate the gesture before aiming at the drop slot: the slot can sit
        // inside the 4pt drag threshold of the grab point, and `active` is
        // sticky once set, so moving away first is enough.
        state.drag_to(NSPoint::new(900.0, target.y + 2.0), true);
        state.drag_to(
            NSPoint::new(target.x + target.width / 2.0, target.y + 2.0),
            true,
        );
        assert!(state.pressed.as_ref().unwrap().target.is_some());
    }

    #[test]
    fn releasing_a_window_over_either_toolbar_button_cancels_the_drag() {
        for button in toolbar::buttons(34.0) {
            let (mut state, target) = drag_state();
            let original = state.display.clone();
            state.drag_to(target, true);
            assert!(state.pressed.as_ref().unwrap().target.is_some());
            let point = NSPoint::new(
                button.rect.x + button.rect.width / 2.0,
                button.rect.y + button.rect.height / 2.0,
            );
            assert!(state.release_drag(point, true).is_none());
            assert!(state.pressed.is_none());
            assert_eq!(state.display, original);
        }
    }

    #[test]
    fn release_clears_preview_on_outside_drop_capability_loss_or_stale_source() {
        for scenario in 0..3 {
            let (mut state, point) = drag_state();
            state.drag_to(point, true);
            match scenario {
                1 => state.can_move_windows = false,
                2 => {
                    state.display.spaces[1].columns.remove(0);
                }
                _ => {}
            }
            assert!(state.release_drag(point, scenario != 0).is_none());
            assert!(state.pressed.is_none());
            let mut scroll = state.space_scroll.clone();
            assert_eq!(
                state.layout,
                BarLayout::resolve_with_metrics(
                    &state.display,
                    1200.0,
                    &mut scroll,
                    state.preferences.metrics(),
                    BarAlign::Center,
                )
            );
        }
    }

    #[test]
    fn scrolled_icons_are_not_hittable_outside_their_space() {
        let mut display = crate::bar::layout::tests::display();
        let template = display.spaces[1].floating[0].clone();
        for id in 20..40 {
            let mut window = template.clone();
            window.id = id;
            display.spaces[1].floating.push(window);
        }
        let mut scroll = HashMap::new();
        scroll.insert(11, 60.0);
        let layout = BarLayout::resolve_with_metrics(
            &display,
            700.0,
            &mut scroll,
            BarMetrics::default(),
            BarAlign::Center,
        );
        let slot = layout
            .spans
            .iter()
            .find(|span| span.space_id == 11)
            .expect("focused slot")
            .rect;
        let escaped = layout
            .items
            .iter()
            .find(|item| {
                matches!(item.kind, ItemKind::Window { space_id: 11, .. }) && item.rect.x < slot.x
            })
            .expect("an icon is scrolled out of its slot");
        let outside = NSPoint::new(
            escaped.rect.x + 1.0,
            escaped.rect.y + escaped.rect.height / 2.0,
        );
        assert!(outside.x < slot.x, "the probe point is left of the slot");
        let hit = window_at(&layout, outside);
        assert_ne!(
            hit.as_ref().map(|item| item.kind.space_id()),
            Some(11),
            "a clipped icon must not be hit through its raw rect"
        );

        let visible = layout
            .items
            .iter()
            .find(|item| {
                matches!(item.kind, ItemKind::Window { space_id: 11, .. })
                    && item.rect.x >= slot.x
                    && item.rect.x + item.rect.width <= slot.x + slot.width
            })
            .expect("an icon remains inside the slot");
        assert!(
            window_at(
                &layout,
                NSPoint::new(
                    visible.rect.x + 1.0,
                    visible.rect.y + visible.rect.height / 2.0
                )
            )
            .is_some(),
            "icons inside the slot stay hittable"
        );
    }

    #[test]
    fn collapsing_eases_the_chrome_between_the_two_states() {
        let now = Instant::now();
        let mut chrome = ChromeMotion::new(true, now);
        assert!((chrome.progress() - 1.0).abs() < f64::EPSILON);
        assert!(!chrome.advance(now), "a settled panel does not redraw");
        chrome.set_target(false, now);
        assert!(chrome.is_active(), "collapsing animates");
        assert!(chrome.advance(now), "the first frame still needs drawing");
        chrome.advance(now + Duration::from_millis(120));
        let half = chrome.progress();
        assert!(
            half < 1.0 && half > -0.05,
            "mid-flight, allowing the spring's small overshoot: {half}"
        );
        let settled = Duration::from_secs_f64(MORPH.duration() + 0.01);
        assert!(
            chrome.advance(now + settled),
            "the settling frame is drawn too"
        );
        assert!((chrome.progress() - 0.0).abs() < f64::EPSILON);
        assert!(!chrome.is_active());
        assert!(!chrome.advance(now + settled + Duration::from_millis(20)));

        // Reversing mid-flight carries on from the frame on screen.
        chrome.set_target(true, now + Duration::from_millis(300));
        chrome.advance(now + Duration::from_millis(360));
        let flying = chrome.progress();
        assert!(flying > 0.0 && flying < 1.0, "expanding: {flying}");
        chrome.set_target(false, now + Duration::from_millis(370));
        assert!(
            (chrome.progress() - flying).abs() < f64::EPSILON,
            "an interrupted morph does not jump"
        );
        assert!(chrome.advance(now + Duration::from_secs(2)));
        assert!((chrome.progress() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_collapsed_shape_keeps_its_top_edge_and_only_grows_downwards() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        let expanded = state.chrome_rect();
        assert_eq!(expanded, band, "expanded, the chrome is the whole band");

        state.set_collapsed(true);
        state
            .chrome
            .advance(Instant::now() + Duration::from_secs(1));
        let collapsed = state.chrome_rect();
        assert_eq!(
            collapsed,
            super::super::placement::collapsed_rect(
                (band.width, band.height),
                state.surface.notch,
                state.hovered,
            ),
            "the drawn shape and the halo share one geometry"
        );
        assert!(
            (collapsed.y - band.y).abs() < f64::EPSILON,
            "the top edge is pinned to the band"
        );
        assert!(
            collapsed.width < band.width,
            "the collapsed shape is narrower"
        );

        // Hovering grows the tab downwards: the top edge never moves, which is
        // what used to make the rectangle appear to jump.
        state.hovered = true;
        let hovered = state.chrome_rect();
        assert_eq!(
            hovered,
            super::super::placement::collapsed_rect(
                (band.width, band.height),
                state.surface.notch,
                true,
            ),
            "hovering moves the same geometry the halo follows"
        );
        assert!((hovered.y - collapsed.y).abs() < f64::EPSILON);
        assert!(hovered.height > collapsed.height, "hover grows the tab");
        assert!((hovered.width - collapsed.width).abs() < f64::EPSILON);
    }

    #[test]
    fn the_bar_ends_toggle_collapse_and_the_middle_does_not() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        assert!(!state.collapsed, "every Bar starts expanded");
        // A hidden handle does not react: the strip sits where the menu bar's
        // own controls were, so an accidental click must not collapse the Bar.
        assert!(state.press_at(NSPoint::new(2.0, 18.0)).is_none());
        assert!(!state.collapsed, "an unrevealed handle is inert");
        // Hovering anywhere on the Bar reveals both handles, so no edge hunting
        // is needed to reach them.
        state.hovered = true;
        assert!(state.press_at(NSPoint::new(2.0, 18.0)).is_none());
        assert!(
            state.collapsed,
            "the revealed left handle collapses the Bar"
        );
        assert!(state.take_chrome_dirty(), "the panel must be rewritten");

        // A plain-screen tab is only a few points tall, so all of it expands,
        // and only it: the rest of the band belongs to the menu bar.
        assert!(
            state
                .press_at(NSPoint::new(band.width / 2.0, 3.0))
                .is_none()
        );
        assert!(!state.collapsed, "the tab expands the Bar again");
        state.set_collapsed(true);
        assert!(
            state
                .press_at(NSPoint::new(band.width - 4.0, 18.0))
                .is_none()
        );
        assert!(
            state.collapsed,
            "the far end of the band is the menu bar's while collapsed"
        );
    }

    #[test]
    fn a_collapsed_capsule_only_expands_from_its_own_handles() {
        let (mut state, _) = drag_state();
        state.surface.notch = Some(Rect {
            x: 500.0,
            y: 0.0,
            width: 180.0,
            height: 37.0,
        });
        state.set_collapsed(true);
        let capsule = state.hover_box();
        // The black area between the handles is not a target: it is the notch.
        assert!(
            state
                .press_at(NSPoint::new(capsule.x + capsule.width / 2.0, 17.0))
                .is_none()
        );
        assert!(state.collapsed, "the middle of the capsule stays collapsed");
        assert!(
            state
                .press_at(NSPoint::new(capsule.x + 3.0, 17.0))
                .is_none()
        );
        assert!(!state.collapsed, "the left handle expands");
    }

    #[test]
    fn every_morph_curve_starts_and_ends_where_it_should() {
        for morph in [Morph::Ease, Morph::Spring, Morph::Smooth] {
            assert!(morph.duration() > 0.0);
            for t in [0.0, 0.5, 1.0] {
                let value = morph.ease(t);
                assert!(value.is_finite(), "{morph:?} at {t} is {value}");
            }
            assert!(
                (morph.ease(1.0) - 1.0).abs() < f64::EPSILON,
                "{morph:?} ends"
            );
            assert!(morph.ease(0.0).abs() < 1e-9, "{morph:?} starts at zero");
            // Nothing may undershoot far enough to invert the collapsed shape.
            let lowest = (0..=100)
                .map(|i| morph.ease(f64::from(i) / 100.0))
                .fold(f64::INFINITY, f64::min);
            assert!(lowest > -0.1, "{morph:?} dips to {lowest}");
        }
        // Only the spring passes the target, and only slightly.
        let peak = (0..=100)
            .map(|i| motion::spring(f64::from(i) / 100.0))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(peak > 1.0 && peak < 1.1, "spring peaks at {peak}");
        // Smoothstep starts slower than the ease-out it is an alternative to.
        assert!(motion::smooth(0.25) < motion::ease_out(0.25));
    }

    #[test]
    fn the_collapsed_capsule_overhangs_the_body_and_rounds_at_the_bottom() {
        // The notch's silhouette: the black is widest along the screen edge, a
        // fillet carries each end back to a vertical wall, and the bottom
        // corners round outwards. Which shoulder shape that fillet is — B in the
        // prototype's render, a convex fillet — is a visual property settled
        // against a photograph of the hardware, so what is pinned here is the
        // structure and the direction.
        let capsule = Rect {
            x: 100.0,
            y: 0.0,
            width: 227.0,
            height: 34.0,
        };
        let shape = ChromeShape {
            bottom: super::super::placement::CAPSULE_BOTTOM_RADIUS,
            top: super::super::placement::CAPSULE_TOP_RADIUS,
            overhang: true,
            shoulder: SHOULDER,
        };
        let path = chrome_path(capsule, shape);
        let t = shape.top;
        assert!(
            (view_rect(path.bounds()).width - (capsule.width + t * 2.0)).abs() < f64::EPSILON,
            "the top edge overhangs the body: {:?}",
            view_rect(path.bounds())
        );
        assert!(
            path.containsPoint(NSPoint::new(capsule.x - 4.0, 0.5)),
            "the overhang is black at the screen edge"
        );
        assert!(
            !path.containsPoint(NSPoint::new(capsule.x - t, t)),
            "and the shoulder has rounded it away by the time it reaches the wall"
        );
        assert!(
            path.containsPoint(NSPoint::new(capsule.x + 0.5, t + 1.0)),
            "the wall is solid below the shoulder"
        );
        assert!(
            !path.containsPoint(NSPoint::new(capsule.x - 1.0, t + 1.0)),
            "and nothing sits outside that wall"
        );
        assert!(
            path.containsPoint(NSPoint::new(capsule.x + 40.0, capsule.height / 2.0)),
            "the body is filled"
        );
        assert!(
            !path.containsPoint(NSPoint::new(capsule.x + 0.5, capsule.height - 0.5)),
            "the bottom-left corner rounds outwards"
        );

        // Expanded, the outline is the band itself: square top corners, rounded
        // bottom ones.
        let band = Rect {
            x: 0.0,
            y: 0.0,
            width: 1470.0,
            height: 34.0,
        };
        let expanded = chrome_path(
            band,
            ChromeShape {
                bottom: 10.0,
                top: 0.0,
                overhang: false,
                shoulder: Shoulder::Fillet,
            },
        );
        assert!(expanded.containsPoint(NSPoint::new(0.5, 0.5)));
        assert!(!expanded.containsPoint(NSPoint::new(0.5, band.height - 0.5)));

        // A plain-screen tab is a pill: six points tall, both ends rounded, so
        // its top corners are convex rather than scooped.
        let tab = Rect {
            x: 900.0,
            y: 0.0,
            width: 120.0,
            height: super::super::placement::PLAIN_TAB_HEIGHT,
        };
        let pill = chrome_path(
            tab,
            ChromeShape {
                bottom: super::super::placement::PLAIN_TAB_HEIGHT / 2.0,
                top: super::super::placement::PLAIN_TAB_HEIGHT / 2.0,
                overhang: false,
                shoulder: Shoulder::Fillet,
            },
        );
        assert!(pill.containsPoint(NSPoint::new(960.0, 3.0)));
        assert!(
            !pill.containsPoint(NSPoint::new(tab.x + 1.0, 0.5)),
            "the tab rounds its top corners"
        );
        assert!(
            !pill.containsPoint(NSPoint::new(tab.x + 1.0, tab.height - 0.5)),
            "and its bottom edge is a semicircle"
        );
    }

    #[test]
    fn the_hover_pulses_stay_inside_their_budget() {
        for (low, high) in [GLOW_PULSE, BUTTON_PULSE] {
            assert!(low > 0.0 && low < high && high <= 1.0, "{low}..{high}");
        }
        assert!(
            BUTTON_PULSE.1 < GLOW_PULSE.0,
            "a button highlight stays quieter than the halo it competes with"
        );
        assert!(
            GLOW_SWELL.0 < 1.0 && GLOW_SWELL.1 > 1.0,
            "the bloom both tightens and swells: {GLOW_SWELL:?}"
        );
        assert!(
            GLOW_PULSE.1 < 1.0,
            "a full-opacity rim reads as neon, not as a hint"
        );
    }

    #[test]
    fn a_breath_never_leaves_the_band() {
        // A six-point tab has room to grow; a capsule that already fills the
        // band can only brighten, so its scale is left at rest.
        let tab = breath_reach(6.0, 31.0);
        assert!(tab > 1.0 && tab <= 1.35, "tab reach: {tab}");
        assert!(
            tab * 6.0 - 6.0 >= 1.5,
            "a breath is visible: {}pt",
            tab * 6.0 - 6.0
        );
        assert!((breath_reach(34.0, 34.0) - 1.0).abs() < f64::EPSILON);
        // A clipped band still cannot push the shape past its own edge.
        assert!(breath_reach(9.0, 8.0) >= 1.0);
    }

    #[test]
    fn measured_text_is_vertically_centered_and_bounded() {
        let region = NSRect::new(NSPoint::new(10.0, 6.0), NSSize::new(96.0, 47.0));
        for height in [12.0, 16.0, 60.0] {
            let text = centered_text_rect(region, height);
            assert!((text.origin.x - region.origin.x).abs() < f64::EPSILON);
            assert!((text.size.width - region.size.width).abs() < f64::EPSILON);
            assert!(
                (text.origin.y + text.size.height / 2.0
                    - region.origin.y
                    - region.size.height / 2.0)
                    .abs()
                    < f64::EPSILON
            );
            assert!(text.size.height <= region.size.height);
        }
    }

    #[test]
    fn overlapping_stack_hit_testing_follows_focused_paint_layer() {
        let mut display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 1200.0);
        let mut motion = BarMotion::new(&initial, now);
        for (step, focused) in [Some(2), None, Some(1)].into_iter().enumerate() {
            for window in &mut display.spaces[1].columns[0].windows {
                window.focused = focused == Some(window.id);
            }
            let target = BarLayout::resolve(&display, 1200.0);
            let start = now + std::time::Duration::from_millis(u64::try_from(step).unwrap() * 100);
            motion.retarget(&target, start);
            motion.advance(start + std::time::Duration::from_millis(50));
            let hits = motion.presented.interaction_layout(&target);
            let front = hits
                .items
                .iter()
                .rev()
                .find(|item| {
                    matches!(
                        item.kind,
                        ItemKind::Window {
                            space_id: 11,
                            column_window_id: Some(1),
                            ..
                        }
                    )
                })
                .unwrap();
            let point = NSPoint::new(
                front.rect.x + front.rect.width / 2.0,
                front.rect.y + front.rect.height / 2.0,
            );
            let hit = window_at(&hits, point).unwrap();
            assert!(
                matches!(hit.kind, ItemKind::Window { window_id, column_window_id: Some(1), .. } if window_id == focused.unwrap_or(1))
            );
            let exposed_top = window_at(
                &hits,
                NSPoint::new(front.rect.x + front.rect.width / 2.0, 7.0),
            )
            .unwrap();
            assert!(matches!(
                exposed_top.kind,
                ItemKind::Window { window_id: 1, .. }
            ));
            assert_eq!(
                display.spaces[1].columns[0]
                    .windows
                    .iter()
                    .map(|window| window.id)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }
    }
}

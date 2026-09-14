use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

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

use super::layout::{ItemKind, Rect};
use super::model::{BarDisplay, BarSnapshot};
use super::motion::VisualItem;
use super::preferences::BarPreferences;
use super::runtime::{
    Bar, BarInput, BarOutcome, BarPoll, BarScreenGeometry, BarSurface, DragPreview, RenderFrame,
};
use super::toolbar;

/// One breath, in seconds. Slow enough to read as breathing rather than as a
/// blink, which is what makes a hovered control feel alive.
const BREATH_PERIOD: f64 = 1.8;

/// A breath has to be slow enough to read as breathing rather than as a blink,
/// and short enough that nobody waits for it. Pinned at compile time so a tuning
/// pass cannot quietly leave the range.
const _: () = assert!(BREATH_PERIOD >= 1.0 && BREATH_PERIOD <= 3.0);

/// Which part of the pulse is animated. The animation's key is its key path, so
/// adding and removing a pulse can never disagree about what to look for.
const PULSE_OPACITY: &str = "opacity";
const PULSE_SCALE: &str = "transform.scale";

/// How bright the translucent highlight under the pointer's toolbar button
/// breathes, low to high. Quiet enough that a hovered button never looks like a
/// second, brighter Bar. The Bar Handle does not use it: a control glued to the
/// Bar answers the pointer by growing a little rather than by changing colour,
/// which keeps the join seamless.
const BUTTON_PULSE: (f64, f64) = (0.07, 0.17);

#[derive(Debug)]
struct BarViewIvars {
    display_id: u32,
    /// The last frame [`Bar`] published. The view draws from it and never
    /// computes state of its own.
    render: RefCell<Option<RenderFrame>>,
    /// Interactions the selectors translated; drained by `Bar::animate`.
    inputs: RefCell<Vec<BarInput>>,
    ghost: RefCell<Option<DragGhost>>,
    toolbar_buttons: RefCell<Vec<(Action, Retained<NSButton>)>>,
    /// One hover highlight per toolbar button, sitting under the button's own
    /// view so the symbol draws on top of it.
    highlights: RefCell<Vec<Retained<CALayer>>>,
    /// Bundle identity to application icon, resolved lazily and shared by every
    /// drawn window.
    icons: RefCell<HashMap<String, Retained<NSImage>>>,
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
            self.enqueue(Action::MissionControl);
        }

        #[unsafe(method(showDesktop:))]
        fn show_desktop(&self, _sender: Option<&AnyObject>) {
            self.enqueue(Action::ShowDesktop);
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
            self.push(BarInput::Press {
                display_id: self.ivars().display_id,
                point: (point.x, point.y),
            });
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.push(BarInput::Drag {
                display_id: self.ivars().display_id,
                point: (point.x, point.y),
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.push(BarInput::Release {
                display_id: self.ivars().display_id,
                point: (point.x, point.y),
            });
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            self.push(BarInput::Cancel {
                display_id: self.ivars().display_id,
            });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let delta = if event.scrollingDeltaX().abs() > event.scrollingDeltaY().abs() {
                event.scrollingDeltaX()
            } else {
                event.scrollingDeltaY()
            };
            // Scrolling belongs to the Space under the pointer: a slot narrower
            // than its content scrolls inside itself, and every other Space
            // keeps its place.
            self.push(BarInput::Scroll {
                display_id: self.ivars().display_id,
                point: (point.x, point.y),
                delta,
            });
        }
    }
);

impl BarView {
    /// Creates the toolbar button for one control, or reports that `AppKit`
    /// could not resolve its symbol.
    fn make_button(&self, control: &toolbar::ToolbarButton) -> Option<Retained<NSButton>> {
        let tooltip = NSString::from_str(control.tooltip);
        let Some(symbol) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(control.symbol),
            Some(&tooltip),
        ) else {
            warn!(symbol = control.symbol, "Bar toolbar symbol unavailable");
            return None;
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
            NSButton::buttonWithImage_target_action(&image, Some(self), Some(action), self.mtm())
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
        Some(button)
    }

    fn sync_toolbar(
        &self,
        preferences: &BarPreferences,
        origin: f64,
        slide: f64,
        controls: &[toolbar::ToolbarButton],
        hidden: bool,
    ) {
        let mut buttons = self.ivars().toolbar_buttons.borrow_mut();
        for control in controls {
            if buttons.iter().any(|(action, _)| *action == control.action) {
                continue;
            }
            let Some(button) = self.make_button(control) else {
                continue;
            };
            self.addSubview(&button);
            buttons.push((control.action.clone(), button));
        }
        let tint = foreground_color(preferences, 1.0);
        for (action, button) in buttons.iter() {
            match controls.iter().find(|control| control.action == *action) {
                // Buttons are subviews, so a Bar that has slid away would leave
                // them floating over the desktop; a collapsed Bar has none.
                None => button.setHidden(true),
                Some(control) => {
                    button.setHidden(hidden);
                    // The buttons lead the Bar's group, so they ride out with it.
                    let mut rect = control.rect;
                    rect.x += origin;
                    rect.y += slide;
                    button.setFrame(ns_rect(rect));
                    button.setContentTintColor(Some(&tint));
                }
            }
        }
        self.sync_highlights(&buttons, controls, origin, slide);
    }

    /// Places the per-button hover highlights under their buttons. The pulse
    /// itself is [`Self::set_button_highlight`]'s, so positioning here cannot
    /// restart a running breath.
    fn sync_highlights(
        &self,
        buttons: &[(Action, Retained<NSButton>)],
        controls: &[toolbar::ToolbarButton],
        origin: f64,
        slide: f64,
    ) {
        let view_height = self.bounds().size.height;
        let parent = self.layer();
        let mut highlights = self.ivars().highlights.borrow_mut();
        while highlights.len() < buttons.len() {
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
            let Some((action, _)) = buttons.get(index) else {
                settle(layer);
                continue;
            };
            let Some(control) = controls.iter().find(|control| control.action == *action) else {
                // A disabled button keeps no highlight.
                settle(layer);
                continue;
            };
            let rect = Rect {
                x: control.rect.x + origin,
                y: control.rect.y + slide,
                ..control.rect
            };
            if let Some(parent) = &parent {
                layer.setFrame(sublayer_rect(parent, view_height, rect));
            }
        }
    }

    /// Breathes the button under the pointer and settles the rest. Idempotent,
    /// so a running breath survives the per-frame sync.
    fn set_button_highlight(&self, action: Option<Action>) {
        let buttons = self.ivars().toolbar_buttons.borrow();
        let highlights = self.ivars().highlights.borrow();
        for ((button_action, _), layer) in buttons.iter().zip(highlights.iter()) {
            if action.as_ref() == Some(button_action) {
                breathe(
                    layer,
                    PULSE_OPACITY,
                    BUTTON_PULSE.0,
                    BUTTON_PULSE.1,
                    BREATH_PERIOD,
                );
                breathe(layer, PULSE_SCALE, 1.0, 1.04, BREATH_PERIOD);
                layer.setOpacity(wash_opacity(BUTTON_PULSE.0));
            } else {
                settle(layer);
            }
        }
    }

    fn push(&self, input: BarInput) {
        self.ivars().inputs.borrow_mut().push(input);
    }

    fn enqueue(&self, action: Action) {
        self.push(BarInput::Activate {
            display_id: self.ivars().display_id,
            action,
        });
    }

    fn take_inputs(&self) -> Vec<BarInput> {
        std::mem::take(&mut *self.ivars().inputs.borrow_mut())
    }

    fn set_render(&self, frame: RenderFrame) {
        *self.ivars().render.borrow_mut() = Some(frame);
    }

    fn new(mtm: MainThreadMarker, display_id: u32, size: NSSize) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
        let this = Self::alloc(mtm).set_ivars(BarViewIvars {
            display_id,
            render: RefCell::new(None),
            inputs: RefCell::new(Vec::new()),
            ghost: RefCell::new(None),
            toolbar_buttons: RefCell::new(Vec::new()),
            highlights: RefCell::new(Vec::new()),
            icons: RefCell::new(HashMap::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        this
    }

    fn draw_bar(&self) {
        let bounds = self.bounds();
        let render = self.ivars().render.borrow();
        let Some(frame) = render.as_ref() else {
            clear(bounds);
            return;
        };
        let preferences = &frame.preferences;
        let band = frame.band;
        let path = chrome_path(
            frame.band,
            frame.handle,
            preferences.corner_radius.clamp(0.0, 20.0),
            preferences.handle_metrics().radius,
        );
        let progress = frame.chrome_progress;
        clear(bounds);

        // One shape: the band slides up out of the screen and the handle stays
        // behind. Nothing here moves the window, so the blur behind it is only
        // ever sampled at the panel's fixed size.
        //
        // The expanded Bar's look is that blur, and the default background
        // colour is deliberately transparent so it shows through. The blur
        // leaves with the chrome, so the shape has to become opaque in its own
        // right or the last thing on screen would be see-through. This fills
        // black in proportion to how much of the Bar has gone, then lays the
        // configured colour over it — which is also what makes the collapsed
        // handle solid black once the Bar is out of the way.
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
            // The content rides the band out of the screen: it is clipped to
            // the band's own rect at the same progress, and to nothing else.
            // There is no fade — the screen's top edge is what takes it away.
            NSGraphicsContext::saveGraphicsState_class();
            translate(0.0, band.y);
            let radius = preferences.corner_radius.clamp(0.0, 20.0);
            let band_at_rest = ns_rect(Rect { y: 0.0, ..band });
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(band_at_rest, radius, radius)
                .addClip();
            if let Some(split) = &frame.presentation.split {
                clip_notch(band_at_rest, split.gap);
            }
            self.draw_strip(frame);
            NSGraphicsContext::restoreGraphicsState_class();
        }

        // The handle is always drawn — it is part of the chrome path, and the
        // Bar's only control — so there is nothing to reveal on hover here. What
        // the pointer changes is how far it has grown, which is already in the
        // path this frame was drawn from.
    }

    /// The Bar's content, drawn in the band's own coordinates and clipped to it
    /// by the caller, which also translates it with the slide.
    fn draw_strip(&self, frame: &RenderFrame) {
        let presentation = &frame.presentation;
        let drag = frame.drag.as_ref();
        NSGraphicsContext::saveGraphicsState_class();
        // Scrolling and dragging belong only to the Space viewport, never the fixed buttons.
        NSBezierPath::bezierPathWithRect(ns_rect(Rect {
            x: presentation.content_left,
            width: (presentation.width - presentation.content_left).max(0.0),
            height: presentation.height,
            ..Rect::default()
        }))
        .addClip();
        for visual in presentation
            .items
            .iter()
            .filter(|visual| matches!(visual.item.kind, ItemKind::Space { .. }))
        {
            Self::draw_space(frame, visual, presentation.viewport(visual.space_id()));
        }
        // Keep the moving fill below icons and its indicator above opaque decks.
        for pass in 0..3 {
            for visual in &presentation.items {
                if drag.is_some_and(|drag| drag.hides(visual)) {
                    continue;
                }
                if matches!(visual.item.kind, ItemKind::Space { .. })
                    || matches!(visual.item.kind, ItemKind::Focus { .. }) != (pass != 1)
                {
                    continue;
                }
                let viewport = presentation.viewport(visual.space_id());
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
                    ItemKind::Window { .. } => self.draw_window(frame, visual),
                    ItemKind::Label { ordinal, .. } => {
                        draw_label(ordinal, visual, &frame.preferences);
                    }
                    ItemKind::Placeholder { .. } => Self::draw_placeholder(frame, visual),
                    ItemKind::Focus { .. } if pass == 0 => Self::draw_focus(frame, visual),
                    ItemKind::Focus { .. } => Self::draw_focus_indicator(frame, visual),
                    _ => {}
                }
                NSGraphicsContext::restoreGraphicsState_class();
            }
        }
        if let Some(drag) = drag
            && let Some(gap) = drag.gap_rect(presentation)
        {
            let gap = gap.intersection(presentation.viewport(drag.target_space()));
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

    fn draw_space(frame: &RenderFrame, visual: &VisualItem, viewport: Rect) {
        let ItemKind::Space { .. } = visual.item.kind else {
            return;
        };
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::bezierPathWithRect(ns_rect(viewport)).addClip();
        let preferences = &frame.preferences;
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

    fn draw_window(&self, _frame: &RenderFrame, visual: &VisualItem) {
        let ItemKind::Window { bundle_id, .. } = &visual.item.kind else {
            return;
        };
        draw_window_visual(visual, self.icon(bundle_id).as_deref());
    }

    fn show_drag_preview(&self, preview: &DragPreview) {
        let mut ghost = self.ivars().ghost.borrow_mut();
        if ghost.is_none() {
            *ghost = Some(DragGhost::new(self.mtm()));
        }
        let Some(ghost) = ghost.as_mut() else {
            return;
        };
        *ghost.view.ivars().borrow_mut() = preview
            .items
            .iter()
            .map(|visual| {
                let icon = match &visual.item.kind {
                    ItemKind::Window { bundle_id, .. } => self.icon(bundle_id),
                    _ => None,
                };
                (visual.clone(), icon)
            })
            .collect();
        ghost
            .view
            .setFrameSize(NSSize::new(preview.rect.width, preview.rect.height));
        ghost.window.setFrame_display(ns_rect(preview.rect), false);
        ghost.view.setNeedsDisplay(true);
        ghost.window.orderFrontRegardless();
    }

    fn hide_drag_preview(&self) {
        self.ivars().ghost.borrow_mut().take();
    }

    fn draw_placeholder(_frame: &RenderFrame, visual: &VisualItem) {
        let rect = ns_rect(visual.item.rect.inset(2.0));
        rounded_stroke(rect, 4.0, 1.0, color(1.0, 1.0, 1.0, 0.22 * visual.opacity));
        draw_symbol(
            "rectangle.dashed",
            ns_rect(visual.item.rect.inset(5.0)),
            0.65 * visual.opacity,
        );
    }

    fn draw_focus(frame: &RenderFrame, visual: &VisualItem) {
        let preferences = &frame.preferences;
        let mut selection =
            BarPreferences::rgba(&preferences.selection_color, [0.04, 0.52, 1.0, 1.0]);
        selection[3] *= visual.opacity;
        let mut fill = selection;
        fill[3] *= 0.35;
        let rect = ns_rect(visual.item.rect);
        rounded_fill(rect, 5.0, rgba(fill));
    }

    fn draw_focus_indicator(frame: &RenderFrame, visual: &VisualItem) {
        let preferences = &frame.preferences;
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
        let mut icons = self.ivars().icons.borrow_mut();
        if let Some(icon) = icons.get(bundle_id) {
            return Some(icon.clone());
        }
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        let icon = apps.firstObject()?.icon()?;
        icons.insert(bundle_id.to_owned(), icon.clone());
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
}

impl DragGhost {
    fn new(mtm: MainThreadMarker) -> Self {
        let view = DragPreviewView::alloc(mtm).set_ivars(RefCell::new(Vec::new()));
        let view: Retained<DragPreviewView> = unsafe {
            msg_send![super(view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0))]
        };
        let window = make_bar_window(mtm, &view);
        // Retained owns the preview lifetime; closing must not release it a second time.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setIgnoresMouseEvents(true);
        window.setLevel(NSMainMenuWindowLevel + 2);
        Self { window, view }
    }
}

impl Drop for DragGhost {
    fn drop(&mut self) {
        self.window.close();
    }
}

/// The Bar's own outline, in viewport coordinates: the band, sliding up out of
/// the screen as the Bar collapses, plus the handle that stays behind.
///
/// The two are separate subpaths of one path, so the fill unions them. The
/// handle's top edge is square because it is glued to the Bar above it (or to
/// the screen's top edge once the band has gone); only its bottom corners
/// round, and the band's own bottom corners follow the configured radius.
fn chrome_path(
    band: Rect,
    handle: Rect,
    band_corner: f64,
    handle_radius: f64,
) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    append_rounded_bottom(&path, band, band_corner);
    append_rounded_bottom(&path, handle, handle_radius);
    path
}

/// Appends one square-topped, round-bottomed rect as a subpath.
///
/// Circular-arc approximation for a cubic Bezier quadrant.
fn append_rounded_bottom(path: &NSBezierPath, rect: Rect, radius: f64) {
    const KAPPA: f64 = 0.552_284_749_8;
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let r = radius.clamp(0.0, (rect.width / 2.0).min(rect.height));
    path.moveToPoint(NSPoint::new(left, top));
    path.lineToPoint(NSPoint::new(right, top));
    if r > 0.0 {
        path.lineToPoint(NSPoint::new(right, bottom - r));
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(right - r, bottom),
            NSPoint::new(right, bottom - r + r * KAPPA),
            NSPoint::new(right - r + r * KAPPA, bottom),
        );
        path.lineToPoint(NSPoint::new(left + r, bottom));
        path.curveToPoint_controlPoint1_controlPoint2(
            NSPoint::new(left, bottom - r),
            NSPoint::new(left + r - r * KAPPA, bottom),
            NSPoint::new(left, bottom - r + r * KAPPA),
        );
    } else {
        path.lineToPoint(NSPoint::new(right, bottom));
        path.lineToPoint(NSPoint::new(left, bottom));
    }
    path.lineToPoint(NSPoint::new(left, top));
    path.closePath();
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

/// The model opacity a hover wash rests at, in the `f32` a layer wants.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a 0..1 alpha, well within f32's range; only sub-pixel precision is lost"
)]
fn wash_opacity(alpha: f64) -> f32 {
    alpha as f32
}

/// Stops every pulse on a layer and rests it.
fn settle(layer: &CALayer) {
    for key in [PULSE_OPACITY, PULSE_SCALE] {
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

struct PanelRecord {
    window: Retained<NSPanel>,
    view: Retained<BarView>,
    /// The mask that shapes the blur into the chrome: the band sliding out,
    /// plus the handle that stays. The blur itself is a subview of the panel's
    /// container, which owns it, and keeps the panel's geometry — re-blurring a
    /// moving window every frame is what used to stutter.
    mask: Retained<CAShapeLayer>,
    /// The panel window: the menu-bar band plus the handle's overhang, and only
    /// ever that.
    frame: Rect,
    shadow: bool,
}

impl PanelRecord {
    /// Presents one frame: the window keeps its rect and the chrome inside it
    /// slides.
    fn present(&self, frame: &RenderFrame) {
        let rect = ns_rect(self.frame);
        if self.window.frame() != rect {
            self.window.setFrame_display(rect, false);
        }
        // The blur is masked to the moving chrome rather than faded: what the
        // eye should read is the Bar leaving, not the Bar dissolving. The mask
        // keeps the panel's size for the panel's whole life, so only its path
        // changes as the chrome slides.
        let path = chrome_path(
            frame.band,
            frame.handle,
            frame.preferences.corner_radius.clamp(0.0, 20.0),
            frame.preferences.handle_metrics().radius,
        );
        let cg_path = path.CGPath();
        self.mask.setPath(Some(&cg_path));
        self.view.setNeedsDisplay(true);
    }

    /// The menu bar underneath is only ours to block where the Bar is actually
    /// drawn. While collapsed the panel still covers the band, so it lets
    /// clicks through everywhere except the handle.
    fn set_interactive(&self, interactive: bool) {
        if self.window.ignoresMouseEvents() == interactive {
            self.window.setIgnoresMouseEvents(!interactive);
        }
    }
}

/// The production adapter: it owns every native handle and turns the port's
/// idempotent effects into `AppKit` calls.
struct AppKitSurface {
    mtm: MainThreadMarker,
    panels: HashMap<u32, PanelRecord>,
}

impl AppKitSurface {
    fn new(mtm: MainThreadMarker) -> Self {
        Self {
            mtm,
            panels: HashMap::new(),
        }
    }
}

impl BarSurface for AppKitSurface {
    fn screen_geometry(
        &self,
        display: &BarDisplay,
        preferences: &BarPreferences,
    ) -> Option<BarScreenGeometry> {
        let screen = screens_by_id(self.mtm).remove(&display.id)?;
        Some(BarScreenGeometry {
            frame: view_rect(screen.frame()),
            visible_frame: view_rect(screen.visibleFrame()),
            safe_area_top: screen.safeAreaInsets().top,
            menu_bar_thickness: NSStatusBar::systemStatusBar().thickness(),
            notch_left: view_rect(screen.auxiliaryTopLeftArea()),
            notch_right: view_rect(screen.auxiliaryTopRightArea()),
            label_width: measured_label_width(display, preferences),
        })
    }

    fn poll(&mut self) -> BarPoll {
        let mut inputs = Vec::new();
        for record in self.panels.values() {
            inputs.append(&mut record.view.take_inputs());
        }
        let pointer = NSEvent::mouseLocation();
        BarPoll {
            pointer: (pointer.x, pointer.y),
            mouse_button_down: NSEvent::pressedMouseButtons() & 1 != 0,
            inputs,
        }
    }

    fn pointer_is_on_chrome(&self) -> bool {
        self.panels
            .values()
            .any(|record| !record.window.ignoresMouseEvents())
    }

    fn ensure_panel(&mut self, display_id: u32, preferences: &BarPreferences, frame: Rect) {
        if let Some(record) = self.panels.get_mut(&display_id) {
            record.frame = frame;
            record.window.setFrame_display(ns_rect(frame), false);
            if record.shadow != preferences.show_shadow {
                record.window.setHasShadow(preferences.show_shadow);
                record.shadow = preferences.show_shadow;
            }
            record.window.orderFrontRegardless();
            return;
        }
        let view = BarView::new(self.mtm, display_id, NSSize::new(frame.width, frame.height));
        let (window, mask) = make_bar_panel(self.mtm, &view);
        window.setHasShadow(preferences.show_shadow);
        window.orderFrontRegardless();
        self.panels.insert(
            display_id,
            PanelRecord {
                window,
                view,
                mask,
                frame,
                shadow: preferences.show_shadow,
            },
        );
    }

    fn remove_panel(&mut self, display_id: u32) {
        if let Some(record) = self.panels.remove(&display_id) {
            record.window.close();
        }
    }

    fn present(&mut self, display_id: u32, frame: &RenderFrame) {
        if let Some(record) = self.panels.get(&display_id) {
            record.view.set_render(frame.clone());
            record.present(frame);
        }
    }

    fn set_interactive(&mut self, display_id: u32, interactive: bool) {
        if let Some(record) = self.panels.get(&display_id) {
            record.set_interactive(interactive);
        }
    }

    fn sync_toolbar(
        &mut self,
        display_id: u32,
        preferences: &BarPreferences,
        origin: f64,
        slide: f64,
        buttons: &[toolbar::ToolbarButton],
        hidden: bool,
    ) {
        if let Some(record) = self.panels.get(&display_id) {
            record
                .view
                .sync_toolbar(preferences, origin, slide, buttons, hidden);
        }
    }

    fn set_button_highlight(&mut self, display_id: u32, action: Option<Action>) {
        if let Some(record) = self.panels.get(&display_id) {
            record.view.set_button_highlight(action);
        }
    }

    fn show_drag_preview(&mut self, display_id: u32, preview: &DragPreview) {
        if let Some(record) = self.panels.get(&display_id) {
            record.view.show_drag_preview(preview);
        }
    }

    fn hide_drag_preview(&mut self, display_id: u32) {
        if let Some(record) = self.panels.get(&display_id) {
            record.view.hide_drag_preview();
        }
    }
}

impl Drop for AppKitSurface {
    fn drop(&mut self) {
        for record in self.panels.values() {
            record.window.close();
        }
    }
}

/// The `NonSend` resource the ECS talks to. It holds the Bar's decisions and
/// the main-thread surface that realises them, and routes returned actions back
/// to the bus.
pub struct BarManager {
    bar: Bar,
    surface: Box<dyn BarSurface>,
    events: EventSender,
}

impl BarManager {
    pub fn new(mtm: MainThreadMarker, events: EventSender) -> Self {
        Self {
            bar: Bar::default(),
            surface: Box::new(AppKitSurface::new(mtm)),
            events,
        }
    }

    pub fn update(&mut self, snapshot: BarSnapshot, preferences: BarPreferences) {
        let outcome = self
            .bar
            .update(snapshot, preferences, self.surface.as_mut());
        self.dispatch(outcome);
    }

    /// Collapses or expands the Bar on the display the user is working on.
    ///
    /// Runtime-only: nothing persists this and every Bar starts expanded.
    pub fn toggle_collapse(&mut self) {
        let outcome = self
            .bar
            .toggle_collapse(Instant::now(), self.surface.as_mut());
        self.dispatch(outcome);
    }

    pub fn is_animating(&self) -> bool {
        self.bar.is_animating()
    }

    /// Whether the pointer is on some Bar panel's own chrome.
    ///
    /// [`BarSurface::set_interactive`] gives a panel mouse events exactly while
    /// the pointer is on the Bar's band or handle — the same test `AppKit`
    /// applies before it hands the click to the Bar rather than to the window
    /// underneath. The global event tap sees the click either way, so this is
    /// how the focus path tells a Bar click from a desktop click.
    pub fn pointer_is_on_chrome(&self) -> bool {
        self.surface.pointer_is_on_chrome()
    }

    pub fn animate(&mut self) {
        let outcome = self.bar.animate(Instant::now(), self.surface.as_mut());
        self.dispatch(outcome);
    }

    fn dispatch(&self, outcome: BarOutcome) {
        for action in outcome.actions {
            if let Err(error) = self.events.dispatch(action) {
                warn!(%error, "unable to dispatch Bar action");
            }
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
    window.setCollectionBehavior(bar_collection_behavior());
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
/// on blur: the blur is faded out as the Bar collapses, and `AppKit` propagates a
/// view's alpha to its subviews, so a nested content view faded to nothing along
/// with it and the collapsed Bar became invisible.
fn make_bar_panel(
    mtm: MainThreadMarker,
    view: &NSView,
) -> (Retained<NSPanel>, Retained<CAShapeLayer>) {
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
    let mask = mask_chrome(&backdrop, size);
    (make_bar_window(mtm, &container), mask)
}

/// Clips the blur to the chrome shape, so the glass leaves with the Bar and the
/// strip beside the handle stays unblurred.
///
/// The backdrop is a plain (unflipped) view, so its mask layer is given a
/// y-down geometry by hand: the chrome path is built in the Bar view's own
/// flipped viewport space, and this is what lets the same numbers mean the same
/// place in both.
fn mask_chrome(backdrop: &NSVisualEffectView, size: NSSize) -> Retained<CAShapeLayer> {
    let mask = CAShapeLayer::new();
    mask.setAnchorPoint(NSPoint::new(0.0, 0.0));
    mask.setPosition(NSPoint::new(0.0, 0.0));
    mask.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), size));
    mask.setAffineTransform(objc2_core_foundation::CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: -1.0,
        tx: 0.0,
        ty: size.height,
    });
    mask.setFillColor(Some(&NSColor::whiteColor().CGColor()));
    backdrop.setWantsLayer(true);
    if let Some(layer) = backdrop.layer() {
        unsafe { layer.setMask(Some(&mask)) };
    } else {
        warn!("Bar backdrop is not layer backed; the blur will cover the handle's overhang");
    }
    mask
}

/// What the Bar's panel is to the window server.
///
/// `Stationary` is the one that matters: it means "unaffected by Exposé", and
/// without it the window server scales the panel to a point the moment Mission
/// Control or Show Desktop runs, so the Bar disappears for as long as the mode
/// is up. It is measurably that — the panel's `WindowServer` bounds collapse to
/// 1x1 — not the Bar hiding itself.
///
/// `Transient` is the alternative and is what the Bar used to set. Apple's
/// guide is explicit that they are mutually exclusive: `Transient` "causes the
/// window to float in Spaces and be hidden in Exposé", which is exactly the
/// wrong half of it here. Dropping it costs nothing, because the Spaces half is
/// already asked for by `CanJoinAllSpaces`.
fn bar_collection_behavior() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::FullScreenAuxiliary
        | NSWindowCollectionBehavior::IgnoresCycle
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

/// Moves the drawing origin, in viewport coordinates.
fn translate(dx: f64, dy: f64) {
    if let Some(context) = objc2_app_kit::NSGraphicsContext::currentContext() {
        objc2_core_graphics::CGContext::translate_ctm(Some(&context.CGContext()), dx, dy);
    }
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

/// The widest label `AppKit` actually draws on this display, clamped the way
/// the layout expects. Without labels nothing is measured, so a display's layout
/// cannot change when labels are off.
fn measured_label_width(display: &BarDisplay, preferences: &BarPreferences) -> f64 {
    if !preferences.show_workspace_labels {
        return 0.0;
    }
    display
        .spaces
        .iter()
        .map(|space| {
            let text = attributed_text(
                &BarPreferences::workspace_label(space.ordinal),
                preferences.label_font_size.clamp(8.0, 20.0),
                NSColor::whiteColor(),
            );
            (text.size().width + 12.0).clamp(24.0, 120.0)
        })
        .fold(0.0, f64::max)
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
    use super::super::motion::{self, EasedProgress};
    use super::*;
    use std::time::Duration;

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
    fn collapsing_eases_the_bar_out_over_the_shared_240ms() {
        let now = Instant::now();
        let mut chrome = EasedProgress::new(true, now);
        assert!((chrome.progress() - 1.0).abs() < f64::EPSILON);
        assert!(!chrome.advance(now), "a settled Bar does not redraw");
        chrome.set_target(false, now);
        assert!(chrome.is_active(), "collapsing animates");
        assert!(chrome.advance(now), "the first frame still needs drawing");
        chrome.advance(now + Duration::from_millis(120));
        let half = chrome.progress();
        assert!(half < 1.0 && half > 0.0, "mid-flight: {half}");
        let settled = Duration::from_secs_f64(motion::DURATION + 0.01);
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
            "an interrupted collapse does not jump"
        );
        assert!(chrome.advance(now + Duration::from_secs(2)));
        assert!((chrome.progress() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_bar_shares_the_one_curve_that_never_passes_its_target() {
        // The Bar leaves through the screen's top edge, so a curve that
        // overshoots would push the handle past the edge it is resting on: the
        // content's ease-out is the only curve in the Bar.
        assert!((motion::ease_out(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((motion::ease_out(1.0) - 1.0).abs() < f64::EPSILON);
        let mut previous = -1.0;
        for step in 0..=100 {
            let value = motion::ease_out(f64::from(step) / 100.0);
            assert!(value >= previous, "monotonic at {step}: {value}");
            assert!((0.0..=1.0).contains(&value), "bounded at {step}: {value}");
            previous = value;
        }
    }

    #[test]
    fn the_chrome_path_is_the_band_with_a_square_topped_handle_below_it() {
        let band = Rect {
            x: 0.0,
            y: 0.0,
            width: 1470.0,
            height: 24.0,
        };
        let handle = Rect {
            x: 703.0,
            y: 24.0,
            width: 64.0,
            height: 10.0,
        };
        let path = chrome_path(
            band,
            handle,
            0.0,
            super::super::placement::HandleMetrics::DEFAULT.radius,
        );
        assert!(path.containsPoint(NSPoint::new(0.5, 0.5)), "the band fills");
        assert!(
            path.containsPoint(NSPoint::new(735.0, 33.5)),
            "the handle fills"
        );
        assert!(
            path.containsPoint(NSPoint::new(704.5, 24.5)),
            "the handle's top corners are square: it is glued to the Bar"
        );
        assert!(
            !path.containsPoint(NSPoint::new(703.5, 33.5)),
            "its bottom-left corner rounds"
        );
        assert!(
            !path.containsPoint(NSPoint::new(766.5, 33.5)),
            "its bottom-right corner rounds"
        );
        assert!(
            !path.containsPoint(NSPoint::new(400.0, 30.0)),
            "nothing is drawn beside the handle, below the band"
        );

        // Collapsed, the band is off the top edge and the handle is all that is
        // left of the Bar.
        let collapsed = chrome_path(
            Rect { y: -24.0, ..band },
            Rect { y: 0.0, ..handle },
            0.0,
            super::super::placement::HandleMetrics::DEFAULT.radius,
        );
        assert!(collapsed.containsPoint(NSPoint::new(735.0, 5.0)));
        assert!(!collapsed.containsPoint(NSPoint::new(400.0, 5.0)));
        assert!(!collapsed.containsPoint(NSPoint::new(735.0, 29.0)));

        // The band's own bottom corners follow the configured radius, so a
        // user's `corner_radius` still rounds the Bar itself.
        let rounded = chrome_path(
            band,
            handle,
            10.0,
            super::super::placement::HandleMetrics::DEFAULT.radius,
        );
        assert!(!rounded.containsPoint(NSPoint::new(0.5, band.height - 0.5)));
        assert!(rounded.containsPoint(NSPoint::new(0.5, 0.5)));
    }

    #[test]
    fn the_panel_is_stationary_so_expose_cannot_take_the_bar_away() {
        let behavior = bar_collection_behavior();
        assert!(
            behavior.contains(NSWindowCollectionBehavior::Stationary),
            "Mission Control and Show Desktop must not move or scale the Bar"
        );
        assert!(
            !behavior.contains(NSWindowCollectionBehavior::Transient),
            "Transient means hidden in Expose — the two are mutually exclusive"
        );
        assert!(
            !behavior.contains(NSWindowCollectionBehavior::Managed),
            "the Bar would then be a thumbnail in the Mission Control grid"
        );
        assert!(
            behavior.contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
            "the Bar belongs to every Space, which Transient used to provide"
        );
    }

    #[test]
    fn the_hover_wash_stays_inside_its_budget() {
        let (low, high) = BUTTON_PULSE;
        assert!(low > 0.0 && low < high && high <= 1.0, "{low}..{high}");
        assert!(
            high <= 0.25,
            "a hover wash is a hint, not a second, brighter Bar: {high}"
        );
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
}

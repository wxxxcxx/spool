use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSFloatingWindowLevel,
    NSFont, NSGraphicsContext, NSModalPanelWindowLevel, NSParagraphStyle, NSScreen, NSView,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGFloat;
use objc2_core_graphics::CGDirectDisplayID;
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableCopying, NSNumber, NSPoint, NSRect, NSSize,
    NSString, ns_string,
};

use crate::manager::move_owned_window_to_space;
use crate::platform::{WinID, WorkspaceId};

#[derive(Clone, Debug, PartialEq)]
pub struct BorderParams {
    pub color: (f64, f64, f64),
    pub opacity: f64,
    pub width: f64,
    pub radius: f64,
}

/// Declarative decorations for one ordinary native Space.
///
/// The Space owns one transparent surface on its physical display. A missing
/// focused target intentionally renders a fully dimmed Space with no border;
/// this lets newly discovered inactive Spaces have a prepared surface before
/// their first transition.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceOverlayTarget {
    pub space_id: WorkspaceId,
    pub display_id: CGDirectDisplayID,
    pub focused_abs_cg: Option<NSRect>,
    pub focused_window_id: Option<WinID>,
    pub border: Option<BorderParams>,
}

#[derive(Clone, Debug, PartialEq)]
struct DecorationStyle {
    dim_opacity: f32,
    dim_color: (f64, f64, f64),
    cutout_radius: f64,
    border: Option<BorderParams>,
}

#[derive(Clone, Debug, PartialEq)]
struct DecorationDrawState {
    style: DecorationStyle,
    cutout: Option<NSRect>,
    border_rect: Option<NSRect>,
}

// ── DecorationView: dim cutout and border on one persistent surface ──

#[derive(Debug, Clone)]
struct DecorationViewIvars {
    dim_opacity: Cell<f32>,
    dim_r: Cell<f64>,
    dim_g: Cell<f64>,
    dim_b: Cell<f64>,
    cutout_x: Cell<f64>,
    cutout_y: Cell<f64>,
    cutout_w: Cell<f64>,
    cutout_h: Cell<f64>,
    has_cutout: Cell<bool>,
    cutout_radius: Cell<f64>,
    border_x: Cell<f64>,
    border_y: Cell<f64>,
    border_w: Cell<f64>,
    border_h: Cell<f64>,
    has_border: Cell<bool>,
    border_r: Cell<f64>,
    border_g: Cell<f64>,
    border_b: Cell<f64>,
    border_opacity: Cell<f64>,
    border_width: Cell<f64>,
    border_radius: Cell<f64>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolDecorationView"]
    #[ivars = DecorationViewIvars]
    #[derive(Debug)]
    struct DecorationView;

    impl DecorationView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let ivars = self.ivars();
            let bounds = self.bounds();

            // Every animation frame must erase the previous cutout and border
            // from the buffered transparent window before drawing the next one.
            if let Some(ctx) = NSGraphicsContext::currentContext() {
                ctx.setCompositingOperation(NSCompositingOperation::Clear);
                NSBezierPath::fillRect(bounds);
                ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
            }

            if ivars.dim_opacity.get() != 0.0 {
                let dim_color = NSColor::colorWithSRGBRed_green_blue_alpha(
                    ivars.dim_r.get() as CGFloat,
                    ivars.dim_g.get() as CGFloat,
                    ivars.dim_b.get() as CGFloat,
                    CGFloat::from(ivars.dim_opacity.get()),
                );
                dim_color.setFill();
                NSBezierPath::fillRect(bounds);
            }

            if ivars.dim_opacity.get() != 0.0 && ivars.has_cutout.get() {
                let cutout = NSRect::new(
                    NSPoint::new(ivars.cutout_x.get(), ivars.cutout_y.get()),
                    NSSize::new(ivars.cutout_w.get(), ivars.cutout_h.get()),
                );

                // Punch a rounded transparent hole using Clear compositing.
                if let Some(ctx) = NSGraphicsContext::currentContext() {
                    ctx.setCompositingOperation(NSCompositingOperation::Clear);
                    let hole = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                        cutout,
                        ivars.cutout_radius.get() as CGFloat,
                        ivars.cutout_radius.get() as CGFloat,
                    );
                    hole.fill();
                    ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
                }
            }

            if ivars.has_border.get() {
                let rect = NSRect::new(
                    NSPoint::new(ivars.border_x.get(), ivars.border_y.get()),
                    NSSize::new(ivars.border_w.get(), ivars.border_h.get()),
                );
                let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    rect,
                    ivars.border_radius.get() as CGFloat,
                    ivars.border_radius.get() as CGFloat,
                );
                path.setLineWidth(ivars.border_width.get() as CGFloat);
                let color = NSColor::colorWithSRGBRed_green_blue_alpha(
                    ivars.border_r.get() as CGFloat,
                    ivars.border_g.get() as CGFloat,
                    ivars.border_b.get() as CGFloat,
                    ivars.border_opacity.get() as CGFloat,
                );
                color.setStroke();
                path.stroke();
            }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl DecorationView {
    fn new(mtm: MainThreadMarker, frame: NSRect, state: &DecorationDrawState) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DecorationViewIvars {
            dim_opacity: Cell::new(0.0),
            dim_r: Cell::new(0.0),
            dim_g: Cell::new(0.0),
            dim_b: Cell::new(0.0),
            cutout_x: Cell::new(0.0),
            cutout_y: Cell::new(0.0),
            cutout_w: Cell::new(0.0),
            cutout_h: Cell::new(0.0),
            has_cutout: Cell::new(false),
            cutout_radius: Cell::new(0.0),
            border_x: Cell::new(0.0),
            border_y: Cell::new(0.0),
            border_w: Cell::new(0.0),
            border_h: Cell::new(0.0),
            has_border: Cell::new(false),
            border_r: Cell::new(0.0),
            border_g: Cell::new(0.0),
            border_b: Cell::new(0.0),
            border_opacity: Cell::new(0.0),
            border_width: Cell::new(0.0),
            border_radius: Cell::new(0.0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.update(state);
        this
    }

    fn update(&self, state: &DecorationDrawState) {
        let (has_cutout, cx, cy, cw, ch) =
            state.cutout.map_or((false, 0.0, 0.0, 0.0, 0.0), |rect| {
                (
                    true,
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                )
            });
        let (has_border, bx, by, bw, bh) =
            state
                .border_rect
                .map_or((false, 0.0, 0.0, 0.0, 0.0), |rect| {
                    (
                        true,
                        rect.origin.x,
                        rect.origin.y,
                        rect.size.width,
                        rect.size.height,
                    )
                });
        let ivars = self.ivars();
        ivars.dim_opacity.set(state.style.dim_opacity);
        ivars.dim_r.set(state.style.dim_color.0);
        ivars.dim_g.set(state.style.dim_color.1);
        ivars.dim_b.set(state.style.dim_color.2);
        ivars.cutout_x.set(cx);
        ivars.cutout_y.set(cy);
        ivars.cutout_w.set(cw);
        ivars.cutout_h.set(ch);
        ivars.has_cutout.set(has_cutout);
        ivars.cutout_radius.set(state.style.cutout_radius);
        ivars.border_x.set(bx);
        ivars.border_y.set(by);
        ivars.border_w.set(bw);
        ivars.border_h.set(bh);
        ivars.has_border.set(has_border);
        if let Some(border) = &state.style.border {
            ivars.border_r.set(border.color.0);
            ivars.border_g.set(border.color.1);
            ivars.border_b.set(border.color.2);
            ivars.border_opacity.set(border.opacity);
            ivars.border_width.set(border.width);
            ivars.border_radius.set(border.radius);
        }
        self.setNeedsDisplay(true);
    }
}

// ── Coordinate helpers ──────────────────────────────────────────────────

/// Convert an absolute CG screen frame (origin top-left, y-down) to Cocoa
/// screen coordinates (origin bottom-left of primary screen, y-up).
fn cg_abs_to_cocoa(frame: NSRect, primary_screen_height: f64) -> NSRect {
    let cocoa_y = primary_screen_height - frame.origin.y - frame.size.height;
    NSRect::new(NSPoint::new(frame.origin.x, cocoa_y), frame.size)
}

/// Height of the display at the global origin (the main display, whose Cocoa
/// frame origin is `(0, 0)`). The CG↔Cocoa Y-flip is anchored to this display,
/// so it must be *that* screen — `NSScreen::screens()[0]` is NOT reliably the
/// main display, and using the wrong one offsets the overlay (and, when
/// displays are stacked, lands it on the wrong monitor).
fn primary_screen_height(mtm: MainThreadMarker) -> f64 {
    let screens = NSScreen::screens(mtm);
    let mut fallback = 0.0;
    let mut first = true;
    for screen in &screens {
        let frame = screen.frame();
        if first {
            fallback = frame.size.height;
            first = false;
        }
        if frame.origin.x == 0.0 && frame.origin.y == 0.0 {
            return frame.size.height;
        }
    }
    fallback
}

/// Do two Cocoa rects overlap? Used to decide which displays a focused window
/// touches (a window straddling a seam touches both).
fn rects_intersect(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScreenFrame {
    id: CGDirectDisplayID,
    frame: NSRect,
}

fn border_touches_screen(target: NSRect, screen: NSRect, border_width: f64) -> bool {
    let half_width = border_width.max(0.0) / 2.0;
    let outer = NSRect::new(
        NSPoint::new(target.origin.x - half_width, target.origin.y - half_width),
        NSSize::new(
            target.size.width + border_width.max(0.0),
            target.size.height + border_width.max(0.0),
        ),
    );
    rects_intersect(outer, screen)
}

fn screen_local_decoration(target: NSRect, screen: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            target.origin.x - screen.origin.x,
            (screen.origin.y + screen.size.height) - (target.origin.y + target.size.height),
        ),
        target.size,
    )
}

fn project_decoration(
    presented: NSRect,
    screen: NSRect,
    style: &DecorationStyle,
) -> DecorationDrawState {
    let local = screen_local_decoration(presented, screen);
    DecorationDrawState {
        style: style.clone(),
        cutout: (style.dim_opacity != 0.0 && rects_intersect(presented, screen)).then_some(local),
        border_rect: style
            .border
            .as_ref()
            .is_some_and(|border| border_touches_screen(presented, screen, border.width))
            .then_some(local),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct DecorationPresentation {
    presented: NSRect,
    target: NSRect,
    target_id: WinID,
    animating: bool,
}

fn retarget_decoration(
    current: Option<DecorationPresentation>,
    target: NSRect,
    target_id: WinID,
    allow_animation: bool,
) -> DecorationPresentation {
    let Some(current) = current else {
        return DecorationPresentation {
            presented: target,
            target,
            target_id,
            animating: false,
        };
    };

    // One observed focus transition dirties the overlay twice: first when the
    // coordinator confirms the new target, then when `FocusedMarker` catches
    // up. The second projection is the same declarative state and must be
    // idempotent; snapping it to `target` would visibly collapse the shared
    // dim cutout/border animation midway through the focus change.
    if allow_animation
        && current.animating
        && current.target_id == target_id
        && current.target == target
    {
        return current;
    }

    let focus_changed = current.target_id != target_id;
    let animate = allow_animation && focus_changed && current.presented != target;
    DecorationPresentation {
        presented: if animate { current.presented } else { target },
        target,
        target_id,
        animating: animate,
    }
}

fn interpolate_rect(current: NSRect, target: NSRect, factor: f64) -> NSRect {
    let lerp = |from: f64, to: f64| from + (to - from) * factor;
    NSRect::new(
        NSPoint::new(
            lerp(current.origin.x, target.origin.x),
            lerp(current.origin.y, target.origin.y),
        ),
        NSSize::new(
            lerp(current.size.width, target.size.width),
            lerp(current.size.height, target.size.height),
        ),
    )
}

fn rect_is_close(left: NSRect, right: NSRect) -> bool {
    const SNAP_THRESHOLD: f64 = 0.5;
    (left.origin.x - right.origin.x).abs() <= SNAP_THRESHOLD
        && (left.origin.y - right.origin.y).abs() <= SNAP_THRESHOLD
        && (left.size.width - right.size.width).abs() <= SNAP_THRESHOLD
        && (left.size.height - right.size.height).abs() <= SNAP_THRESHOLD
}

fn advance_decoration(presentation: &mut DecorationPresentation, rate: f64, delta: f64) -> bool {
    if !presentation.animating {
        return false;
    }

    if rate <= 0.0 || !rate.is_finite() || !delta.is_finite() {
        presentation.presented = presentation.target;
        presentation.animating = false;
        return true;
    }

    let factor = (1.0 - (-rate * delta.max(0.0)).exp()).clamp(0.0, 1.0);
    let next = interpolate_rect(presentation.presented, presentation.target, factor);
    if rect_is_close(next, presentation.target) {
        presentation.presented = presentation.target;
        presentation.animating = false;
    } else {
        presentation.presented = next;
    }
    true
}

fn screen_frames(mtm: MainThreadMarker) -> Vec<ScreenFrame> {
    let screens = NSScreen::screens(mtm);
    (&screens)
        .into_iter()
        .filter_map(|screen| {
            let description = screen.deviceDescription();
            let numbers = unsafe { description.cast_unchecked::<NSString, NSNumber>() };
            let id = numbers.objectForKey(ns_string!("NSScreenNumber"))?.as_u32();
            Some(ScreenFrame {
                id,
                frame: screen.frame(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn decoration_projection_uses_the_flipped_display_local_rect() {
        assert_eq!(
            screen_local_decoration(
                rect(550.0, 120.0, 240.0, 180.0),
                rect(500.0, 0.0, 500.0, 500.0),
            ),
            rect(50.0, 200.0, 240.0, 180.0)
        );
    }

    #[test]
    fn decoration_overlay_stays_above_cross_application_focus_reordering() {
        assert_eq!(
            decoration_overlay_level(),
            objc2_app_kit::NSModalPanelWindowLevel
        );
        assert!(decoration_overlay_level() > NSFloatingWindowLevel);
        assert!(decoration_overlay_level() < objc2_app_kit::NSMainMenuWindowLevel);
    }

    #[test]
    fn decoration_overlay_is_transient_and_belongs_to_exactly_one_non_fullscreen_space() {
        let behavior = decoration_collection_behavior();
        assert!(behavior.contains(NSWindowCollectionBehavior::CanJoinAllApplications));
        assert!(!behavior.contains(NSWindowCollectionBehavior::Stationary));
        assert!(behavior.contains(NSWindowCollectionBehavior::Transient));
        assert!(!behavior.contains(NSWindowCollectionBehavior::Managed));
        assert!(!behavior.contains(NSWindowCollectionBehavior::CanJoinAllSpaces));
        assert!(!behavior.contains(NSWindowCollectionBehavior::FullScreenAuxiliary));
        assert!(behavior.contains(NSWindowCollectionBehavior::FullScreenNone));
    }

    #[test]
    fn dim_cutout_and_border_share_the_same_presented_frame() {
        let presented = rect(550.0, 120.0, 240.0, 180.0);
        let style = DecorationStyle {
            dim_opacity: 0.2,
            dim_color: (0.0, 0.0, 0.0),
            cutout_radius: 10.0,
            border: Some(border_params()),
        };

        let projected = project_decoration(presented, rect(500.0, 0.0, 500.0, 500.0), &style);

        assert_eq!(projected.cutout, projected.border_rect);
        assert_eq!(projected.cutout, Some(rect(50.0, 200.0, 240.0, 180.0)));
    }

    #[test]
    fn border_stroke_is_visible_on_both_sides_of_a_display_boundary() {
        let target = rect(500.0, 100.0, 100.0, 120.0);

        assert!(border_touches_screen(
            target,
            rect(0.0, 0.0, 500.0, 500.0),
            4.0
        ));
        assert!(border_touches_screen(
            target,
            rect(500.0, 0.0, 500.0, 500.0),
            4.0
        ));
    }

    fn border_params() -> BorderParams {
        BorderParams {
            color: (1.0, 0.5, 0.0),
            opacity: 0.9,
            width: 4.0,
            radius: 10.0,
        }
    }

    #[test]
    fn focus_change_animates_from_the_current_presented_border() {
        let original = rect(20.0, 20.0, 300.0, 500.0);
        let next = rect(340.0, 20.0, 300.0, 500.0);
        let current = retarget_decoration(None, original, 1, true);

        let retargeted = retarget_decoration(Some(current), next, 2, true);

        assert_eq!(retargeted.presented, original);
        assert_eq!(retargeted.target, next);
        assert!(retargeted.animating);
    }

    #[test]
    fn repeated_projection_of_the_same_focus_preserves_the_running_animation() {
        let original = rect(20.0, 20.0, 300.0, 500.0);
        let next = rect(340.0, 20.0, 300.0, 500.0);
        let current = retarget_decoration(None, original, 1, true);
        let moving = retarget_decoration(Some(current), next, 2, true);

        let repeated = retarget_decoration(Some(moving), next, 2, true);

        assert_eq!(repeated.presented, original);
        assert_eq!(repeated.target, next);
        assert!(repeated.animating);
    }

    #[test]
    fn changing_the_same_focused_window_geometry_does_not_add_border_lag() {
        let original = rect(20.0, 20.0, 300.0, 500.0);
        let next = rect(40.0, 20.0, 280.0, 500.0);
        let current = retarget_decoration(None, original, 1, true);

        let retargeted = retarget_decoration(Some(current), next, 1, true);

        assert_eq!(retargeted.presented, next);
        assert!(!retargeted.animating);
    }

    #[test]
    fn focus_change_after_overlay_suppression_snaps_instead_of_crossing_spaces() {
        let original = rect(20.0, 20.0, 300.0, 500.0);
        let next = rect(340.0, 20.0, 300.0, 500.0);
        let current = retarget_decoration(None, original, 1, true);

        let retargeted = retarget_decoration(Some(current), next, 2, false);

        assert_eq!(retargeted.presented, next);
        assert!(!retargeted.animating);
    }

    #[test]
    fn border_animation_advances_position_and_size_together() {
        let original = rect(0.0, 20.0, 300.0, 500.0);
        let next = rect(100.0, 40.0, 500.0, 300.0);
        let current = retarget_decoration(None, original, 1, true);
        let mut retargeted = retarget_decoration(Some(current), next, 2, true);

        assert!(advance_decoration(
            &mut retargeted,
            1.0,
            std::f64::consts::LN_2
        ));
        assert_eq!(retargeted.presented, rect(50.0, 30.0, 400.0, 400.0));
        assert!(retargeted.animating);
    }
}

// ── Overlay window factory ──────────────────────────────────────────────

fn decoration_overlay_level() -> isize {
    NSModalPanelWindowLevel
}

fn decoration_collection_behavior() -> NSWindowCollectionBehavior {
    // Accessory NSWindows can retain managed WindowServer tags at elevated levels.
    // Explicit Transient excludes them from Expose without joining other Spaces.
    NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::Transient
        | NSWindowCollectionBehavior::CanJoinAllApplications
        | NSWindowCollectionBehavior::FullScreenNone
}

fn make_overlay_window(
    mtm: MainThreadMarker,
    cocoa_frame: NSRect,
    level: isize,
) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            cocoa_frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setIgnoresMouseEvents(true);
    window.setHasShadow(false);
    window.setLevel(level);
    window.setCollectionBehavior(decoration_collection_behavior());
    // OverlayManager owns the only strong Rust reference. Releasing the
    // WindowServer object on `close()` prevents obsolete Space surfaces from
    // surviving as permanently off-screen windows after topology/config
    // reconciliation.
    unsafe {
        window.setReleasedWhenClosed(true);
    }

    window
}
// ── OverlayManager ──────────────────────────────────────────────────────

pub struct OverlayManager {
    mtm: MainThreadMarker,
    /// One persistent transparent decoration surface per ordinary native
    /// Space. Each surface is moved to that exact Space once, then left for
    /// `WindowServer` to compose as part of the Space transition.
    decoration_overlays: HashMap<WorkspaceId, DecorationOverlay>,
    hidden: bool,
}

struct DecorationOverlay {
    display_id: CGDirectDisplayID,
    screen_frame: NSRect,
    window: Retained<NSWindow>,
    view: Retained<DecorationView>,
    presentation: Option<DecorationPresentation>,
    style: DecorationStyle,
}

impl OverlayManager {
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self {
            mtm,
            decoration_overlays: HashMap::new(),
            hidden: false,
        }
    }

    /// Reconcile all ordinary native Space overlays from declarative state.
    ///
    /// Fullscreen Spaces must not be present in `targets`. A target without a
    /// focused window remains a fully dimmed prepared surface, so switching to
    /// a Space never has to create or move an overlay during the transition.
    pub fn update(
        &mut self,
        dim_opacity: f32,
        dim_color: (f64, f64, f64),
        targets: &[SpaceOverlayTarget],
    ) {
        let screen_h = primary_screen_height(self.mtm);
        let screens = screen_frames(self.mtm);
        self.sync_decoration_overlays(&screens, screen_h, targets, dim_opacity, dim_color);
        self.hidden = false;
        self.render_decorations();
    }

    fn sync_decoration_overlays(
        &mut self,
        screens: &[ScreenFrame],
        primary_screen_height: f64,
        targets: &[SpaceOverlayTarget],
        dim_opacity: f32,
        dim_color: (f64, f64, f64),
    ) {
        let desired_spaces = targets
            .iter()
            .map(|target| target.space_id)
            .collect::<HashSet<_>>();
        self.decoration_overlays.retain(|space_id, overlay| {
            // Native Space identity survives display sleep/wake and temporary
            // display-ID churn. Reuse the same NSWindow and update its display
            // and frame below instead of allocating a second surface.
            let keep = desired_spaces.contains(space_id);
            if !keep {
                overlay.window.close();
            }
            keep
        });

        for target in targets {
            let Some(screen) = screens.iter().find(|screen| screen.id == target.display_id) else {
                continue;
            };
            let style = DecorationStyle {
                dim_opacity,
                dim_color,
                cutout_radius: target.border.as_ref().map_or(0.0, |params| params.radius),
                border: target.border.clone(),
            };
            let focused_cocoa = target
                .focused_abs_cg
                .map(|frame| cg_abs_to_cocoa(frame, primary_screen_height));
            let focused = focused_cocoa.zip(target.focused_window_id);

            if let Some(overlay) = self.decoration_overlays.get_mut(&target.space_id) {
                if overlay.display_id != screen.id || overlay.screen_frame != screen.frame {
                    overlay.window.setFrame_display(screen.frame, false);
                    overlay
                        .view
                        .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), screen.frame.size));
                    overlay.display_id = screen.id;
                    overlay.screen_frame = screen.frame;
                }
                overlay.presentation = focused.map(|(frame, window_id)| {
                    retarget_decoration(overlay.presentation.take(), frame, window_id, !self.hidden)
                });
                overlay.style = style;
                continue;
            }

            let window = make_overlay_window(self.mtm, screen.frame, decoration_overlay_level());
            let view_frame = NSRect::new(NSPoint::new(0.0, 0.0), screen.frame.size);
            let empty_state = DecorationDrawState {
                style: style.clone(),
                cutout: None,
                border_rect: None,
            };
            let view = DecorationView::new(self.mtm, view_frame, &empty_state);
            window.setContentView(Some(&view));
            let Ok(window_id) = WinID::try_from(window.windowNumber()) else {
                window.close();
                continue;
            };
            if let Err(error) = move_owned_window_to_space(window_id, target.space_id) {
                tracing::warn!(
                    space_id = target.space_id,
                    %error,
                    "unable to bind decoration overlay to native Space"
                );
                window.close();
                continue;
            }
            let presentation = focused
                .map(|(frame, target_id)| retarget_decoration(None, frame, target_id, false));
            self.decoration_overlays.insert(
                target.space_id,
                DecorationOverlay {
                    display_id: screen.id,
                    screen_frame: screen.frame,
                    window,
                    view,
                    presentation,
                    style,
                },
            );
        }
    }

    fn render_decorations(&self) {
        for overlay in self.decoration_overlays.values() {
            let draw_state = overlay.presentation.as_ref().map_or_else(
                || DecorationDrawState {
                    style: overlay.style.clone(),
                    cutout: None,
                    border_rect: None,
                },
                |presentation| {
                    project_decoration(presentation.presented, overlay.screen_frame, &overlay.style)
                },
            );
            if overlay.style.dim_opacity != 0.0 || draw_state.border_rect.is_some() {
                overlay.view.update(&draw_state);
                // Commit the replacement backing contents before ordering the
                // window. Cross-application activation must never expose the
                // transparent buffer between the old and new decorations.
                overlay.view.displayIfNeeded();
                overlay.window.orderFrontRegardless();
            } else {
                overlay.window.orderOut(None::<&AnyObject>);
            }
        }
    }

    pub fn animate_decorations(&mut self, delta: f64, rate: f64) {
        if self.hidden {
            return;
        }
        let advanced = self.decoration_overlays.values_mut().any(|overlay| {
            overlay
                .presentation
                .as_mut()
                .is_some_and(|presentation| advance_decoration(presentation, rate, delta))
        });
        if advanced {
            self.render_decorations();
        }
    }

    pub fn decorations_are_animating(&self) -> bool {
        !self.hidden
            && self
                .decoration_overlays
                .values()
                .filter_map(|overlay| overlay.presentation.as_ref())
                .any(|presentation| presentation.animating)
    }

    pub fn remove_all(&mut self) {
        for (_, overlay) in self.decoration_overlays.drain() {
            overlay.window.close();
        }
        self.hidden = false;
    }

    pub fn hide_all(&mut self) {
        if self.hidden {
            return;
        }
        for overlay in self.decoration_overlays.values() {
            overlay.window.orderOut(None::<&AnyObject>);
        }
        self.hidden = true;
    }
}

// ── FlashMessage ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FlashMessageViewIvars {
    opacity: f32,
    message: Retained<NSString>,
    is_badge: bool,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolFlashMessageView"]
    #[ivars = FlashMessageViewIvars]
    #[derive(Debug)]
    struct FlashMessageView;

    impl FlashMessageView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let ivars = self.ivars();
            let bounds = self.bounds();
            let is_badge = ivars.is_badge;

            // 1. Draw semi-transparent bezel
            let bezel_color = NSColor::colorWithSRGBRed_green_blue_alpha(
                0.12, 0.12, 0.12,
                CGFloat::from(ivars.opacity * 0.88),
            );
            bezel_color.setFill();
            let radius = if is_badge {
                24.0
            } else {
                bounds.size.height / 2.0
            };
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                bounds, radius, radius,
            );
            path.fill();

            // Draw subtle border for contrast
            let border_color = NSColor::colorWithSRGBRed_green_blue_alpha(
                1.0, 1.0, 1.0,
                CGFloat::from(ivars.opacity * 0.15),
            );
            border_color.setStroke();
            path.setLineWidth(1.0);
            path.stroke();

            // 2. Draw text
            let font = if is_badge {
                NSFont::boldSystemFontOfSize(bounds.size.height * 0.62)
            } else {
                NSFont::systemFontOfSize(30.0)
            };
            let color = NSColor::colorWithSRGBRed_green_blue_alpha(
                1.0, 1.0, 1.0,
                CGFloat::from(ivars.opacity * 0.95),
            );

            let paragraph_style = unsafe {
                let style = NSParagraphStyle::defaultParagraphStyle().mutableCopy();
                let _: () = msg_send![&style, setAlignment: 1isize]; // Center (NSTextAlignmentCenter = 1)
                let _: () = msg_send![&style, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail = 4
                style
            };

            let attr_str: Retained<NSAttributedString> = unsafe {
                let font_key = NSString::from_str("NSFont");
                let color_key = NSString::from_str("NSColor");
                let para_key = NSString::from_str("NSParagraphStyle");

                let keys = [&*font_key, &*color_key, &*para_key];
                let objects = [
                    &*font as &AnyObject,
                    &*color as &AnyObject,
                    &*paragraph_style as &AnyObject,
                ];

                let attributes = NSDictionary::from_slices(&keys, &objects);
                let alloc = NSAttributedString::alloc();
                msg_send![alloc, initWithString: &*ivars.message, attributes: &*attributes]
            };

            let text_size: NSSize = unsafe { msg_send![&attr_str, size] };

            let text_rect = if is_badge {
                NSRect::new(
                    NSPoint::new(
                        bounds.origin.x + (bounds.size.width - text_size.width) / 2.0,
                        bounds.origin.y + (bounds.size.height - text_size.height) / 2.0,
                    ),
                    text_size,
                )
            } else {
                let h_pad = 24.0;
                let available_width = (bounds.size.width - (2.0 * h_pad)).max(1.0);
                NSRect::new(
                    NSPoint::new(
                        bounds.origin.x + h_pad,
                        bounds.origin.y + (bounds.size.height - text_size.height) / 2.0,
                    ),
                    NSSize::new(available_width, text_size.height),
                )
            };

            unsafe {
                let _: () = msg_send![&attr_str, drawInRect: text_rect];
            };
        }
    }
);

impl FlashMessageView {
    fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        message: &str,
        opacity: f32,
        is_badge: bool,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(FlashMessageViewIvars {
            opacity,
            message: NSString::from_str(message),
            is_badge,
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

pub struct FlashMessageManager {
    mtm: MainThreadMarker,
    window: Option<Retained<NSWindow>>,
}

impl FlashMessageManager {
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self { mtm, window: None }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn show(&mut self, message: &str, opacity: f32, top_right_abs_cg: NSPoint) {
        let is_badge = message.chars().count() <= 2;
        let screen_h = primary_screen_height(self.mtm);

        let size = if is_badge {
            NSSize::new(150.0, 150.0)
        } else {
            let font = NSFont::systemFontOfSize(30.0);
            let font_key = NSString::from_str("NSFont");
            let keys = [&*font_key];
            let objects = [&*font as &AnyObject];
            let attributes = NSDictionary::from_slices(&keys, &objects);
            let msg_ns = NSString::from_str(message);
            let attr_str: Retained<NSAttributedString> = unsafe {
                let alloc = NSAttributedString::alloc();
                msg_send![alloc, initWithString: &*msg_ns, attributes: &*attributes]
            };
            let text_size: NSSize = unsafe { msg_send![&attr_str, size] };
            let horizontal_padding = 48.0;
            let max_width = 780.0;
            let min_width = 140.0;
            let width = (text_size.width + horizontal_padding).clamp(min_width, max_width);
            NSSize::new(width, 64.0)
        };

        let padding = 20.0;

        let cocoa_origin_x = top_right_abs_cg.x - size.width - padding;
        let cocoa_origin_y = screen_h - (top_right_abs_cg.y + size.height + padding);

        let frame = NSRect::new(NSPoint::new(cocoa_origin_x, cocoa_origin_y), size);

        if let Some(window) = &self.window {
            let view = FlashMessageView::new(
                self.mtm,
                NSRect::new(NSPoint::new(0.0, 0.0), size),
                message,
                opacity,
                is_badge,
            );
            window.setContentView(Some(&view));
            window.setFrame_display(frame, true);
            window.orderFront(None::<&AnyObject>);
        } else {
            let window = make_overlay_window(self.mtm, frame, NSFloatingWindowLevel + 1);
            let view = FlashMessageView::new(
                self.mtm,
                NSRect::new(NSPoint::new(0.0, 0.0), size),
                message,
                opacity,
                is_badge,
            );
            window.setContentView(Some(&view));
            window.orderFront(None::<&AnyObject>);
            self.window = Some(window);
        }
    }

    pub fn remove(&mut self) {
        if let Some(window) = self.window.take() {
            window.orderOut(None::<&AnyObject>);
        }
    }
}

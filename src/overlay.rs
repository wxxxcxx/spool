use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSFloatingWindowLevel,
    NSFont, NSGraphicsContext, NSNormalWindowLevel, NSParagraphStyle, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_core_foundation::CGFloat;
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableCopying, NSPoint, NSRect, NSSize, NSString,
};

use crate::platform::WinID;

#[derive(Clone, PartialEq)]
pub struct BorderParams {
    pub color: (f64, f64, f64),
    pub opacity: f64,
    pub width: f64,
    pub radius: f64,
}

/// Parameters for the fullscreen dim + cutout overlay.
#[derive(Clone, PartialEq)]
pub struct DimParams {
    pub opacity: f32,
    pub color: (f64, f64, f64),
    /// The focused window rect to cut out (in Cocoa screen coordinates).
    /// `None` means dim everything (no focused window).
    pub cutout: Option<NSRect>,
    pub cutout_radius: f64,
}

// ── DimView: fullscreen dark overlay with a transparent cutout ──

#[derive(Debug, Clone)]
struct DimViewIvars {
    opacity: f32,
    dim_r: f64,
    dim_g: f64,
    dim_b: f64,
    // Cutout rect in the view's local coordinates.
    cutout_x: f64,
    cutout_y: f64,
    cutout_w: f64,
    cutout_h: f64,
    has_cutout: bool,
    cutout_radius: f64,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolDimView"]
    #[ivars = DimViewIvars]
    #[derive(Debug)]
    struct DimView;

    impl DimView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let ivars = self.ivars();
            let bounds = self.bounds();

            // Fill the entire view with the dim color.
            let dim_color = NSColor::colorWithSRGBRed_green_blue_alpha(
                ivars.dim_r as CGFloat,
                ivars.dim_g as CGFloat,
                ivars.dim_b as CGFloat,
                CGFloat::from(ivars.opacity),
            );
            dim_color.setFill();
            NSBezierPath::fillRect(bounds);

            if ivars.has_cutout {
                let cutout = NSRect::new(
                    NSPoint::new(ivars.cutout_x, ivars.cutout_y),
                    NSSize::new(ivars.cutout_w, ivars.cutout_h),
                );

                // Punch a rounded transparent hole using Clear compositing.
                if let Some(ctx) = NSGraphicsContext::currentContext() {
                    ctx.setCompositingOperation(NSCompositingOperation::Clear);
                    let hole = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                        cutout,
                        ivars.cutout_radius as CGFloat,
                        ivars.cutout_radius as CGFloat,
                    );
                    hole.fill();
                    ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
                }
            }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl DimView {
    fn new(mtm: MainThreadMarker, frame: NSRect, params: &DimParams) -> Retained<Self> {
        let (has_cutout, cx, cy, cw, ch) = params.cutout.map_or((false, 0.0, 0.0, 0.0, 0.0), |r| {
            (true, r.origin.x, r.origin.y, r.size.width, r.size.height)
        });
        let this = Self::alloc(mtm).set_ivars(DimViewIvars {
            opacity: params.opacity,
            dim_r: params.color.0,
            dim_g: params.color.1,
            dim_b: params.color.2,
            cutout_x: cx,
            cutout_y: cy,
            cutout_w: cw,
            cutout_h: ch,
            has_cutout,
            cutout_radius: params.cutout_radius,
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

#[derive(Debug, Clone)]
struct BorderViewIvars {
    border_x: f64,
    border_y: f64,
    border_w: f64,
    border_h: f64,
    color_r: f64,
    color_g: f64,
    color_b: f64,
    opacity: f64,
    width: f64,
    radius: f64,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolBorderView"]
    #[ivars = BorderViewIvars]
    #[derive(Debug)]
    struct BorderView;

    impl BorderView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let ivars = self.ivars();
            let rect = NSRect::new(
                NSPoint::new(ivars.border_x, ivars.border_y),
                NSSize::new(ivars.border_w, ivars.border_h),
            );
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                rect,
                ivars.radius as CGFloat,
                ivars.radius as CGFloat,
            );
            path.setLineWidth(ivars.width as CGFloat);
            let color = NSColor::colorWithSRGBRed_green_blue_alpha(
                ivars.color_r as CGFloat,
                ivars.color_g as CGFloat,
                ivars.color_b as CGFloat,
                ivars.opacity as CGFloat,
            );
            color.setStroke();
            path.stroke();
        }
    }
);

impl BorderView {
    fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        border_rect: NSRect,
        params: &BorderParams,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(BorderViewIvars {
            border_x: border_rect.origin.x,
            border_y: border_rect.origin.y,
            border_w: border_rect.size.width,
            border_h: border_rect.size.height,
            color_r: params.color.0,
            color_g: params.color.1,
            color_b: params.color.2,
            opacity: params.opacity,
            width: params.width,
            radius: params.radius,
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
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
struct BorderSurface {
    /// The compact overlay window frame in global Cocoa coordinates.
    window_frame: NSRect,
    /// The target window border rect in the compact overlay's local coordinates.
    border_rect: NSRect,
}

fn plan_border_surfaces(
    target: NSRect,
    screens: &[NSRect],
    border_width: f64,
) -> Vec<BorderSurface> {
    let half_width = border_width.max(0.0) / 2.0;
    let outer = NSRect::new(
        NSPoint::new(target.origin.x - half_width, target.origin.y - half_width),
        NSSize::new(
            target.size.width + border_width.max(0.0),
            target.size.height + border_width.max(0.0),
        ),
    );

    screens
        .iter()
        .filter_map(|screen| {
            let min_x = outer.origin.x.max(screen.origin.x);
            let min_y = outer.origin.y.max(screen.origin.y);
            let max_x =
                (outer.origin.x + outer.size.width).min(screen.origin.x + screen.size.width);
            let max_y =
                (outer.origin.y + outer.size.height).min(screen.origin.y + screen.size.height);
            (max_x > min_x && max_y > min_y).then(|| {
                let window_frame = NSRect::new(
                    NSPoint::new(min_x, min_y),
                    NSSize::new(max_x - min_x, max_y - min_y),
                );
                BorderSurface {
                    window_frame,
                    border_rect: NSRect::new(
                        NSPoint::new(
                            target.origin.x - window_frame.origin.x,
                            target.origin.y - window_frame.origin.y,
                        ),
                        target.size,
                    ),
                }
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
    fn border_surface_is_compact_and_accounts_for_stroke_width() {
        let surfaces = plan_border_surfaces(
            rect(100.0, 100.0, 200.0, 120.0),
            &[rect(0.0, 0.0, 500.0, 500.0)],
            4.0,
        );

        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].window_frame, rect(98.0, 98.0, 204.0, 124.0));
        assert_eq!(surfaces[0].border_rect, rect(2.0, 2.0, 200.0, 120.0));
    }

    #[test]
    fn border_surface_splits_at_a_display_boundary_without_moving_the_path() {
        let surfaces = plan_border_surfaces(
            rect(450.0, 100.0, 100.0, 120.0),
            &[rect(0.0, 0.0, 500.0, 500.0), rect(500.0, 0.0, 500.0, 500.0)],
            4.0,
        );

        assert_eq!(surfaces.len(), 2);
        assert_eq!(surfaces[0].window_frame, rect(448.0, 98.0, 52.0, 124.0));
        assert_eq!(surfaces[0].border_rect, rect(2.0, 2.0, 100.0, 120.0));
        assert_eq!(surfaces[1].window_frame, rect(500.0, 98.0, 52.0, 124.0));
        assert_eq!(surfaces[1].border_rect, rect(-50.0, 2.0, 100.0, 120.0));
    }
}

// ── Overlay window factory ──────────────────────────────────────────────

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
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenNone,
    );

    window
}
// ── OverlayManager ──────────────────────────────────────────────────────

pub struct OverlayManager {
    mtm: MainThreadMarker,
    /// One dim window per display. macOS will not reliably let a single
    /// window span multiple displays (with "Displays have separate Spaces" it
    /// renders on only one), so each screen gets its own overlay drawn in that
    /// screen's local coordinates. Indexed in lockstep with `NSScreen::screens`.
    dim_overlays: Vec<(Retained<NSWindow>, DimParams)>,
    /// Compact border windows clipped to the displays touched by the target.
    border_overlays: Vec<(Retained<NSWindow>, BorderSurface, BorderParams, WinID)>,
    hidden: bool,
}

impl OverlayManager {
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self {
            mtm,
            dim_overlays: Vec::new(),
            border_overlays: Vec::new(),
            hidden: false,
        }
    }

    /// Update the per-display overlays.
    /// `focused_abs_cg` is the focused window rect in absolute CG coords,
    /// or `None` if no window is focused.
    pub fn update(
        &mut self,
        dim_opacity: f32,
        dim_color: (f64, f64, f64),
        focused_abs_cg: Option<NSRect>,
        focused_window_id: Option<WinID>,
        border: Option<&BorderParams>,
    ) {
        let screen_h = primary_screen_height(self.mtm);
        let screens = NSScreen::screens(self.mtm);
        let screen_frames: Vec<NSRect> = (&screens)
            .into_iter()
            .map(|screen| screen.frame())
            .collect();

        // The focused window in Cocoa global coords (shared across all screens).
        let focused_cocoa = focused_abs_cg.map(|cg| cg_abs_to_cocoa(cg, screen_h));
        self.update_dim_overlays(
            dim_opacity,
            dim_color,
            focused_cocoa,
            focused_window_id,
            border,
            &screen_frames,
        );
        self.update_border_overlays(focused_cocoa, focused_window_id, border, &screen_frames);
        self.hidden = false;
    }

    fn update_dim_overlays(
        &mut self,
        dim_opacity: f32,
        dim_color: (f64, f64, f64),
        focused_cocoa: Option<NSRect>,
        focused_window_id: Option<WinID>,
        border: Option<&BorderParams>,
        screen_frames: &[NSRect],
    ) {
        let target_window_number = focused_window_id.and_then(|id| isize::try_from(id).ok());

        if let Some(target_window_number) = target_window_number.filter(|_| dim_opacity != 0.0) {
            // A display was added/removed — tear down and rebuild from scratch.
            if self.dim_overlays.len() != screen_frames.len() {
                for (window, _) in self.dim_overlays.drain(..) {
                    window.orderOut(None::<&AnyObject>);
                }
            }

            for (i, frame) in screen_frames.iter().copied().enumerate() {
                // Cut out the focused window on every display it touches, each
                // in that display's local (flipped, top-left origin) coords.
                let cutout_local =
                    focused_cocoa
                        .filter(|wc| rects_intersect(*wc, frame))
                        .map(|wc| {
                            NSRect::new(
                                NSPoint::new(
                                    wc.origin.x - frame.origin.x,
                                    (frame.origin.y + frame.size.height)
                                        - (wc.origin.y + wc.size.height),
                                ),
                                wc.size,
                            )
                        });

                let params = DimParams {
                    opacity: dim_opacity,
                    color: dim_color,
                    cutout: cutout_local,
                    cutout_radius: border.map_or(0.0, |params| params.radius),
                };

                if let Some((window, stored)) = self.dim_overlays.get_mut(i) {
                    if *stored == params {
                        window.setFrame_display(frame, false);
                    } else {
                        let view = DimView::new(self.mtm, frame, &params);
                        window.setContentView(Some(&view));
                        window.setFrame_display(frame, true);
                        *stored = params;
                    }
                    window
                        .orderWindow_relativeTo(NSWindowOrderingMode::Below, target_window_number);
                } else {
                    let window = make_overlay_window(self.mtm, frame, NSNormalWindowLevel);
                    let view = DimView::new(self.mtm, frame, &params);
                    window.setContentView(Some(&view));
                    window
                        .orderWindow_relativeTo(NSWindowOrderingMode::Below, target_window_number);
                    self.dim_overlays.push((window, params));
                }
            }
        } else {
            for (window, _) in self.dim_overlays.drain(..) {
                window.orderOut(None::<&AnyObject>);
            }
        }
    }

    fn update_border_overlays(
        &mut self,
        focused_cocoa: Option<NSRect>,
        focused_window_id: Option<WinID>,
        border: Option<&BorderParams>,
        screen_frames: &[NSRect],
    ) {
        let target_window_number = focused_window_id.and_then(|id| isize::try_from(id).ok());
        let border_surfaces = focused_cocoa
            .zip(border)
            .zip(focused_window_id)
            .map_or_else(Vec::new, |((target, params), _)| {
                plan_border_surfaces(target, screen_frames, params.width)
            });

        while self.border_overlays.len() > border_surfaces.len() {
            if let Some((window, ..)) = self.border_overlays.pop() {
                window.orderOut(None::<&AnyObject>);
            }
        }

        if let (Some(params), Some(target_id), Some(target_number)) =
            (border, focused_window_id, target_window_number)
        {
            for (i, surface) in border_surfaces.into_iter().enumerate() {
                if let Some((window, stored_surface, stored_params, stored_target)) =
                    self.border_overlays.get_mut(i)
                {
                    if *stored_surface != surface
                        || *stored_params != *params
                        || *stored_target != target_id
                    {
                        let view_frame =
                            NSRect::new(NSPoint::new(0.0, 0.0), surface.window_frame.size);
                        let view =
                            BorderView::new(self.mtm, view_frame, surface.border_rect, params);
                        window.setContentView(Some(&view));
                        window.setFrame_display(surface.window_frame, true);
                        *stored_surface = surface;
                        *stored_params = params.clone();
                        *stored_target = target_id;
                    }
                    window.orderWindow_relativeTo(NSWindowOrderingMode::Above, target_number);
                } else {
                    let window =
                        make_overlay_window(self.mtm, surface.window_frame, NSNormalWindowLevel);
                    let view_frame = NSRect::new(NSPoint::new(0.0, 0.0), surface.window_frame.size);
                    let view = BorderView::new(self.mtm, view_frame, surface.border_rect, params);
                    window.setContentView(Some(&view));
                    window.orderWindow_relativeTo(NSWindowOrderingMode::Above, target_number);
                    self.border_overlays
                        .push((window, surface, params.clone(), target_id));
                }
            }
        } else {
            for (window, ..) in self.border_overlays.drain(..) {
                window.orderOut(None::<&AnyObject>);
            }
        }
    }

    pub fn remove_all(&mut self) {
        for (window, _) in self.dim_overlays.drain(..) {
            window.orderOut(None::<&AnyObject>);
        }
        for (window, ..) in self.border_overlays.drain(..) {
            window.orderOut(None::<&AnyObject>);
        }
        self.hidden = false;
    }

    pub fn hide_all(&mut self) {
        if self.hidden {
            return;
        }
        for (window, _) in &self.dim_overlays {
            window.orderOut(None::<&AnyObject>);
        }
        for (window, ..) in &self.border_overlays {
            window.orderOut(None::<&AnyObject>);
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

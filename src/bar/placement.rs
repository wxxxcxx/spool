//! Where the Bar lives: the menu-bar band, and the handle that outlives it.
//!
//! The Bar used to be a free-floating panel whose width and inset came from
//! configuration. It now takes over the menu bar exactly, so placement is one
//! rect plus the notch the content has to work around. The panel window is that
//! band plus the handle's overhang below it — the one part of the Bar that stays
//! on screen once the band has slid away.

use super::layout::{BarSurface, Rect};
use super::preferences::NotchSide;

/// The menu-bar band's height for one screen.
///
/// The top inset of the visible frame is the menu bar plus, on a notched
/// display, the safe area; the larger of the two observations wins.
pub fn menu_height(screen: Rect, visible: Rect, safe_top: f64, fallback: f64) -> f64 {
    let inset = screen.y + screen.height - visible.y - visible.height;
    let observed = inset.max(safe_top);
    if observed > 0.0 {
        observed.clamp(18.0, 64.0)
    } else {
        fallback.clamp(18.0, 64.0)
    }
}

/// The Bar's panel: the full width of its display, the menu-bar height, flush
/// with the screen top. No inset, no rounding — it *is* the menu bar band.
#[must_use]
pub fn panel_rect(screen: Rect, menu_height: f64) -> Rect {
    let height = menu_height.clamp(1.0, screen.height.max(1.0));
    Rect {
        x: screen.x,
        y: screen.y + screen.height - height,
        width: screen.width.max(1.0),
        height,
    }
}

impl BarSurface {
    /// The gap the Bar's content must keep clear: the Notch, plus how far the
    /// Bar Handle reaches past it on each side.
    ///
    /// The collapsed Bar is a collar around the Notch, and that collar is drawn
    /// in the band — so a Space lane hugging the physical Notch would sit under
    /// it, with its label and icons showing through the collar's ears and its
    /// clicks going to the handle instead of the Space. The lanes hug this gap
    /// instead, and the content is clipped to it.
    #[must_use]
    pub fn keep_out(&self, handle_height: f64) -> Option<Rect> {
        let notch = self.notch?;
        let margin = handle_height.clamp(0.0, self.width / 2.0);
        let left = (notch.x - margin).max(0.0);
        let right = (notch.x + notch.width + margin).min(self.width);
        Some(Rect {
            x: left,
            width: (right - left).max(1.0),
            ..notch
        })
    }
}

/// The drawing surface for a panel, with the notch marked in
/// panel-local coordinates.
///
/// A display without a notch gets a single run of Spaces; one with a notch
/// gets two lanes, one on each side of `gap`.
#[must_use]
pub fn surface(panel: Rect, gap: Option<Rect>, bias: NotchSide) -> BarSurface {
    BarSurface {
        width: panel.width,
        notch: gap.filter(|gap| gap.width > 0.0).map(|gap| Rect {
            x: gap.x - panel.x,
            y: 0.0,
            width: gap.width,
            height: panel.height,
        }),
        bias,
    }
}

/// The Bar Handle's own geometry, resolved from the user's preferences.
///
/// One description, because the handle *is* its height: on a notched display the
/// same number is how far the collar reaches past the notch on every side, and
/// on any display it is what the panel window hangs below the band by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandleMetrics {
    /// How wide the handle is on a display without a notch. A notched display
    /// gets a collar the width of the notch plus [`HandleMetrics::height`] a
    /// side instead, because the middle of the menu-bar band there is the camera
    /// housing: a tab centred inside it would be on pixels that do not exist.
    pub width: f64,
    /// How far the handle hangs below the Bar's bottom edge.
    pub height: f64,
    /// The handle's bottom corners. Its top edge never rounds: it is glued to
    /// the Bar above it.
    pub radius: f64,
}

impl HandleMetrics {
    /// The shape the Bar ships with: the width a plain display gets, and the
    /// height and corner the user's preferences default to.
    pub const DEFAULT: Self = Self {
        width: 64.0,
        height: 5.0,
        radius: 2.5,
    };
    /// The narrowest and widest handle a preference may ask for, and the
    /// matching bounds for its corners: below 4pt it is a hairline the pointer
    /// cannot reliably hit, above 24pt it is thicker than most menu bars, and a
    /// corner radius past the handle's own height has nothing left to round.
    pub const HEIGHT_RANGE: (f64, f64) = (4.0, 24.0);
    /// How much the handle grows under the pointer, as a fraction of its own
    /// size: an eighth wider, a third taller. Proportional, so a handle the user
    /// makes slimmer or taller keeps the feel the default was tuned for.
    ///
    /// The width half only applies to the plain tab, whose width *is* its own
    /// size. A collar's width is mostly the Notch, so scaling it would slide the
    /// ears sideways over the neighbouring Spaces; it thickens by the height's
    /// share instead, which always fits inside the gap the lanes keep clear.
    pub const HOVER_GROWTH: (f64, f64) = (0.125, 0.30);

    /// Resolves a preference into geometry, clamping everything the renderer
    /// cannot draw sensibly. Finite values are already guaranteed by
    /// [`super::preferences::BarPreferences::validate`].
    #[must_use]
    pub fn resolve(height: f64, radius: f64) -> Self {
        let height = height.clamp(Self::HEIGHT_RANGE.0, Self::HEIGHT_RANGE.1);
        Self {
            width: Self::DEFAULT.width,
            height,
            radius: radius.clamp(0.0, height),
        }
    }

    /// The extra width and height the pointer reveals. Split evenly about the
    /// handle's centre so it never shifts sideways.
    #[must_use]
    pub fn hover_growth(self) -> (f64, f64) {
        (
            self.width * Self::HOVER_GROWTH.0,
            self.height * Self::HOVER_GROWTH.1,
        )
    }
}

/// The menu-bar band at `progress` (1 = expanded), in panel-local coordinates.
///
/// Collapsing slides the band up out of the screen by its own height, so the
/// bar leaves through the screen's top edge and its bottom edge — with the
/// content riding on it — comes to rest there.
#[must_use]
pub fn band_rect(panel: (f64, f64), progress: f64) -> Rect {
    let (width, height) = (panel.0.max(1.0), panel.1.max(1.0));
    Rect {
        x: 0.0,
        y: -(1.0 - progress.clamp(0.0, 1.0)) * height,
        width,
        height,
    }
}

/// The collapsed Bar, in panel-local coordinates: the same space as
/// [`BarSurface::notch`], with the origin at the panel's top-left corner and
/// `y` growing downwards, because a `BarView` is flipped.
///
/// A display with a notch gets a collar hugging it — the notch plus the handle's
/// height on each side and below it — which does not move: the notch is a hole in
/// the hardware, so the Bar has to stay around it while the band leaves through
/// the screen's top edge. A display without one gets a small tab that rides up
/// with the band and comes to rest flush with the screen top.
#[must_use]
pub fn handle_rect(
    panel: (f64, f64),
    notch: Option<Rect>,
    progress: f64,
    handle: HandleMetrics,
) -> Rect {
    let (width, height) = (panel.0.max(1.0), panel.1.max(1.0));
    match notch {
        None => {
            let tab = handle.width.min(width);
            Rect {
                x: (width - tab) / 2.0,
                // Expanded it hangs below the band; collapsed it has ridden up
                // with the band, which moves it by the band's own height.
                y: progress.clamp(0.0, 1.0) * height,
                width: tab,
                height: handle.height,
            }
        }
        Some(notch) => {
            let collar = (notch.width + handle.height * 2.0).clamp(1.0, width);
            let centre = notch.x + notch.width / 2.0;
            Rect {
                x: (centre - collar / 2.0).clamp(0.0, (width - collar).max(0.0)),
                y: 0.0,
                width: collar,
                height: height + handle.height,
            }
        }
    }
}

/// The handle's grown rect: the same top edge, the same centre, and the growth
/// [`HandleMetrics::hover_growth`] asks for.
///
/// `notched` picks the collar's growth: it thickens by the height's share on
/// each side rather than by a share of its own (mostly-Notch) width.
///
/// It grows *away* from the edge it is glued to — the Bar's bottom edge while
/// expanded, the screen's top edge once collapsed — because that join has to
/// stay exactly where it is for the handle to read as part of the Bar. On a
/// notched display the same growth thickens the collar's ears and its chin.
#[must_use]
pub fn grown_handle_rect(resting: Rect, handle: HandleMetrics, notched: bool) -> Rect {
    let (scaled_width, grow_height) = handle.hover_growth();
    let grow_width = if notched {
        grow_height * 2.0
    } else {
        scaled_width
    };
    Rect {
        x: resting.x - grow_width / 2.0,
        y: resting.y,
        width: resting.width + grow_width,
        height: resting.height + grow_height,
    }
}

/// How far below the menu-bar band the panel window reaches: the handle at rest,
/// plus the room it grows into under the pointer.
///
/// The headroom matters: a view clips its drawing, so a window that only just
/// held the resting handle would slice the rounded bottom off the moment the
/// handle grew, leaving it square-cornered.
#[must_use]
pub fn window_overhang(handle: HandleMetrics) -> f64 {
    handle.height + handle.hover_growth().1
}

/// The window the Bar is drawn in: the menu-bar band plus [`window_overhang`]
/// below it.
///
/// The window frame never moves — only what is drawn inside it does — which is
/// what keeps the blur behind the Bar from being recomputed every frame.
#[must_use]
pub fn window_rect(panel: Rect, handle: HandleMetrics) -> Rect {
    let overhang = window_overhang(handle);
    Rect {
        y: panel.y - overhang,
        height: panel.height + overhang,
        ..panel
    }
}

/// The physical notch between the two auxiliary top areas, if the
/// display has one.
#[must_use]
pub fn notch_gap(left: Rect, right: Rect, menu_height: f64) -> Option<Rect> {
    let start = left.x + left.width;
    (left.width > 0.0 && right.width > 0.0 && right.x > start).then_some(Rect {
        x: start,
        y: 0.0,
        width: right.x - start,
        height: menu_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1470.0,
            height: 956.0,
        }
    }

    #[test]
    fn the_panel_is_exactly_the_menu_bar_band() {
        for menu_height in [22.0, 24.0, 34.0, 37.0] {
            let frame = panel_rect(screen(), menu_height);
            assert!((frame.x - 0.0).abs() < f64::EPSILON, "flush with the left");
            assert!((frame.width - 1470.0).abs() < f64::EPSILON, "full width");
            assert!((frame.height - menu_height).abs() < f64::EPSILON);
            assert!(
                (frame.y + frame.height - 956.0).abs() < f64::EPSILON,
                "flush with the screen top"
            );
        }
    }

    #[test]
    fn an_offset_display_keeps_its_own_band() {
        // A second display above and to the right of the primary one.
        let other = Rect {
            x: 1470.0,
            y: 200.0,
            width: 1920.0,
            height: 1080.0,
        };
        let frame = panel_rect(other, 24.0);
        assert!((frame.x - 1470.0).abs() < f64::EPSILON);
        assert!((frame.width - 1920.0).abs() < f64::EPSILON);
        assert!((frame.y - 1256.0).abs() < f64::EPSILON);
        assert!((frame.y + frame.height - 1280.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_notch_is_reported_in_panel_local_coordinates() {
        let screen = screen();
        let panel = panel_rect(screen, 34.0);
        let left = Rect {
            x: 0.0,
            y: 922.0,
            width: 646.0,
            height: 34.0,
        };
        let right = Rect { x: 825.0, ..left };
        let gap = notch_gap(left, right, 34.0).expect("a notched display");
        assert!((gap.x - 646.0).abs() < f64::EPSILON);
        assert!((gap.width - 179.0).abs() < f64::EPSILON);

        let layout_surface = surface(panel, Some(gap), NotchSide::Balanced);
        assert!((layout_surface.width - 1470.0).abs() < f64::EPSILON);
        let notch = layout_surface.notch.expect("the notch survives");
        assert!((notch.x - 646.0).abs() < f64::EPSILON, "panel x is 0 here");
        assert!((notch.height - 34.0).abs() < f64::EPSILON);

        // An offset panel shifts the notch with it.
        let offset_panel = Rect { x: 100.0, ..panel };
        let offset_notch = surface(offset_panel, Some(gap), NotchSide::Balanced)
            .notch
            .unwrap();
        assert!((offset_notch.x - 546.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_display_without_auxiliary_areas_has_no_notch() {
        let menu_height = 24.0;
        assert!(notch_gap(Rect::default(), Rect::default(), menu_height).is_none());
        // Overlapping auxiliary areas are not a notch either.
        let left = Rect {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: menu_height,
        };
        let overlapping = Rect { x: 400.0, ..left };
        assert!(notch_gap(left, overlapping, menu_height).is_none());
        let zero = surface(
            panel_rect(screen(), menu_height),
            Some(Rect::default()),
            NotchSide::Balanced,
        );
        assert!(zero.notch.is_none(), "a zero-width gap is not a notch");
    }

    /// The notch of a 1470pt display, sized to the mock menu bar.
    fn gap() -> Rect {
        Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        }
    }

    #[test]
    fn a_collapsed_notched_bar_is_a_collar_around_the_notch() {
        let panel = (1470.0, 34.0);
        let gap = gap();
        let handle = HandleMetrics::DEFAULT;
        for progress in [0.0, 0.5, 1.0] {
            let rect = handle_rect(panel, Some(gap), progress, handle);
            assert!((rect.width - (179.0 + handle.height * 2.0)).abs() < f64::EPSILON);
            assert!(
                (rect.height - (34.0 + handle.height)).abs() < f64::EPSILON,
                "the collar reaches below the band, where the chin shows"
            );
            assert!(
                (rect.x + rect.width / 2.0 - (gap.x + gap.width / 2.0)).abs() < f64::EPSILON,
                "centred on the notch"
            );
            assert!(
                (rect.y - 0.0).abs() < f64::EPSILON,
                "pinned: the notch is a hole in the hardware"
            );
        }
        // A notch near the edge still keeps the collar on its display.
        let edge_gap = Rect {
            x: 1430.0,
            width: 40.0,
            ..gap
        };
        let rect = handle_rect(panel, Some(edge_gap), 0.0, handle);
        assert!(rect.x >= 0.0 && rect.x + rect.width <= 1470.0);
    }

    #[test]
    fn a_plain_handle_hangs_below_the_band_and_rides_up_to_the_screen_top() {
        let panel = (1470.0, 24.0);
        let handle = HandleMetrics::DEFAULT;
        let expanded = handle_rect(panel, None, 1.0, handle);
        assert!((expanded.width - handle.width).abs() < f64::EPSILON);
        assert!((expanded.height - handle.height).abs() < f64::EPSILON);
        assert!(
            (expanded.x + expanded.width / 2.0 - 735.0).abs() < f64::EPSILON,
            "centred on the display"
        );
        assert!(
            (expanded.y - 24.0).abs() < f64::EPSILON,
            "glued to the band's bottom edge"
        );

        let collapsed = handle_rect(panel, None, 0.0, handle);
        assert!(
            (collapsed.y - 0.0).abs() < f64::EPSILON,
            "flush with the top"
        );
        assert_eq!(
            (collapsed.width, collapsed.height, collapsed.x),
            (expanded.width, expanded.height, expanded.x),
            "the handle keeps its size: only its progress changes"
        );

        // Halfway through the slide it is halfway up the band.
        let mid = handle_rect(panel, None, 0.5, handle);
        assert!((mid.y - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_band_leaves_through_the_top_edge() {
        let panel = (1470.0, 24.0);
        let expanded = band_rect(panel, 1.0);
        assert!((expanded.y - 0.0).abs() < f64::EPSILON);
        assert!((expanded.height - 24.0).abs() < f64::EPSILON);

        let collapsed = band_rect(panel, 0.0);
        assert!(
            (collapsed.y + collapsed.height - 0.0).abs() < f64::EPSILON,
            "fully collapsed, the band's bottom edge is the screen's top edge"
        );
    }

    #[test]
    fn the_window_is_the_band_plus_the_handles_overhang() {
        let panel = panel_rect(screen(), 24.0);
        let overhang = window_overhang(HandleMetrics::DEFAULT);
        let window = window_rect(panel, HandleMetrics::DEFAULT);
        assert!((window.x - panel.x).abs() < f64::EPSILON);
        assert!((window.width - panel.width).abs() < f64::EPSILON);
        assert!(
            (window.y - (panel.y - overhang)).abs() < f64::EPSILON,
            "it hangs below the band, so the handle has room outside it"
        );
        assert!((window.height - (panel.height + overhang)).abs() < f64::EPSILON);
    }

    #[test]
    fn the_window_has_room_for_the_handle_to_grow() {
        // A view clips its own drawing, so the window must hold the handle at
        // its grown size, not just its resting one: otherwise hovering slices
        // the rounded bottom off and the handle goes square-cornered.
        let panel = (1470.0, 24.0);
        let handle = HandleMetrics::DEFAULT;
        let overhang = window_overhang(handle);
        assert!(
            overhang > handle.height,
            "the window carries headroom for the hover growth"
        );
        // In panel-local coordinates the window ends where the band ends plus
        // the overhang: the handle's `y` is measured from the band's top edge.
        let window_bottom = panel.1 + overhang;
        let grown = grown_handle_rect(handle_rect(panel, None, 1.0, handle), handle, false);
        assert!(
            grown.y + grown.height <= window_bottom + f64::EPSILON,
            "the grown tab fits: {} > {window_bottom}",
            grown.y + grown.height
        );

        let collar_panel = (1470.0, 34.0);
        let grown_collar = grown_handle_rect(
            handle_rect(collar_panel, Some(gap()), 0.0, handle),
            handle,
            true,
        );
        assert!(
            grown_collar.y + grown_collar.height <= collar_panel.1 + overhang + f64::EPSILON,
            "the grown collar fits too"
        );
    }

    #[test]
    fn a_hovered_handle_grows_away_from_the_edge_it_is_glued_to() {
        let handle = HandleMetrics::DEFAULT;
        let resting = handle_rect((1470.0, 24.0), None, 1.0, handle);
        let grown = grown_handle_rect(resting, handle, false);
        assert!(
            (grown.y - resting.y).abs() < f64::EPSILON,
            "the glued top edge does not move: no seam, and no lift off the Bar"
        );
        assert!(
            (grown.x + grown.width / 2.0 - (resting.x + resting.width / 2.0)).abs() < f64::EPSILON,
            "it grows about its own centre"
        );
        let (grow_width, grow_height) = handle.hover_growth();
        assert!((grown.width - resting.width - grow_width).abs() < f64::EPSILON);
        assert!((grown.height - resting.height - grow_height).abs() < f64::EPSILON);
        assert!(
            (grow_width - 8.0).abs() < f64::EPSILON && (grow_height - 1.5).abs() < f64::EPSILON,
            "the growth is proportional: a slimmer handle grows less"
        );
        // Growing must not break the hit test: the resting shape is inside it.
        assert!(grown.x <= resting.x);
        assert!(grown.y + grown.height >= resting.y + resting.height);

        // The collar thickens rather than scaling: its own width is mostly the
        // Notch, so a share of it would slide the ears over the neighbouring
        // Spaces. Growth per side is the height's share, which the gap the lanes
        // keep clear (the handle's height) always contains.
        let collar = handle_rect((1470.0, 34.0), Some(gap()), 0.0, handle);
        let grown_collar = grown_handle_rect(collar, handle, true);
        assert!((grown_collar.y - collar.y).abs() < f64::EPSILON);
        assert!((collar.x - grown_collar.x - grow_height).abs() < f64::EPSILON);
        assert!(
            (grown_collar.x + grown_collar.width - (collar.x + collar.width) - grow_height).abs()
                < f64::EPSILON
        );
        assert!((grown_collar.height - collar.height - grow_height).abs() < f64::EPSILON);
        assert!(
            grow_height < handle.height,
            "the grown ears stay inside the gap the lanes keep clear"
        );
    }

    #[test]
    fn the_handle_reaches_past_the_notch_on_every_visible_side() {
        // The notch is a hole in the hardware, so the only black the eye can
        // see around a collapsed notched Bar is the collar outside it. It has
        // to clear the notch on the two sides and below, and it does so by the
        // handle's own height.
        let gap = gap();
        let handle = HandleMetrics::DEFAULT;
        let collar = handle_rect((1470.0, 34.0), Some(gap), 0.0, handle);
        assert!(collar.x <= gap.x - handle.height + f64::EPSILON);
        assert!(collar.x + collar.width >= gap.x + gap.width + handle.height - f64::EPSILON);
        assert!(collar.y + collar.height >= gap.y + gap.height + handle.height - f64::EPSILON);
    }

    #[test]
    fn a_preference_is_clamped_to_a_shape_the_renderer_can_draw() {
        // The width has no key, so it is always the default.
        assert!(
            (HandleMetrics::resolve(10.0, 5.0).width - HandleMetrics::DEFAULT.width).abs()
                < f64::EPSILON
        );
        // Height: below the pointer's business, above most menu bars.
        assert!(
            (HandleMetrics::resolve(0.5, 5.0).height - HandleMetrics::HEIGHT_RANGE.0).abs()
                < f64::EPSILON
        );
        assert!(
            (HandleMetrics::resolve(200.0, 5.0).height - HandleMetrics::HEIGHT_RANGE.1).abs()
                < f64::EPSILON
        );
        // The corner can consume the whole handle, and no more: a radius past
        // the height has nothing left to round.
        assert!((HandleMetrics::resolve(10.0, 40.0).radius - 10.0).abs() < f64::EPSILON);
        assert!((HandleMetrics::resolve(10.0, -3.0).radius - 0.0).abs() < f64::EPSILON);
        // The shipped default: half the height it used to be, and the corner
        // with it, so the handle keeps the rounded-rectangle silhouette.
        let default = HandleMetrics::DEFAULT;
        assert!((default.height - 5.0).abs() < f64::EPSILON);
        assert!((default.radius - default.height / 2.0).abs() < f64::EPSILON);
        assert_eq!(
            HandleMetrics::resolve(default.height, default.radius),
            default
        );
    }

    #[test]
    fn the_collar_and_the_window_follow_the_configured_height() {
        // One knob for the handle's thickness: on a notched display that
        // thickness is the ears and the chin, and it is always what the panel
        // window hangs below the band by.
        for height in [4.0, 10.0, 24.0] {
            let handle = HandleMetrics::resolve(height, height / 2.0);
            let collar = handle_rect((1470.0, 34.0), Some(gap()), 0.0, handle);
            assert!((collar.width - (179.0 + height * 2.0)).abs() < f64::EPSILON);
            assert!((collar.height - (34.0 + height)).abs() < f64::EPSILON);
            let tab = handle_rect((1470.0, 24.0), None, 1.0, handle);
            assert!((tab.height - height).abs() < f64::EPSILON);
            let (_, grow_height) = handle.hover_growth();
            assert!((window_overhang(handle) - (height + grow_height)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn menu_height_uses_top_inset_not_bottom_dock_and_handles_hidden_menu() {
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let visible = Rect {
            y: 80.0,
            height: 976.0,
            ..screen
        };
        assert!((menu_height(screen, visible, 0.0, 22.0) - 24.0).abs() < f64::EPSILON);
        assert!((menu_height(screen, screen, 37.0, 22.0) - 37.0).abs() < f64::EPSILON);
        assert!((menu_height(screen, screen, 0.0, 22.0) - 22.0).abs() < f64::EPSILON);
    }
}

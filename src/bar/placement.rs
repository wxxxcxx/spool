//! Where the Bar lives: the menu-bar band, and nothing else.
//!
//! The Bar used to be a free-floating panel whose width and inset came from
//! configuration. It now takes over the menu bar exactly, so placement is one
//! rect plus the camera cutout the content has to work around.

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

/// The drawing surface for a panel, with the camera cutout marked in
/// panel-local coordinates.
///
/// A display without a cutout gets a single run of Spaces; one with a cutout
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

/// How much black capsule sits either side of the cutout when collapsed.
pub const CAPSULE_SIDE: f64 = 24.0;
/// The collapsed tab on a display without a cutout.
pub const PLAIN_TAB_WIDTH: f64 = 120.0;
pub const PLAIN_TAB_HEIGHT: f64 = 6.0;
/// Hovering the tab grows it, which is the only affordance it has.
pub const PLAIN_TAB_HOVER_HEIGHT: f64 = 9.0;

/// How far the collapsed capsule's top corners flare outwards, so the black
/// region spreads into the display's top edge instead of ending on a square
/// corner. With the bottom corners rounded the other way, the side profile
/// reads as an S.
pub const CAPSULE_FLARE: f64 = 9.0;

/// The collapsed Bar, in panel-local coordinates: the same space as
/// [`BarSurface::notch`], with the origin at the panel's top-left corner and
/// `y` growing downwards, because a `BarView` is flipped.
///
/// A display with a cutout gets a black capsule merged with it; one without
/// gets a small top-centred tab. Either way the shape hangs from the top edge,
/// so hovering only ever grows it downwards and never moves it.
///
/// `hovered` only matters for the plain tab, where growing is the click
/// affordance; the capsule keeps its size and brightens its handles instead.
#[must_use]
pub fn collapsed_rect(panel: (f64, f64), notch: Option<Rect>, hovered: bool) -> Rect {
    let (panel_width, panel_height) = (panel.0.max(1.0), panel.1.max(1.0));
    let Some(notch) = notch else {
        let width = PLAIN_TAB_WIDTH.min(panel_width);
        let height = if hovered {
            PLAIN_TAB_HOVER_HEIGHT
        } else {
            PLAIN_TAB_HEIGHT
        };
        return Rect {
            x: (panel_width - width) / 2.0,
            y: 0.0,
            width,
            height,
        };
    };
    let width = (notch.width + CAPSULE_SIDE * 2.0).clamp(1.0, panel_width);
    let centre = notch.x + notch.width / 2.0;
    Rect {
        x: (centre - width / 2.0).clamp(0.0, (panel_width - width).max(0.0)),
        y: 0.0,
        width,
        height: panel_height,
    }
}

/// The rect the pointer must be inside for the collapsed Bar to count as
/// hovered: the grown tab, not the resting one.
///
/// Hover is judged against this rather than against the panel as it currently
/// is, so the tab growing under the pointer cannot drop the pointer out of
/// hover and flip the Bar back and forth every frame.
#[must_use]
pub fn collapsed_hover_rect(panel: (f64, f64), notch: Option<Rect>) -> Rect {
    collapsed_rect(panel, notch, true)
}

/// The physical camera cutout between the two auxiliary top areas, if the
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
    fn the_cutout_is_reported_in_panel_local_coordinates() {
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
        let cutout = layout_surface.notch.expect("the cutout survives");
        assert!((cutout.x - 646.0).abs() < f64::EPSILON, "panel x is 0 here");
        assert!((cutout.height - 34.0).abs() < f64::EPSILON);

        // An offset panel shifts the cutout with it.
        let offset_panel = Rect { x: 100.0, ..panel };
        let offset_cutout = surface(offset_panel, Some(gap), NotchSide::Balanced)
            .notch
            .unwrap();
        assert!((offset_cutout.x - 546.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_display_without_auxiliary_areas_has_no_cutout() {
        let menu_height = 24.0;
        assert!(notch_gap(Rect::default(), Rect::default(), menu_height).is_none());
        // Overlapping auxiliary areas are not a cutout either.
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
        assert!(zero.notch.is_none(), "a zero-width gap is not a cutout");
    }

    #[test]
    fn a_collapsed_notched_bar_is_a_capsule_around_the_cutout() {
        let panel = (1470.0, 34.0);
        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        for hovered in [false, true] {
            let rect = collapsed_rect(panel, Some(gap), hovered);
            assert!((rect.width - (179.0 + CAPSULE_SIDE * 2.0)).abs() < f64::EPSILON);
            assert!((rect.height - 34.0).abs() < f64::EPSILON);
            assert!(
                (rect.x + rect.width / 2.0 - (gap.x + gap.width / 2.0)).abs() < f64::EPSILON,
                "centred on the cutout"
            );
            assert!(
                (rect.y - 0.0).abs() < f64::EPSILON,
                "flush with the panel's top edge"
            );
        }
        // A cutout near the edge still keeps the capsule on its display.
        let edge_gap = Rect {
            x: 1430.0,
            width: 40.0,
            ..gap
        };
        let rect = collapsed_rect(panel, Some(edge_gap), false);
        assert!(rect.x >= 0.0 && rect.x + rect.width <= 1470.0);
    }

    #[test]
    fn the_collapsed_hover_rect_is_the_grown_bar() {
        // The plain tab is judged by the box it grows into, so a growing tab
        // never drops the pointer out of hover.
        let hover = collapsed_hover_rect((1920.0, 24.0), None);
        assert!((hover.height - PLAIN_TAB_HOVER_HEIGHT).abs() < f64::EPSILON);
        assert!((hover.width - PLAIN_TAB_WIDTH).abs() < f64::EPSILON);
        assert_eq!(hover, collapsed_rect((1920.0, 24.0), None, true));
        // A cutout keeps its capsule size whether or not it is hovered, so
        // there is nothing to grow into.
        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        assert_eq!(
            collapsed_hover_rect((1470.0, 34.0), Some(gap)),
            collapsed_rect((1470.0, 34.0), Some(gap), false)
        );
    }

    #[test]
    fn a_collapsed_plain_bar_is_a_small_top_centred_tab() {
        let panel = (1470.0, 24.0);
        let resting = collapsed_rect(panel, None, false);
        assert!((resting.width - PLAIN_TAB_WIDTH).abs() < f64::EPSILON);
        assert!((resting.height - PLAIN_TAB_HEIGHT).abs() < f64::EPSILON);
        assert!((resting.x + resting.width / 2.0 - 735.0).abs() < f64::EPSILON);
        assert!((resting.y - 0.0).abs() < f64::EPSILON, "flush with the top");

        let hovered = collapsed_rect(panel, None, true);
        assert!(hovered.height > resting.height, "hover grows the tab");
        assert!(
            (hovered.width - resting.width).abs() < f64::EPSILON,
            "width is fixed"
        );
        assert!(
            (hovered.y - 0.0).abs() < f64::EPSILON,
            "growth goes downwards, never off the top"
        );
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

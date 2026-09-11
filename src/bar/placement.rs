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

/// The collapsed Bar: a black capsule merged with the camera cutout on a
/// notched display, or a small top-centred tab on a plain one.
///
/// `hovered` only matters for the plain tab, where growing is the click
/// affordance; the capsule keeps its size and brightens its handles instead.
#[must_use]
pub fn collapsed_rect(screen: Rect, menu_height: f64, gap: Option<Rect>, hovered: bool) -> Rect {
    let top = screen.y + screen.height;
    let Some(gap) = gap else {
        let width = PLAIN_TAB_WIDTH.min(screen.width.max(1.0));
        let height = if hovered {
            PLAIN_TAB_HOVER_HEIGHT
        } else {
            PLAIN_TAB_HEIGHT
        };
        return Rect {
            x: screen.x + (screen.width - width) / 2.0,
            y: top - height,
            width,
            height,
        };
    };
    let menu_height = menu_height.min(screen.height.max(1.0));
    let width = (gap.width + CAPSULE_SIDE * 2.0).clamp(1.0, screen.width.max(1.0));
    let centre = gap.x + gap.width / 2.0;
    let x = (centre - width / 2.0).clamp(screen.x, (screen.x + screen.width - width).max(screen.x));
    Rect {
        x,
        y: top - menu_height,
        width,
        height: menu_height,
    }
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
        let screen = screen();
        let gap = Rect {
            x: 646.0,
            y: 922.0,
            width: 179.0,
            height: 34.0,
        };
        for hovered in [false, true] {
            let rect = collapsed_rect(screen, 34.0, Some(gap), hovered);
            assert!((rect.width - (179.0 + CAPSULE_SIDE * 2.0)).abs() < f64::EPSILON);
            assert!((rect.height - 34.0).abs() < f64::EPSILON);
            assert!(
                (rect.x + rect.width / 2.0 - (gap.x + gap.width / 2.0)).abs() < f64::EPSILON,
                "centred on the cutout"
            );
            assert!(
                (rect.y + rect.height - 956.0).abs() < f64::EPSILON,
                "flush with the top"
            );
        }
        // A cutout near the edge still keeps the capsule on its display.
        let edge_gap = Rect {
            x: 1430.0,
            width: 40.0,
            ..gap
        };
        let rect = collapsed_rect(screen, 34.0, Some(edge_gap), false);
        assert!(rect.x >= 0.0 && rect.x + rect.width <= 1470.0);
    }

    #[test]
    fn a_collapsed_plain_bar_is_a_small_top_centred_tab() {
        let screen = screen();
        let resting = collapsed_rect(screen, 24.0, None, false);
        assert!((resting.width - PLAIN_TAB_WIDTH).abs() < f64::EPSILON);
        assert!((resting.height - PLAIN_TAB_HEIGHT).abs() < f64::EPSILON);
        assert!((resting.x + resting.width / 2.0 - 735.0).abs() < f64::EPSILON);
        assert!((resting.y + resting.height - 956.0).abs() < f64::EPSILON);

        let hovered = collapsed_rect(screen, 24.0, None, true);
        assert!(hovered.height > resting.height, "hover grows the tab");
        assert!(
            (hovered.width - resting.width).abs() < f64::EPSILON,
            "width is fixed"
        );
        assert!(
            (hovered.y + hovered.height - 956.0).abs() < f64::EPSILON,
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

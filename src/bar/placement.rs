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

/// The Bar Handle: a small rounded tab at the display's centre. Its square top
/// edge is glued to the Bar above it and its bottom corners round, so it reads
/// as part of the Bar rather than as a separate control.
///
/// This is its width on a display without a notch. A notched display gets a
/// collar the width of the notch plus [`HANDLE_HEIGHT`] a side instead, because
/// the middle of the menu-bar band there is the camera housing: a tab centred
/// inside it would be drawn on pixels that do not exist.
pub const HANDLE_WIDTH: f64 = 64.0;
/// The handle's height, the distance it hangs below the band while expanded,
/// and — on a notched display — how far the collar reaches past the notch on
/// every side. It is also what the panel window hangs below the band by.
pub const HANDLE_HEIGHT: f64 = 10.0;
/// The handle's bottom corners. Its top edge never rounds.
pub const HANDLE_RADIUS: f64 = 5.0;

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
/// A display with a notch gets a collar hugging it — the notch plus
/// [`HANDLE_HEIGHT`] on each side and below it — which does not move: the notch
/// is a hole in the hardware, so the Bar has to stay around it while the band
/// leaves through the screen's top edge. A display without one gets a small tab
/// that rides up with the band and comes to rest flush with the screen top.
#[must_use]
pub fn handle_rect(panel: (f64, f64), notch: Option<Rect>, progress: f64) -> Rect {
    let (width, height) = (panel.0.max(1.0), panel.1.max(1.0));
    match notch {
        None => {
            let tab = HANDLE_WIDTH.min(width);
            Rect {
                x: (width - tab) / 2.0,
                // Expanded it hangs below the band; collapsed it has ridden up
                // with the band, which moves it by the band's own height.
                y: progress.clamp(0.0, 1.0) * height,
                width: tab,
                height: HANDLE_HEIGHT,
            }
        }
        Some(notch) => {
            let collar = (notch.width + HANDLE_HEIGHT * 2.0).clamp(1.0, width);
            let centre = notch.x + notch.width / 2.0;
            Rect {
                x: (centre - collar / 2.0).clamp(0.0, (width - collar).max(0.0)),
                y: 0.0,
                width: collar,
                height: height + HANDLE_HEIGHT,
            }
        }
    }
}

/// How much more of the handle the pointer reveals: extra width and extra
/// height, split evenly about its centre so it never shifts sideways.
///
/// It grows *away* from the edge it is glued to — the Bar's bottom edge while
/// expanded, the screen's top edge once collapsed — because that join has to
/// stay exactly where it is for the handle to read as part of the Bar. On a
/// notched display the same deltas thicken the collar's ears and its chin.
pub const HANDLE_HOVER_GROWTH: (f64, f64) = (8.0, 3.0);

/// The handle's grown rect: the same top edge, the same centre, and
/// [`HANDLE_HOVER_GROWTH`] more of it.
#[must_use]
pub fn grown_handle_rect(resting: Rect) -> Rect {
    Rect {
        x: resting.x - HANDLE_HOVER_GROWTH.0 / 2.0,
        y: resting.y,
        width: resting.width + HANDLE_HOVER_GROWTH.0,
        height: resting.height + HANDLE_HOVER_GROWTH.1,
    }
}

/// How far below the menu-bar band the panel window reaches: the handle at rest,
/// plus the room it grows into under the pointer.
///
/// The headroom matters: a view clips its drawing, so a window that only just
/// held the resting handle would slice the rounded bottom off the moment the
/// handle grew, leaving it square-cornered.
#[must_use]
pub fn window_overhang() -> f64 {
    HANDLE_HEIGHT + HANDLE_HOVER_GROWTH.1
}

/// The window the Bar is drawn in: the menu-bar band plus [`window_overhang`]
/// below it.
///
/// The window frame never moves — only what is drawn inside it does — which is
/// what keeps the blur behind the Bar from being recomputed every frame.
#[must_use]
pub fn window_rect(panel: Rect) -> Rect {
    let overhang = window_overhang();
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

    #[test]
    fn a_collapsed_notched_bar_is_a_collar_around_the_notch() {
        let panel = (1470.0, 34.0);
        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        for progress in [0.0, 0.5, 1.0] {
            let rect = handle_rect(panel, Some(gap), progress);
            assert!((rect.width - (179.0 + HANDLE_HEIGHT * 2.0)).abs() < f64::EPSILON);
            assert!(
                (rect.height - (34.0 + HANDLE_HEIGHT)).abs() < f64::EPSILON,
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
        let rect = handle_rect(panel, Some(edge_gap), 0.0);
        assert!(rect.x >= 0.0 && rect.x + rect.width <= 1470.0);
    }

    #[test]
    fn a_plain_handle_hangs_below_the_band_and_rides_up_to_the_screen_top() {
        let panel = (1470.0, 24.0);
        let expanded = handle_rect(panel, None, 1.0);
        assert!((expanded.width - HANDLE_WIDTH).abs() < f64::EPSILON);
        assert!((expanded.height - HANDLE_HEIGHT).abs() < f64::EPSILON);
        assert!(
            (expanded.x + expanded.width / 2.0 - 735.0).abs() < f64::EPSILON,
            "centred on the display"
        );
        assert!(
            (expanded.y - 24.0).abs() < f64::EPSILON,
            "glued to the band's bottom edge"
        );

        let collapsed = handle_rect(panel, None, 0.0);
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
        let mid = handle_rect(panel, None, 0.5);
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
        let window = window_rect(panel);
        assert!((window.x - panel.x).abs() < f64::EPSILON);
        assert!((window.width - panel.width).abs() < f64::EPSILON);
        assert!(
            (window.y - (panel.y - window_overhang())).abs() < f64::EPSILON,
            "it hangs below the band, so the handle has room outside it"
        );
        assert!((window.height - (panel.height + window_overhang())).abs() < f64::EPSILON);
    }

    #[test]
    fn the_window_has_room_for_the_handle_to_grow() {
        // A view clips its own drawing, so the window must hold the handle at
        // its grown size, not just its resting one: otherwise hovering slices
        // the rounded bottom off and the handle goes square-cornered.
        let panel = (1470.0, 24.0);
        let overhang = window_overhang();
        assert!(
            overhang > HANDLE_HEIGHT,
            "the window carries headroom for the hover growth"
        );
        // In panel-local coordinates the window ends where the band ends plus
        // the overhang: the handle's `y` is measured from the band's top edge.
        let window_bottom = panel.1 + overhang;
        let grown = grown_handle_rect(handle_rect(panel, None, 1.0));
        assert!(
            grown.y + grown.height <= window_bottom + f64::EPSILON,
            "the grown tab fits: {} > {window_bottom}",
            grown.y + grown.height
        );

        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        let collar_panel = (1470.0, 34.0);
        let grown_collar = grown_handle_rect(handle_rect(collar_panel, Some(gap), 0.0));
        assert!(
            grown_collar.y + grown_collar.height <= collar_panel.1 + overhang + f64::EPSILON,
            "the grown collar fits too"
        );
    }

    #[test]
    fn a_hovered_handle_grows_away_from_the_edge_it_is_glued_to() {
        let resting = handle_rect((1470.0, 24.0), None, 1.0);
        let grown = grown_handle_rect(resting);
        assert!(
            (grown.y - resting.y).abs() < f64::EPSILON,
            "the glued top edge does not move: no seam, and no lift off the Bar"
        );
        assert!(
            (grown.x + grown.width / 2.0 - (resting.x + resting.width / 2.0)).abs() < f64::EPSILON,
            "it grows about its own centre"
        );
        assert!((grown.width - resting.width - HANDLE_HOVER_GROWTH.0).abs() < f64::EPSILON);
        assert!((grown.height - resting.height - HANDLE_HOVER_GROWTH.1).abs() < f64::EPSILON);
        // Growing must not break the hit test: the resting shape is inside it.
        assert!(grown.x <= resting.x);
        assert!(grown.y + grown.height >= resting.y + resting.height);

        // The collar grows the same way, which thickens its ears and its chin.
        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        let collar = handle_rect((1470.0, 34.0), Some(gap), 0.0);
        let grown_collar = grown_handle_rect(collar);
        assert!((grown_collar.y - collar.y).abs() < f64::EPSILON);
        assert!(grown_collar.x < collar.x);
        assert!(grown_collar.x + grown_collar.width > collar.x + collar.width);
        assert!(grown_collar.height > collar.height);
    }

    #[test]
    fn the_handle_reaches_past_the_notch_on_every_visible_side() {
        // The notch is a hole in the hardware, so the only black the eye can
        // see around a collapsed notched Bar is the collar outside it. It has
        // to clear the notch on the two sides and below, and it does so by the
        // handle's own height.
        let gap = Rect {
            x: 646.0,
            y: 0.0,
            width: 179.0,
            height: 34.0,
        };
        let collar = handle_rect((1470.0, 34.0), Some(gap), 0.0);
        assert!(collar.x <= gap.x - HANDLE_HEIGHT + f64::EPSILON);
        assert!(collar.x + collar.width >= gap.x + gap.width + HANDLE_HEIGHT - f64::EPSILON);
        assert!(collar.y + collar.height >= gap.y + gap.height + HANDLE_HEIGHT - f64::EPSILON);
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

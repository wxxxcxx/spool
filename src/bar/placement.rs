use super::geometry::PanelOverride;
use super::layout::{BarSurface, Rect};
use super::preferences::BarPreferences;

pub fn menu_height(screen: Rect, visible: Rect, safe_top: f64, fallback: f64) -> f64 {
    let inset = screen.y + screen.height - visible.y - visible.height;
    let observed = inset.max(safe_top);
    if observed > 0.0 {
        observed.clamp(18.0, 64.0)
    } else {
        fallback.clamp(18.0, 64.0)
    }
}

pub fn available_region(
    screen: Rect,
    menu_height: f64,
    notch_side: Option<Rect>,
    preferences: &BarPreferences,
) -> Rect {
    let horizontal = notch_side
        .filter(|side| side.width > 0.0 && side.height > 0.0)
        .map_or(screen, |side| side.intersection(screen));
    let padding = preferences
        .screen_padding
        .clamp(0.0, (horizontal.width / 2.0 - 1.0).max(0.0));
    let top = screen.y + screen.height - preferences.top_offset;
    let height = if preferences.embed_in_menu_bar {
        menu_height - preferences.top_offset
    } else {
        screen.height - preferences.top_offset
    };
    Rect {
        x: horizontal.x + padding,
        y: top - height,
        width: (horizontal.width - 2.0 * padding).max(1.0),
        height: height.max(1.0),
    }
}

pub fn panel_frame(screen: Rect, width: f64, height: f64, drag_x: Option<f64>) -> Rect {
    let width = width.min(screen.width).max(1.0);
    let x = drag_x
        .unwrap_or(screen.x + (screen.width - width) / 2.0)
        .clamp(screen.x, screen.x + screen.width - 1.0);
    let width = width.min(screen.x + screen.width - x);
    let height = height.min(screen.height).max(1.0);
    Rect {
        x,
        y: screen.y + screen.height - height,
        width,
        height,
    }
}

/// Applies a hand-set override on top of the automatic frame.
///
/// The result is clamped to the owning display, so geometry saved against a
/// larger or differently arranged display can never strand the panel
/// off-screen after a resolution or arrangement change.
#[must_use]
pub fn apply_override(base: Rect, display: Rect, panel: Option<PanelOverride>) -> Rect {
    let Some(panel) = panel else {
        return base;
    };
    let width = panel
        .width
        .unwrap_or(base.width)
        .clamp(1.0, display.width.max(1.0));
    let height = panel
        .height
        .unwrap_or(base.height)
        .clamp(1.0, display.height.max(1.0));
    let x = panel.x.unwrap_or(base.x).clamp(
        display.x,
        (display.x + display.width - width).max(display.x),
    );
    let y = panel.y.unwrap_or(base.y).clamp(
        display.y,
        (display.y + display.height - height).max(display.y),
    );
    Rect {
        x,
        y,
        width,
        height,
    }
}

// The native auxiliary regions, not the content midpoint, anchor the spacer.
pub fn balanced_region(
    region: Rect,
    left: Rect,
    right: Rect,
    preferences: &BarPreferences,
) -> Option<(Rect, BarSurface)> {
    let start = left.x + left.width - 6.0;
    let end = right.x + 6.0;
    let minimum_lane = preferences.toolbar_width() + 38.0;
    if left.width <= 0.0
        || right.width <= 0.0
        || end <= start
        || start - region.x < minimum_lane
        || region.x + region.width - end < minimum_lane
    {
        return None;
    }
    let width = preferences
        .width_limit(region.width)
        .max(end - start + 2.0 * minimum_lane)
        .min(region.width);
    let x = ((start + end - width) / 2.0).clamp(region.x, region.x + region.width - width);
    let region = Rect { x, width, ..region };
    Some((
        region,
        BarSurface {
            width,
            notch: Some(Rect {
                x: start - x,
                y: 0.0,
                width: end - start,
                height: preferences.height,
            }),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_spacer_tracks_physical_notch_on_offset_displays_and_width_limits() {
        for origin in [-1800.0, 0.0, 2560.0] {
            for max_width in [0.0, 120.0, 600.0, 4000.0] {
                let preferences = BarPreferences {
                    max_width,
                    ..BarPreferences::default()
                }
                .for_menu_height(37.0);
                let screen = Rect {
                    x: origin,
                    y: -982.0,
                    width: 1512.0,
                    height: 982.0,
                };
                let left = Rect {
                    x: origin,
                    y: -37.0,
                    width: 660.0,
                    height: 37.0,
                };
                let right = Rect {
                    x: origin + 850.0,
                    width: 662.0,
                    ..left
                };
                let region = available_region(screen, 37.0, None, &preferences);
                let (region, surface) = balanced_region(region, left, right, &preferences).unwrap();
                let frame = panel_frame(region, surface.width, 37.0, None);
                let gap = surface.notch.unwrap();
                assert!((frame.x + gap.x - origin - 654.0).abs() < f64::EPSILON);
                assert!((gap.width - 202.0).abs() < f64::EPSILON);
                assert!(gap.x >= preferences.toolbar_width() + 38.0);
                assert!(surface.width - gap.x - gap.width >= 38.0);
                assert!(frame.x >= screen.x && frame.x + frame.width <= screen.x + screen.width);
                assert!((frame.y + frame.height).abs() < f64::EPSILON);
            }
        }
    }

    #[test]
    fn default_bar_is_flush_with_screen_top_and_inside_menu_bar() {
        for (y, menu_height) in [(0.0, 22.0), (1080.0, 24.0), (-900.0, 37.0)] {
            let screen = Rect {
                x: -1920.0,
                y,
                width: 1920.0,
                height: 1080.0,
            };
            let preferences = BarPreferences::default().for_menu_height(menu_height);
            let metrics = preferences.metrics();
            let region = available_region(screen, menu_height, None, &preferences);
            let frame = panel_frame(
                region,
                400.0,
                metrics.icon_size + metrics.vertical_padding * 2.0,
                None,
            );
            assert!(
                (frame.y + frame.height - y - 1080.0).abs() < f64::EPSILON,
                "top gap: {}",
                y + 1080.0 - frame.y - frame.height
            );
            assert!(
                frame.height <= menu_height,
                "height {} exceeds menu bar",
                frame.height
            );
            assert!((frame.height - menu_height).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn notch_region_and_drag_never_escape_the_owning_display() {
        let screen = Rect {
            x: 100.0,
            y: -1000.0,
            width: 1512.0,
            height: 982.0,
        };
        let right = Rect {
            x: 930.0,
            y: -55.0,
            width: 682.0,
            height: 37.0,
        };
        let preferences = BarPreferences::default().for_menu_height(37.0);
        let region = available_region(screen, 37.0, Some(right), &preferences);
        assert!(region.x >= right.x);
        for drag_x in [None, Some(-10000.0), Some(10000.0)] {
            let frame = panel_frame(region, 1000.0, 64.0, drag_x);
            assert!(frame.x >= region.x);
            assert!(frame.x + frame.width <= region.x + region.width);
            assert!((frame.y + frame.height + 18.0).abs() < f64::EPSILON);
            assert!(frame.height <= 37.0);
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

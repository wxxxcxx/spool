//! Direct manipulation of the Bar panel itself: move it, resize its edges.
//!
//! The panel is the only part of the Bar the user places by hand; everything
//! inside it stays automatic. Gesture results are expressed as a
//! [`PanelOverride`] so the same values can be persisted per display.
//!
//! Coordinates: pointer points are view-local with Y growing downwards (the
//! Bar view is flipped), while panel rects are screen rectangles with Y growing
//! upwards. [`apply`] is the single place that converts between the two.

use super::geometry::PanelOverride;
use super::layout::Rect;

/// How close to an outer edge counts as grabbing that edge.
pub const EDGE_GRAB: f64 = 4.0;
/// Narrower than this the toolbar and a Space lane no longer fit.
pub const MIN_PANEL_WIDTH: f64 = 120.0;
pub const MIN_PANEL_HEIGHT: f64 = 18.0;
/// Same bound the preferences use; icons stop growing past it anyway.
pub const MAX_PANEL_HEIGHT: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelGesture {
    Move,
    ResizeLeft,
    ResizeRight,
    ResizeBottom,
}

/// An in-flight gesture: what was grabbed, where, and the rect it started from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelGestureState {
    pub kind: PanelGesture,
    pub origin: (f64, f64),
    pub start: Rect,
}

/// Which part of the panel a press grabbed, if any.
///
/// The grip wins over an edge so a handle drawn hard against the left edge
/// still moves the panel instead of resizing it.
#[must_use]
pub fn hit(
    size: (f64, f64),
    grip: Option<Rect>,
    point: (f64, f64),
    edge: f64,
) -> Option<PanelGesture> {
    if grip.is_some_and(|grip| grip.contains(point.0, point.1)) {
        return Some(PanelGesture::Move);
    }
    let (width, height) = size;
    if height <= 0.0 || width <= 0.0 {
        return None;
    }
    if point.0 <= edge {
        return Some(PanelGesture::ResizeLeft);
    }
    if point.0 >= width - edge {
        return Some(PanelGesture::ResizeRight);
    }
    if point.1 >= height - edge {
        return Some(PanelGesture::ResizeBottom);
    }
    None
}

/// The override a gesture produces once the pointer is at `pointer`.
///
/// Every axis is written, including the ones the gesture did not touch: the
/// panel was placed by hand, so its whole rect becomes explicit.
#[must_use]
pub fn apply(state: PanelGestureState, pointer: (f64, f64), display: Rect) -> PanelOverride {
    let dx = pointer.0 - state.origin.0;
    let dy = pointer.1 - state.origin.1;
    let start = state.start;
    let (mut x, mut y, mut width, mut height) = (start.x, start.y, start.width, start.height);

    width = width.clamp(MIN_PANEL_WIDTH, display.width.max(MIN_PANEL_WIDTH));
    height = height.clamp(
        MIN_PANEL_HEIGHT,
        MAX_PANEL_HEIGHT.min(display.height.max(MIN_PANEL_HEIGHT)),
    );

    match state.kind {
        // The view is flipped, so a downward pointer delta lowers the panel.
        PanelGesture::Move => {
            x = start.x + dx;
            y = start.y - dy;
        }
        // A resized edge holds the opposite edge still, including when the
        // width or height limit stops the drag.
        PanelGesture::ResizeLeft => {
            let right = start.x + start.width;
            width = (start.width - dx).clamp(MIN_PANEL_WIDTH, display.width.max(MIN_PANEL_WIDTH));
            x = right - width;
        }
        PanelGesture::ResizeRight => {
            width = (start.width + dx).clamp(MIN_PANEL_WIDTH, display.width.max(MIN_PANEL_WIDTH));
        }
        PanelGesture::ResizeBottom => {
            let top = start.y + start.height;
            height = (start.height + dy).clamp(
                MIN_PANEL_HEIGHT,
                MAX_PANEL_HEIGHT.min(display.height.max(MIN_PANEL_HEIGHT)),
            );
            y = top - height;
        }
    }

    PanelOverride {
        x: Some(clamp_origin(
            x,
            display.x,
            display.x + display.width - width,
        )),
        y: Some(clamp_origin(
            y,
            display.y,
            display.y + display.height - height,
        )),
        width: Some(width),
        height: Some(height),
    }
}

fn clamp_origin(value: f64, low: f64, high: f64) -> f64 {
    value.clamp(low, high.max(low))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1470.0,
            height: 956.0,
        }
    }

    fn panel() -> Rect {
        Rect {
            x: 500.0,
            y: 900.0,
            width: 400.0,
            height: 34.0,
        }
    }

    #[test]
    fn moving_follows_the_pointer_in_both_axes() {
        let state = PanelGestureState {
            kind: PanelGesture::Move,
            origin: (20.0, 10.0),
            start: panel(),
        };
        // Pointer 30 right and 6 down in view space, where Y grows downwards.
        let panel = apply(state, (50.0, 16.0), display());
        assert_eq!(panel.x, Some(530.0));
        assert_eq!(panel.y, Some(894.0));
        assert_eq!(panel.width, Some(400.0));
        assert_eq!(panel.height, Some(34.0));
    }

    #[test]
    fn dragging_the_bottom_edge_keeps_the_top_edge_still() {
        let state = PanelGestureState {
            kind: PanelGesture::ResizeBottom,
            origin: (200.0, 34.0),
            start: panel(),
        };
        let grown = apply(state, (200.0, 44.0), display());
        assert_eq!(grown.height, Some(44.0));
        assert_eq!(grown.y, Some(890.0));
        assert_eq!(grown.y.map(|y| y + 44.0), Some(934.0), "top edge stays");
    }

    #[test]
    fn height_and_width_stop_at_their_limits_without_moving_the_fixed_edge() {
        let bottom = PanelGestureState {
            kind: PanelGesture::ResizeBottom,
            origin: (200.0, 34.0),
            start: panel(),
        };
        let tallest = apply(bottom, (200.0, 400.0), display());
        assert_eq!(tallest.height, Some(MAX_PANEL_HEIGHT));
        assert_eq!(tallest.y, Some(934.0 - MAX_PANEL_HEIGHT));

        let left = PanelGestureState {
            kind: PanelGesture::ResizeLeft,
            origin: (0.0, 10.0),
            start: panel(),
        };
        let narrowest = apply(left, (1000.0, 10.0), display());
        assert_eq!(narrowest.width, Some(MIN_PANEL_WIDTH));
        assert_eq!(
            narrowest.x.map(|x| x + MIN_PANEL_WIDTH),
            Some(900.0),
            "the right edge stays where it was"
        );
    }

    #[test]
    fn the_panel_never_leaves_its_display() {
        let state = PanelGestureState {
            kind: PanelGesture::Move,
            origin: (0.0, 0.0),
            start: panel(),
        };
        let far = apply(state, (5000.0, -5000.0), display());
        assert_eq!(far.x, Some(1070.0));
        assert_eq!(far.y, Some(922.0));
        let near = apply(state, (-5000.0, 5000.0), display());
        assert_eq!(near.x, Some(0.0));
        assert_eq!(near.y, Some(0.0));
    }

    #[test]
    fn the_grip_outranks_an_edge_it_sits_against() {
        let grip = Rect {
            x: 0.0,
            y: 4.0,
            width: 14.0,
            height: 24.0,
        };
        assert_eq!(
            hit((400.0, 34.0), Some(grip), (2.0, 16.0), EDGE_GRAB),
            Some(PanelGesture::Move)
        );
        assert_eq!(
            hit((400.0, 34.0), Some(grip), (200.0, 32.0), EDGE_GRAB),
            Some(PanelGesture::ResizeBottom)
        );
        assert_eq!(
            hit((400.0, 34.0), Some(grip), (399.0, 16.0), EDGE_GRAB),
            Some(PanelGesture::ResizeRight)
        );
        assert_eq!(
            hit((400.0, 34.0), Some(grip), (200.0, 16.0), EDGE_GRAB),
            None
        );
    }
}

use std::collections::HashSet;

use bevy::math::{IRect, IVec2};

use super::Direction;
use crate::ecs::window_frame::checked_frame_size;
use crate::manager::Display;

#[derive(Clone, Copy, Debug)]
pub(super) enum DisplayTarget {
    Next,
    North,
    South,
}

impl DisplayTarget {
    pub(super) fn from_direction(direction: &Direction) -> Option<Self> {
        match direction {
            Direction::North => Some(Self::North),
            Direction::South => Some(Self::South),
            _ => None,
        }
    }
}

/// Explicit cycling follows the Bar's spatial order. Directional navigation
/// prefers horizontally overlapping screens and never wraps at an edge.
pub(super) fn select_display<'a>(
    source: u32,
    target: DisplayTarget,
    displays: impl IntoIterator<Item = &'a Display>,
) -> Option<u32> {
    let mut displays = displays.into_iter().collect::<Vec<_>>();
    let mut ids = HashSet::new();
    if displays.len() < 2
        || displays.iter().any(|display| {
            checked_frame_size(display.bounds()).is_none() || !ids.insert(display.id())
        })
    {
        return None;
    }
    displays.sort_unstable_by_key(|display| {
        (display.bounds().min.x, display.bounds().min.y, display.id())
    });
    let index = displays.iter().position(|display| display.id() == source)?;
    if matches!(target, DisplayTarget::Next) {
        return Some(displays[(index + 1) % displays.len()].id());
    }
    let source = displays[index];
    displays
        .iter()
        .filter(|display| display.id() != source.id())
        .filter_map(|display| {
            directional_rank(source.bounds(), display.bounds(), target)
                .map(|rank| (rank, display.id()))
        })
        .min_by_key(|&(rank, id)| (rank, id))
        .map(|(_, id)| id)
}

fn directional_rank(
    source: IRect,
    candidate: IRect,
    target: DisplayTarget,
) -> Option<(bool, i64, i64, i64, i64)> {
    let center = |near: i32, far: i32| i64::from(near) + i64::from(far);
    let vertical = center(candidate.min.y, candidate.max.y) - center(source.min.y, source.max.y);
    let (forward, gap) = match target {
        DisplayTarget::North => (
            -vertical,
            i64::from(source.min.y) - i64::from(candidate.max.y),
        ),
        DisplayTarget::South => (
            vertical,
            i64::from(candidate.min.y) - i64::from(source.max.y),
        ),
        DisplayTarget::Next => return None,
    };
    if forward <= 0 {
        return None;
    }
    let horizontal_gap = (i64::from(source.min.x) - i64::from(candidate.max.x))
        .max(i64::from(candidate.min.x) - i64::from(source.max.x))
        .max(0);
    let overlaps = source.min.x < candidate.max.x && candidate.min.x < source.max.x;
    let horizontal =
        (center(candidate.min.x, candidate.max.x) - center(source.min.x, source.max.x)).abs();
    Some((!overlaps, gap.max(0), horizontal_gap, forward, horizontal))
}

pub(super) fn display_at<'a>(
    point: IVec2,
    displays: impl IntoIterator<Item = &'a Display>,
) -> Option<u32> {
    let mut owners = displays.into_iter().filter(|display| {
        let bounds = display.bounds();
        (point.cmpge(bounds.min) & point.cmplt(bounds.max)).all()
    });
    let owner = owners.next()?;
    owners.next().is_none().then_some(owner.id())
}

pub(super) fn visible_frame(viewport: IRect, frame: IRect) -> Option<IRect> {
    checked_frame_size(frame)?;
    let min = viewport.min.max(frame.min);
    let max = viewport.max.min(frame.max);
    (min.x < max.x && min.y < max.y).then_some(IRect { min, max })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u32, x: i32, y: i32) -> Display {
        Display::new(id, IRect::new(x, y, x + 100, y + 100), 0)
    }

    #[test]
    fn display_navigation_cycle_is_independent_of_input_order() {
        let displays = [display(1, 0, 0), display(2, 0, -100), display(3, 100, 0)];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            for (source, target) in [(1, 3), (3, 2), (2, 1)] {
                assert_eq!(
                    select_display(
                        source,
                        DisplayTarget::Next,
                        order.map(|index| &displays[index])
                    ),
                    Some(target)
                );
            }
        }
    }

    #[test]
    fn display_navigation_direction_prefers_near_aligned_screens_without_wrapping() {
        let displays = [
            display(1, 0, 0),
            display(3, 0, -400),
            display(4, 150, -120),
            display(2, 0, -100),
        ];
        assert_eq!(select_display(1, DisplayTarget::North, &displays), Some(2));
        assert_eq!(select_display(1, DisplayTarget::South, &displays), None);
        assert_eq!(select_display(2, DisplayTarget::North, &displays), Some(3));
        assert_eq!(select_display(2, DisplayTarget::South, &displays), Some(1));
        assert_eq!(select_display(3, DisplayTarget::North, &displays), None);
    }

    #[test]
    fn display_navigation_uses_nearest_diagonal_when_no_screen_is_aligned() {
        let displays = [
            display(1, 0, 0),
            display(2, 200, -100),
            display(3, -500, -100),
        ];
        assert_eq!(select_display(1, DisplayTarget::North, &displays), Some(2));
    }

    #[test]
    fn display_navigation_rejects_missing_duplicate_and_invalid_geometry() {
        let source = display(1, 0, 0);
        assert_eq!(select_display(1, DisplayTarget::Next, [&source]), None);
        assert_eq!(
            select_display(1, DisplayTarget::Next, [&source, &source]),
            None
        );
        assert_eq!(
            select_display(9, DisplayTarget::Next, [&source, &display(2, 100, 0)]),
            None
        );
        for bounds in [
            IRect {
                min: IVec2::ONE,
                max: IVec2::ZERO,
            },
            IRect::new(i32::MIN, 0, i32::MAX, 100),
        ] {
            assert_eq!(
                select_display(
                    1,
                    DisplayTarget::Next,
                    [&source, &Display::new(2, bounds, 0)]
                ),
                None
            );
        }
    }

    #[test]
    fn display_navigation_ties_use_display_identity() {
        let displays = [display(4, 0, -100), display(1, 0, 0), display(2, 0, -100)];
        assert_eq!(select_display(1, DisplayTarget::North, &displays), Some(2));
        assert_eq!(select_display(1, DisplayTarget::Next, &displays), Some(2));
        assert_eq!(select_display(2, DisplayTarget::Next, &displays), Some(4));
    }

    #[test]
    fn display_navigation_distances_do_not_overflow_at_integer_limits() {
        let displays = [
            display(1, i32::MIN, i32::MIN),
            display(2, i32::MAX - 100, i32::MAX - 100),
        ];
        assert_eq!(select_display(1, DisplayTarget::South, &displays), Some(2));
        assert_eq!(select_display(2, DisplayTarget::North, &displays), Some(1));
    }

    #[test]
    fn display_navigation_cursor_ownership_is_half_open_and_unique() {
        let displays = [display(1, 0, 0), display(2, 100, 0)];
        assert_eq!(display_at(IVec2::new(100, 50), &displays), Some(2));
        assert_eq!(display_at(IVec2::new(200, 50), &displays), None);
        assert_eq!(
            display_at(IVec2::new(50, 50), [&displays[0], &display(3, 0, 0)]),
            None
        );
    }

    #[test]
    fn display_navigation_visible_frames_require_positive_overlap() {
        let viewport = IRect::new(-100, -100, 0, 0);
        assert_eq!(
            visible_frame(viewport, IRect::new(-50, -50, 50, 50)),
            Some(IRect::new(-50, -50, 0, 0))
        );
        assert_eq!(visible_frame(viewport, IRect::new(0, -100, 50, 0)), None);
        assert_eq!(visible_frame(viewport, IRect::new(-50, 0, 0, 50)), None);
        assert_eq!(
            visible_frame(
                viewport,
                IRect {
                    min: IVec2::ONE,
                    max: IVec2::ZERO
                }
            ),
            None
        );
    }
}

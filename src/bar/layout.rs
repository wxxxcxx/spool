use spool_shared_types::commands::Placement;
use spool_shared_types::state::SpaceKind;

use super::model::{BarColumn, BarDisplay, BarSpace, BarWindow};
use super::toolbar::TOOLBAR_WIDTH;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn inset(self, amount: f64) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - amount * 2.0).max(0.0),
            height: (self.height - amount * 2.0).max(0.0),
        }
    }

    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        if x >= (self.x + self.width).min(other.x + other.width)
            || y >= (self.y + self.height).min(other.y + other.height)
        {
            return Self::default();
        }
        Self {
            x,
            y,
            width: ((self.x + self.width).min(other.x + other.width) - x).max(0.0),
            height: ((self.y + self.height).min(other.y + other.height) - y).max(0.0),
        }
    }
    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && x >= self.x
            && x <= self.x + self.width
            && y >= self.y
            && y <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarMetrics {
    pub toolbar_width: f64,
    pub icon_size: f64,
    pub icon_gap: f64,
    pub item_gap: f64,
    pub vertical_padding: f64,
    pub horizontal_padding: f64,
    pub label_width: f64,
    pub collapsed_width: f64,
}

impl Default for BarMetrics {
    fn default() -> Self {
        Self {
            toolbar_width: TOOLBAR_WIDTH,
            icon_size: 22.0,
            icon_gap: 3.0,
            item_gap: 5.0,
            vertical_padding: 6.0,
            horizontal_padding: 6.0,
            label_width: 24.0,
            collapsed_width: 38.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub width: f64,
    pub height: f64,
    pub content_width: f64,
    pub content_left: f64,
    pub split: Option<NotchSplit>,
    pub items: Vec<PlacedItem>,
}

#[derive(Clone, Copy, Debug)]
pub struct BarSurface {
    pub width: f64,
    pub notch: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotchSplit {
    pub gap: Rect,
    pub left_spaces: Vec<u64>,
}

impl NotchSplit {
    pub fn viewport(&self, space_id: u64, width: f64, height: f64, toolbar: f64) -> Rect {
        let (x, end) = if self.left_spaces.contains(&space_id) {
            (toolbar, self.gap.x)
        } else {
            (self.gap.x + self.gap.width, width)
        };
        Rect {
            x,
            y: 0.0,
            width: (end - x).max(0.0),
            height,
        }
    }
}

#[cfg(test)]
mod notch_tests {
    use super::*;

    pub(super) fn surface() -> BarSurface {
        BarSurface {
            width: 600.0,
            notch: Some(Rect {
                x: 220.0,
                y: 0.0,
                width: 160.0,
                height: 34.0,
            }),
        }
    }

    #[test]
    fn creation_deletion_and_reordering_balance_counts_in_native_order() {
        let template = tests::display();
        let mut display = template.clone();
        display.spaces.clear();
        for count in 0..12 {
            assert_balanced(&display);
            let mut space = template.spaces[count % 3].clone();
            space.id = u64::try_from(count).unwrap() + 100;
            display.spaces.push(space);
        }
        display.spaces.reverse();
        assert_balanced(&display);
        while !display.spaces.is_empty() {
            display.spaces.remove(display.spaces.len() / 2);
            assert_balanced(&display);
        }
    }

    fn assert_balanced(display: &BarDisplay) {
        let layout =
            BarLayout::resolve_surface(display, surface(), &mut [0.0; 2], BarMetrics::default());
        let split = layout.split.as_ref().unwrap();
        let left = display.spaces.len().div_ceil(2);
        assert_eq!(
            split.left_spaces,
            display.spaces[..left]
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>()
        );
        assert!(left.abs_diff(display.spaces.len() - left) <= 1);
        let ordered_ids = layout
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Space { space_id, .. } => Some(space_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_ids,
            display.spaces.iter().map(|s| s.id).collect::<Vec<_>>()
        );
        assert!((layout.width - surface().width).abs() < f64::EPSILON);
        assert!((layout.height - 34.0).abs() < f64::EPSILON);
    }

    #[test]
    fn width_does_not_change_membership_and_scroll_is_independent() {
        let mut display = tests::display();
        display.spaces[2].visible = true;
        for id in 100..140 {
            let mut window = display.spaces[1].floating[0].clone();
            window.id = id;
            display.spaces[1].floating.push(window.clone());
            window.id += 100;
            display.spaces[2].floating.push(window);
        }
        let initial =
            BarLayout::resolve_surface(&display, surface(), &mut [0.0; 2], BarMetrics::default());
        for lane in 0..2 {
            let mut scroll = [0.0; 2];
            scroll[lane] = f64::MAX;
            let scrolled =
                BarLayout::resolve_surface(&display, surface(), &mut scroll, BarMetrics::default());
            assert!(scroll[lane] > 0.0);
            assert_eq!(initial.split, scrolled.split);
            for (a, b) in initial.items.iter().zip(&scrolled.items) {
                let is_left = matches!(
                    a.kind,
                    ItemKind::Space {
                        space_id: 10 | 11,
                        ..
                    } | ItemKind::Window {
                        space_id: 10 | 11,
                        ..
                    } | ItemKind::Label {
                        space_id: 10 | 11,
                        ..
                    } | ItemKind::Placeholder { space_id: 10 | 11 }
                        | ItemKind::Focus { space_id: 10 | 11 }
                        | ItemKind::ColumnDrop {
                            space_id: 10 | 11,
                            ..
                        }
                );
                if is_left != (lane == 0) {
                    assert_eq!(a.rect, b.rect);
                }
            }
        }
        for space in &mut display.spaces {
            space.visible = !space.visible;
            space.focused = !space.focused;
            space.floating.clear();
        }
        let changed =
            BarLayout::resolve_surface(&display, surface(), &mut [0.0; 2], BarMetrics::default());
        assert_eq!(initial.split, changed.split);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedItem {
    pub rect: Rect,
    pub kind: ItemKind,
}

impl PlacedItem {
    pub fn space_background_rect(&self) -> Rect {
        // Painting is inset; the Space hit region still includes its blank padding.
        let padding = 3.0_f64.min(self.rect.height / 2.0);
        Rect {
            y: self.rect.y + padding,
            height: (self.rect.height - padding * 2.0).max(0.0),
            ..self.rect.inset(0.5)
        }
    }

    pub fn fullscreen_badge_rect(&self) -> Option<Rect> {
        let rect = self.rect;
        match self.kind {
            ItemKind::Window {
                fullscreen: true, ..
            } => {
                let size = (rect.width / 2.0).min(9.0);
                Some(Rect {
                    x: rect.x + rect.width - size - 1.0,
                    y: rect.y + rect.height - size - 1.0,
                    width: size,
                    height: size,
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Label {
        space_id: u64,
        ordinal: u32,
    },
    Placeholder {
        space_id: u64,
    },
    Focus {
        space_id: u64,
    },
    Space {
        space_id: u64,
        ordinal: u32,
        visible: bool,
        focused: bool,
        empty: bool,
        fullscreen: bool,
    },
    Window {
        window_id: i32,
        space_id: u64,
        column_window_id: Option<i32>,
        floating: bool,
        focused: bool,
        bundle_id: String,
        title: String,
        collapsed: bool,
        stacked: bool,
        fullscreen: bool,
    },
    ColumnDrop {
        space_id: u64,
        anchor_window_id: i32,
        placement: Placement,
    },
}

impl BarLayout {
    pub fn resolve_surface(
        display: &BarDisplay,
        surface: BarSurface,
        scroll: &mut [f64; 2],
        metrics: BarMetrics,
    ) -> Self {
        let Some(gap) = surface.notch else {
            let layout = Self::resolve_with_metrics(display, surface.width, scroll[0], metrics);
            scroll[0] = scroll[0].clamp(0.0, (layout.content_width - layout.width).max(0.0));
            scroll[1] = 0.0;
            return layout;
        };
        // Membership depends only on native order/count, never on rendered widths.
        let boundary = display.spaces.len().div_ceil(2);
        let mut left = display.clone();
        let right_spaces = left.spaces.split_off(boundary);
        let right = BarDisplay {
            spaces: right_spaces,
            ..display.clone()
        };
        let mut a = Self::resolve_with_metrics(&left, gap.x, scroll[0], metrics);
        let right_x = gap.x + gap.width;
        let b = Self::resolve_with_metrics(
            &right,
            surface.width - right_x,
            scroll[1],
            BarMetrics {
                toolbar_width: 0.0,
                ..metrics
            },
        );
        scroll[0] = scroll[0].clamp(0.0, (a.content_width - a.width).max(0.0));
        scroll[1] = scroll[1].clamp(0.0, (b.content_width - b.width).max(0.0));
        // Compact content hugs the spacer; overflowing content uses the full lane.
        let left_offset = (gap.x - a.width).max(0.0);
        for item in &mut a.items {
            item.rect.x += left_offset;
        }
        a.items.extend(b.items.into_iter().map(|mut item| {
            item.rect.x += right_x;
            item
        }));
        a.width = surface.width;
        a.content_width += gap.width + b.content_width;
        a.split = Some(NotchSplit {
            gap,
            left_spaces: left.spaces.iter().map(|space| space.id).collect(),
        });
        a
    }

    #[cfg(test)]
    #[must_use]
    pub fn resolve(display: &BarDisplay, max_width: f64, scroll_x: f64) -> Self {
        Self::resolve_with_metrics(display, max_width, scroll_x, BarMetrics::default())
    }

    #[must_use]
    pub fn resolve_with_metrics(
        display: &BarDisplay,
        max_width: f64,
        scroll_x: f64,
        metrics: BarMetrics,
    ) -> Self {
        let body_height = metrics.icon_size;
        let height = body_height + metrics.vertical_padding * 2.0;

        let widths = display
            .spaces
            .iter()
            .map(|space| space_width(space, &metrics))
            .collect::<Vec<_>>();
        let content_width = metrics.toolbar_width
            + metrics.horizontal_padding * 2.0
            + widths.iter().sum::<f64>()
            + count(widths.len().saturating_sub(1)) * metrics.item_gap;
        let width = content_width.min(max_width.max(1.0));
        let max_scroll = (content_width - width).max(0.0);
        let scroll_x = scroll_x.clamp(0.0, max_scroll);
        let mut x = metrics.toolbar_width + metrics.horizontal_padding - scroll_x;
        let mut items = Vec::new();

        for (space, space_width) in display.spaces.iter().zip(widths) {
            let space_rect = Rect {
                x,
                y: metrics.vertical_padding,
                width: space_width,
                height: body_height,
            };
            items.push(PlacedItem {
                rect: Rect {
                    y: 0.0,
                    height,
                    ..space_rect
                },
                kind: ItemKind::Space {
                    space_id: space.id,
                    ordinal: space.ordinal,
                    visible: space.visible,
                    focused: space.focused,
                    empty: space.is_empty(),
                    fullscreen: space.kind == SpaceKind::Fullscreen,
                },
            });
            if metrics.label_width > 0.0 {
                items.push(PlacedItem {
                    rect: Rect {
                        x: space_rect.x + 3.0,
                        width: metrics.label_width,
                        ..space_rect
                    },
                    kind: ItemKind::Label {
                        space_id: space.id,
                        ordinal: space.ordinal,
                    },
                });
            }
            if space.is_empty() {
                let content = content_rect(space_rect, &metrics);
                items.push(PlacedItem {
                    rect: Rect {
                        x: content.x + (content.width - metrics.icon_size) / 2.0,
                        y: content.y + (content.height - metrics.icon_size) / 2.0,
                        width: metrics.icon_size,
                        height: metrics.icon_size,
                    },
                    kind: ItemKind::Placeholder { space_id: space.id },
                });
            }
            if space.visible {
                place_expanded(space, space_rect, &metrics, &mut items);
            } else {
                place_collapsed(space, space_rect, &metrics, &mut items);
            }
            x += space_width + metrics.item_gap;
        }

        let focus = items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Window {
                    space_id,
                    focused: true,
                    collapsed: false,
                    ..
                } => Some(PlacedItem {
                    rect: item.rect,
                    kind: ItemKind::Focus { space_id },
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        items.extend(focus);

        Self {
            width,
            height,
            content_width,
            content_left: metrics.toolbar_width.min(width),
            split: None,
            items,
        }
    }
}

fn space_width(space: &BarSpace, metrics: &BarMetrics) -> f64 {
    if !space.visible {
        let deck = collapsed_deck(space);
        let deck = deck
            .last()
            .map_or(metrics.icon_size, |icon| icon.x + metrics.icon_size);
        return label_extent(metrics) + deck + 6.0;
    }
    let columns = space
        .columns
        .iter()
        .map(|column| column_icon_size(column, metrics) + metrics.icon_gap)
        .sum::<f64>();
    let floating = count(space.floating.len()) * (metrics.icon_size + metrics.icon_gap);
    let separator = if !space.columns.is_empty() && !space.floating.is_empty() {
        metrics.item_gap - metrics.icon_gap
    } else {
        0.0
    };
    label_extent(metrics)
        + 6.0
        + (columns + separator + floating
            - if space.is_empty() {
                0.0
            } else {
                metrics.icon_gap
            })
        .max(metrics.icon_size)
}

fn label_extent(metrics: &BarMetrics) -> f64 {
    if metrics.label_width > 0.0 {
        metrics.label_width + metrics.item_gap
    } else {
        0.0
    }
}

fn content_rect(rect: Rect, metrics: &BarMetrics) -> Rect {
    let header = label_extent(metrics);
    Rect {
        x: rect.x + 3.0 + header,
        width: rect.width - 6.0 - header,
        ..rect
    }
}

fn column_icon_size(column: &BarColumn, metrics: &BarMetrics) -> f64 {
    if column.windows.len() > 1 {
        (metrics.icon_size - 4.0).max(10.0)
    } else {
        metrics.icon_size
    }
}

fn column_icon_y(column: &BarColumn, index: usize, metrics: &BarMetrics) -> f64 {
    let spread = (metrics.icon_size - column_icon_size(column, metrics)).max(0.0);
    count(index) * spread / count(column.windows.len().saturating_sub(1)).max(1.0)
}

struct CollapsedIcon {
    kind: ItemKind,
    x: f64,
}

fn collapsed_deck(space: &BarSpace) -> Vec<CollapsedIcon> {
    // Column geometry belongs to expanded Spaces. Collapsed icons share one
    // full-size horizontal lane, regardless of their original layout role.
    space
        .windows()
        .map(|window| collapsed_window_kind(space, window))
        .take(4)
        .enumerate()
        .map(|(index, kind)| CollapsedIcon {
            kind,
            x: count(index) * 5.0,
        })
        .collect()
}

fn place_expanded(space: &BarSpace, rect: Rect, metrics: &BarMetrics, items: &mut Vec<PlacedItem>) {
    let mut x = content_rect(rect, metrics).x;
    for column in &space.columns {
        let Some(anchor) = column.anchor_window_id() else {
            continue;
        };
        let size = column_icon_size(column, metrics);
        let windows = column.windows.iter().enumerate().rev();
        // Preserve logical top-to-bottom positions; only promote focus in paint order.
        for (index, window) in windows
            .clone()
            .filter(|(_, window)| !window.focused)
            .chain(windows.filter(|(_, window)| window.focused))
        {
            items.push(PlacedItem {
                rect: Rect {
                    x,
                    y: rect.y + column_icon_y(column, index, metrics),
                    width: size,
                    height: size,
                },
                kind: ItemKind::Window {
                    window_id: window.id,
                    space_id: space.id,
                    column_window_id: Some(anchor),
                    floating: false,
                    focused: window.focused,
                    bundle_id: window.bundle_id.clone(),
                    title: window.title.clone(),
                    collapsed: false,
                    stacked: column.windows.len() > 1,
                    fullscreen: space.kind == SpaceKind::Fullscreen,
                },
            });
        }
        let half_gap = metrics.icon_gap / 2.0;
        items.push(PlacedItem {
            rect: Rect {
                x: x - half_gap,
                y: rect.y,
                width: half_gap + size / 2.0,
                height: rect.height,
            },
            kind: ItemKind::ColumnDrop {
                space_id: space.id,
                anchor_window_id: anchor,
                placement: Placement::Before,
            },
        });
        items.push(PlacedItem {
            rect: Rect {
                x: x + size / 2.0,
                y: rect.y,
                width: size / 2.0 + half_gap,
                height: rect.height,
            },
            kind: ItemKind::ColumnDrop {
                space_id: space.id,
                anchor_window_id: anchor,
                placement: Placement::After,
            },
        });
        x += size + metrics.icon_gap;
    }

    if !space.columns.is_empty() && !space.floating.is_empty() {
        x += metrics.item_gap - metrics.icon_gap;
    }
    for window in &space.floating {
        items.push(PlacedItem {
            rect: Rect {
                x,
                y: rect.y + (rect.height - metrics.icon_size) / 2.0,
                width: metrics.icon_size,
                height: metrics.icon_size,
            },
            kind: ItemKind::Window {
                window_id: window.id,
                space_id: space.id,
                column_window_id: None,
                floating: true,
                focused: window.focused,
                bundle_id: window.bundle_id.clone(),
                title: window.title.clone(),
                collapsed: false,
                stacked: false,
                fullscreen: space.kind == SpaceKind::Fullscreen,
            },
        });
        x += metrics.icon_size + metrics.icon_gap;
    }
}

fn collapsed_window_kind(space: &BarSpace, window: &BarWindow) -> ItemKind {
    ItemKind::Window {
        window_id: window.id,
        space_id: space.id,
        column_window_id: None,
        floating: false,
        focused: false,
        bundle_id: window.bundle_id.clone(),
        title: window.title.clone(),
        collapsed: true,
        stacked: false,
        fullscreen: space.kind == SpaceKind::Fullscreen,
    }
}

fn place_collapsed(
    space: &BarSpace,
    rect: Rect,
    metrics: &BarMetrics,
    items: &mut Vec<PlacedItem>,
) {
    let deck = collapsed_deck(space);
    let content = content_rect(rect, metrics);
    let width = deck.last().map_or(0.0, |icon| icon.x + metrics.icon_size);
    // Paint back to front: the first logical window remains the front card.
    for icon in deck.iter().rev() {
        items.push(PlacedItem {
            rect: Rect {
                x: content.x + (content.width - width) / 2.0 + icon.x,
                y: rect.y,
                width: metrics.icon_size,
                height: metrics.icon_size,
            },
            kind: icon.kind.clone(),
        });
    }
}

fn count(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;

    use spool_shared_types::windowset::ColumnKind;

    use super::*;
    use crate::bar::model::{BarColumn, BarWindow};

    fn window(id: i32, focused: bool) -> BarWindow {
        BarWindow {
            id,
            bundle_id: format!("com.example.{id}"),
            app_name: format!("App {id}"),
            title: format!("Window {id}"),
            focused,
            visible: true,
        }
    }

    pub(in crate::bar) fn display() -> BarDisplay {
        BarDisplay {
            id: 1,
            frame: spool_shared_types::state::Frame {
                x: 0,
                y: 0,
                width: 1200,
                height: 800,
            },
            active: true,
            spaces: vec![
                BarSpace {
                    id: 10,
                    ordinal: 0,
                    kind: SpaceKind::User,
                    visible: false,
                    focused: false,
                    columns: Vec::new(),
                    floating: Vec::new(),
                },
                BarSpace {
                    id: 11,
                    ordinal: 1,
                    kind: SpaceKind::User,
                    visible: true,
                    focused: true,
                    columns: vec![
                        BarColumn {
                            kind: ColumnKind::Stack,
                            selected: 0,
                            windows: vec![window(1, true), window(2, false)],
                        },
                        BarColumn {
                            kind: ColumnKind::Single,
                            selected: 0,
                            windows: vec![window(3, false)],
                        },
                    ],
                    floating: vec![window(4, false)],
                },
                BarSpace {
                    id: 12,
                    ordinal: 2,
                    kind: SpaceKind::Fullscreen,
                    visible: false,
                    focused: false,
                    columns: vec![BarColumn {
                        kind: ColumnKind::Fullscreen,
                        selected: 0,
                        windows: vec![window(5, false), window(6, false), window(7, false)],
                    }],
                    floating: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn expanded_space_places_stack_members_vertically_and_columns_horizontally() {
        let layout = BarLayout::resolve(&display(), 1200.0, 0.0);
        let windows = layout
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Window {
                    window_id,
                    space_id: 11,
                    ..
                } => Some((window_id, item.rect)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        assert!((windows[&1].x - windows[&2].x).abs() < f64::EPSILON);
        assert!(windows[&2].y > windows[&1].y);
        assert!(windows[&3].x > windows[&1].x);
        assert!(windows[&4].x > windows[&3].x);
    }

    #[test]
    fn collapsed_space_keeps_a_bounded_ordered_icon_deck() {
        let layout = BarLayout::resolve(&display(), 1200.0, 0.0);
        let deck = layout
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Window {
                    window_id,
                    space_id: 12,
                    ..
                } => Some((window_id, item.rect.x)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            deck.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![7, 6, 5]
        );
        assert!(deck[0].1 > deck[1].1 && deck[1].1 > deck[2].1);
        assert!(layout.items.iter().any(|item| matches!(
            item.kind,
            ItemKind::Space {
                space_id: 10,
                empty: true,
                ..
            }
        )));
        assert!(layout.items.iter().any(|item| matches!(
            item.kind,
            ItemKind::Space {
                space_id: 12,
                fullscreen: true,
                ..
            }
        )));
    }

    #[test]
    fn collapsed_stack_icons_match_single_window_size_and_baseline() {
        for icon_size in [12.0, 20.0, 32.0] {
            let metrics = BarMetrics {
                icon_size,
                ..BarMetrics::default()
            };
            let mut display = display();
            display.spaces[1].visible = false;
            let layout = BarLayout::resolve_with_metrics(&display, 1200.0, 0.0, metrics);
            let icons = layout
                .items
                .iter()
                .filter_map(|item| match item.kind {
                    ItemKind::Window { space_id: 11, .. } => Some(item.rect),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(icons.len(), 4);
            for icon in icons {
                assert!((icon.width - metrics.icon_size).abs() < f64::EPSILON);
                assert!((icon.height - metrics.icon_size).abs() < f64::EPSILON);
                assert!((icon.y - metrics.vertical_padding).abs() < f64::EPSILON);
            }
        }
    }

    #[test]
    fn native_fullscreen_badges_follow_space_membership_in_both_views() {
        for visible in [false, true] {
            let mut display = display();
            display.spaces[2].visible = visible;
            display.spaces[2].floating.push(window(8, false));
            // A fullscreen column in an ordinary Space is not native fullscreen.
            display.spaces[1].columns[0].kind = ColumnKind::Fullscreen;
            let layout = BarLayout::resolve(&display, 1200.0, 0.0);
            let mut marked_windows = 0;
            for item in &layout.items {
                match item.kind {
                    ItemKind::Window { space_id, .. } => {
                        assert_eq!(item.fullscreen_badge_rect().is_some(), space_id == 12);
                        if matches!(
                            item.kind,
                            ItemKind::Window {
                                fullscreen: true,
                                ..
                            }
                        ) {
                            marked_windows += 1;
                        }
                    }
                    _ => assert!(item.fullscreen_badge_rect().is_none()),
                }
                if let Some(badge) = item.fullscreen_badge_rect() {
                    assert!(item.rect.contains(badge.x, badge.y));
                    assert!(
                        item.rect
                            .contains(badge.x + badge.width, badge.y + badge.height)
                    );
                }
            }
            assert_eq!(marked_windows, 4);
        }
    }

    #[test]
    fn fullscreen_status_adds_no_header_or_width_with_or_without_labels() {
        for label_width in [0.0, 24.0, 96.0] {
            for icon_size in [12.0, 22.0, 52.0] {
                for (visible, empty) in [(false, false), (true, false), (false, true), (true, true)]
                {
                    let mut display = display();
                    display.spaces[2].visible = visible;
                    if empty {
                        display.spaces[2].columns.clear();
                    }
                    let layout = BarLayout::resolve_with_metrics(
                        &display,
                        1200.0,
                        0.0,
                        BarMetrics {
                            icon_size,
                            label_width,
                            ..BarMetrics::default()
                        },
                    );
                    let space = layout
                        .items
                        .iter()
                        .find(|item| matches!(item.kind, ItemKind::Space { space_id: 12, .. }))
                        .unwrap();
                    assert!(space.fullscreen_badge_rect().is_none());
                    display.spaces[2].kind = SpaceKind::User;
                    let ordinary = BarLayout::resolve_with_metrics(
                        &display,
                        1200.0,
                        0.0,
                        BarMetrics {
                            icon_size,
                            label_width,
                            ..BarMetrics::default()
                        },
                    );
                    assert!((layout.content_width - ordinary.content_width).abs() < f64::EPSILON);
                    assert_eq!(layout.items.len(), ordinary.items.len());
                    for (fullscreen, ordinary) in layout.items.iter().zip(&ordinary.items) {
                        assert_eq!(fullscreen.rect, ordinary.rect);
                        if matches!(fullscreen.kind, ItemKind::Window { space_id: 12, .. }) {
                            assert!(fullscreen.fullscreen_badge_rect().is_some());
                            assert!(ordinary.fullscreen_badge_rect().is_none());
                        }
                    }
                    assert!((layout.height - icon_size - 12.0).abs() < f64::EPSILON);
                }
            }
        }
    }

    #[test]
    fn layout_clamps_scroll_to_content_width() {
        let display = display();
        let layout = BarLayout::resolve(&display, 90.0, f64::MAX);
        assert!((layout.width - 90.0).abs() < f64::EPSILON);
        let leftmost = layout
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Space { .. }))
            .map(|item| item.rect.x)
            .fold(f64::INFINITY, f64::min);
        assert!(leftmost < 0.0);
    }

    #[test]
    fn collapsed_deck_exposes_at_least_five_pixels_per_layer_in_paint_order() {
        let layout = BarLayout::resolve(&display(), 1200.0, 0.0);
        let deck = layout
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Window {
                    window_id,
                    space_id: 12,
                    ..
                } => Some((window_id, item.rect)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            deck.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![7, 6, 5]
        );
        for pair in deck.windows(2) {
            assert!(pair[0].1.x - pair[1].1.x >= 5.0);
            assert!(pair[0].1.x < pair[1].1.x + pair[1].1.width);
        }
    }

    #[test]
    fn hiding_labels_reclaims_width_in_collapsed_spaces_too() {
        let display = display();
        let visible = BarLayout::resolve(&display, 1200.0, 0.0);
        let hidden = BarLayout::resolve_with_metrics(
            &display,
            1200.0,
            0.0,
            BarMetrics {
                label_width: 0.0,
                ..BarMetrics::default()
            },
        );
        assert!(visible.content_width - hidden.content_width >= 3.0 * 24.0);
    }

    #[test]
    fn every_deck_layer_has_visible_pixels_after_later_cards_are_painted() {
        let layout = BarLayout::resolve(&display(), 1200.0, 0.0);
        let deck = layout
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Window { space_id: 12, .. }))
            .collect::<Vec<_>>();
        for card in &deck {
            let point = (
                card.rect.x + card.rect.width - 2.0,
                card.rect.y + card.rect.height / 2.0,
            );
            let painted = deck
                .iter()
                .rev()
                .find(|item| item.rect.contains(point.0, point.1))
                .unwrap();
            assert_eq!(painted.kind, card.kind);
        }
    }

    #[test]
    fn content_and_empty_placeholders_stay_inside_their_own_regions() {
        for label_width in [0.0, 24.0, 96.0] {
            for icon_size in [12.0, 22.0, 52.0] {
                let metrics = BarMetrics {
                    icon_size,
                    label_width,
                    ..BarMetrics::default()
                };
                let display = display();
                let layout = BarLayout::resolve_with_metrics(&display, 1200.0, 0.0, metrics);
                let labels = layout
                    .items
                    .iter()
                    .filter(|item| matches!(item.kind, ItemKind::Label { .. }))
                    .count();
                assert_eq!(labels, if label_width == 0.0 { 0 } else { 3 });
                for item in &layout.items {
                    let (ItemKind::Window { space_id, .. } | ItemKind::Placeholder { space_id }) =
                        item.kind
                    else {
                        continue;
                    };
                    let space = layout.items.iter().find(|item| matches!(item.kind, ItemKind::Space { space_id: id, .. } if id == space_id)).unwrap();
                    let content = content_rect(space.rect, &metrics);
                    assert!(content.contains(item.rect.x, item.rect.y));
                    assert!(content.contains(
                        item.rect.x + item.rect.width,
                        item.rect.y + item.rect.height
                    ));
                }
                let empty = layout
                    .items
                    .iter()
                    .find(|item| matches!(item.kind, ItemKind::Placeholder { space_id: 10 }))
                    .unwrap();
                assert!((empty.rect.width - icon_size).abs() < f64::EPSILON);
                assert!(
                    !layout
                        .items
                        .iter()
                        .any(|item| matches!(item.kind, ItemKind::Window { space_id: 10, .. }))
                );
            }
        }
    }

    #[test]
    fn expanded_empty_space_centers_placeholder_when_labels_are_hidden() {
        let mut display = display();
        display.spaces[0].visible = true;
        let layout = BarLayout::resolve_with_metrics(
            &display,
            1200.0,
            0.0,
            BarMetrics {
                label_width: 0.0,
                ..BarMetrics::default()
            },
        );
        let space = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Space { space_id: 10, .. }))
            .unwrap();
        let placeholder = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Placeholder { space_id: 10 }))
            .unwrap();
        assert!(
            (space.rect.x + space.rect.width / 2.0
                - placeholder.rect.x
                - placeholder.rect.width / 2.0)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (space.rect.y + space.rect.height / 2.0
                - placeholder.rect.y
                - placeholder.rect.height / 2.0)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn hidden_label_centers_the_combined_tiled_and_floating_lane() {
        let layout = BarLayout::resolve_with_metrics(
            &display(),
            1200.0,
            0.0,
            BarMetrics {
                label_width: 0.0,
                ..BarMetrics::default()
            },
        );
        let space = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Space { space_id: 11, .. }))
            .unwrap();
        let windows = layout
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Window { space_id: 11, .. }));
        let left = windows
            .clone()
            .map(|item| item.rect.x)
            .fold(f64::INFINITY, f64::min);
        let right = windows
            .map(|item| item.rect.x + item.rect.width)
            .fold(0.0, f64::max);
        assert!(
            (left.midpoint(right) - space.rect.x - space.rect.width / 2.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn stack_columns_never_increase_bar_height() {
        for members in [2, 3, 8, 20] {
            let mut display = display();
            display.spaces[1].columns[0].windows =
                (1..=members).map(|id| window(id, false)).collect();
            let layout = BarLayout::resolve(&display, 1200.0, 0.0);
            assert!((layout.height - 34.0).abs() < f64::EPSILON);
            let windows = layout
                .items
                .iter()
                .filter(|item| {
                    matches!(
                        item.kind,
                        ItemKind::Window {
                            space_id: 11,
                            column_window_id: Some(1),
                            ..
                        }
                    )
                })
                .collect::<Vec<_>>();
            for pair in windows.windows(2) {
                assert!(pair[0].rect.y > pair[1].rect.y);
                assert!(pair[0].rect.y < pair[1].rect.y + pair[1].rect.height);
            }
        }
    }

    #[test]
    fn collapsing_preserves_single_and_floating_icon_geometry() {
        for icon_size in [18.0, 22.0, 36.0] {
            for label_width in [0.0, 24.0] {
                let mut display = display();
                let metrics = BarMetrics {
                    icon_size,
                    label_width,
                    ..BarMetrics::default()
                };
                let expanded = BarLayout::resolve_with_metrics(&display, 1200.0, 0.0, metrics);
                display.spaces[1].visible = false;
                let collapsed = BarLayout::resolve_with_metrics(&display, 1200.0, 0.0, metrics);
                for item in &collapsed.items {
                    let ItemKind::Window {
                        window_id,
                        space_id: 11,
                        ..
                    } = item.kind
                    else {
                        continue;
                    };
                    let original = expanded.items.iter().find(|item| matches!(item.kind, ItemKind::Window { window_id: id, space_id: 11, .. } if id == window_id)).unwrap();
                    if matches!(original.kind, ItemKind::Window { stacked: true, .. }) {
                        continue;
                    }
                    assert!(
                        (item.rect.y - original.rect.y).abs() < 0.001,
                        "window {window_id} moved vertically"
                    );
                    assert!((item.rect.height - original.rect.height).abs() < 0.001);
                    assert!((item.rect.width - original.rect.width).abs() < 0.001);
                }
            }
        }
    }

    #[test]
    fn stack_paints_lower_members_behind_and_focused_member_last() {
        for focused in [None, Some(1), Some(2), Some(3)] {
            let mut display = display();
            display.spaces[1].columns[0].windows =
                (1..=3).map(|id| window(id, focused == Some(id))).collect();
            let layout = BarLayout::resolve(&display, 1200.0, 0.0);
            let ids = layout
                .items
                .iter()
                .filter_map(|item| match item.kind {
                    ItemKind::Window {
                        window_id,
                        space_id: 11,
                        column_window_id: Some(1),
                        ..
                    } => Some(window_id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let mut expected = vec![3, 2, 1];
            if let Some(id) = focused {
                expected.retain(|candidate| *candidate != id);
                expected.push(id);
            }
            assert_eq!(ids, expected);
        }
    }
}

use spool_shared_types::commands::{Action, MoveFocus, Placement};
use spool_shared_types::state::SpaceKind;

use super::layout::{BarLayout, ItemKind, PlacedItem, Rect};
use super::model::BarDisplay;
use super::motion::{Presentation, VisualItem};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropTarget {
    Column { anchor: i32, placement: Placement },
    Floating { anchor: i32, before: bool },
    Space(u64),
}

#[derive(Clone, Debug)]
pub struct BarDrag {
    pub window_id: i32,
    pub space_id: u64,
    pub column_window_id: Option<i32>,
    pub floating: bool,
    pub active: bool,
    pub target: Option<DropTarget>,
    start: (f64, f64),
    pointer: (f64, f64),
    activation_rect: Option<Rect>,
    payload: Vec<VisualItem>,
}

impl BarDrag {
    pub fn begin(item: &PlacedItem, frame: &Presentation, point: (f64, f64)) -> Option<Self> {
        let ItemKind::Window {
            window_id,
            space_id,
            column_window_id,
            floating,
            collapsed: false,
            ..
        } = item.kind
        else {
            return None;
        };
        let payload = frame.items.iter().filter(|visual| matches!(visual.item.kind,
            ItemKind::Window { window_id: id, space_id: space, column_window_id: column, .. }
            if space == space_id && if floating { id == window_id } else { column == column_window_id }
        )).cloned().collect();
        Some(Self {
            window_id,
            space_id,
            column_window_id,
            floating,
            active: false,
            target: None,
            start: point,
            pointer: point,
            activation_rect: None,
            payload,
        })
    }

    pub fn move_pointer(&mut self, point: (f64, f64)) {
        self.pointer = point;
        let dx = point.0 - self.start.0;
        let dy = point.1 - self.start.1;
        self.active |= dx.mul_add(dx, dy * dy) >= 16.0;
    }

    pub fn ghost_items(&self) -> Vec<VisualItem> {
        self.payload
            .iter()
            .map(|visual| {
                let mut visual = visual.clone();
                visual.item.rect.x += self.pointer.0 - self.start.0;
                visual.item.rect.y += self.pointer.1 - self.start.1;
                visual.opacity = 0.92;
                visual.deck = 1.0;
                visual
            })
            .collect()
    }

    // Screen coordinates are AppKit Y-up; icon coordinates are view-local Y-down.
    pub fn ghost_geometry(&self, pointer: (f64, f64), screen: Rect) -> (Rect, Vec<VisualItem>) {
        let mut items = self.ghost_items();
        let bounds = union(items.iter().map(|item| item.item.rect)).unwrap_or_default();
        let width = bounds.width + 8.0;
        let height = bounds.height + 8.0;
        let offset = (
            self.pointer.0 - bounds.x + 4.0,
            self.pointer.1 - bounds.y + 4.0,
        );
        for visual in &mut items {
            visual.item.rect.x += 4.0 - bounds.x;
            visual.item.rect.y += 4.0 - bounds.y;
        }
        (
            Rect {
                x: (pointer.0 - offset.0)
                    .clamp(screen.x, (screen.x + screen.width - width).max(screen.x)),
                y: (pointer.1 + offset.1 - height)
                    .clamp(screen.y, (screen.y + screen.height - height).max(screen.y)),
                width,
                height,
            },
            items,
        )
    }

    /// Whether the gesture still describes a window that is where it was.
    ///
    /// The Space a drag starts in does not have to be the one macOS is showing:
    /// with `bar.collapse_inactive_spaces` off every Space draws its windows, so
    /// a drag can start in any of them. Whether a Space's icons are live is
    /// decided where the gesture begins (`BarView::press_at`), not here.
    pub fn is_valid(&self, display: &BarDisplay) -> bool {
        let Some(space) = display
            .spaces
            .iter()
            .find(|space| space.id == self.space_id)
        else {
            return false;
        };
        if self.floating {
            return space
                .floating
                .iter()
                .any(|window| window.id == self.window_id);
        }
        space.columns.iter().any(|column| {
            column.anchor_window_id() == self.column_window_id
                && column.windows.len() == self.payload.len()
                && column
                    .windows
                    .iter()
                    .all(|window| self.contains_window(window.id))
        })
    }

    pub fn contains_window(&self, id: i32) -> bool {
        self.payload.iter().any(
            |item| matches!(item.item.kind, ItemKind::Window { window_id, .. } if window_id == id),
        )
    }

    pub fn hides(&self, item: &VisualItem) -> bool {
        self.active
            && match item.item.kind {
                ItemKind::Window { window_id, .. } => self.contains_window(window_id),
                ItemKind::Focus { space_id }
                    if space_id == self.space_id || space_id == self.target_space() =>
                {
                    self.payload.iter().any(|item| {
                        matches!(item.item.kind, ItemKind::Window { focused: true, .. })
                    })
                }
                _ => false,
            }
    }

    pub fn target_space(&self) -> u64 {
        match self.target {
            Some(DropTarget::Space(id)) => id,
            _ => self.space_id,
        }
    }

    pub fn gap_rect(&self, frame: &Presentation) -> Option<Rect> {
        self.target?;
        union(frame.items.iter().filter(|item| item.space_id() == self.target_space()
            && matches!(item.item.kind, ItemKind::Window { window_id, .. } if self.contains_window(window_id)))
            .map(|item| item.item.rect))
    }

    pub fn update_target(
        &mut self,
        display: &BarDisplay,
        layout: &BarLayout,
        frame: &Presentation,
        can_move: bool,
    ) {
        if !self.active
            || !can_move
            || !frame.over_space_strip(self.pointer)
            || !self.is_valid(display)
        {
            self.target = None;
            self.activation_rect = None;
            return;
        }
        // Keep the opened slot addressable while its neighboring icons animate.
        // Using only moving anchor halves would oscillate between before/after.
        if self.target_valid(display)
            && self
                .gap_rect(frame)
                .into_iter()
                .chain(self.activation_rect)
                .any(|rect| {
                    rect.intersection(frame.viewport(self.target_space()))
                        .contains(self.pointer.0, self.pointer.1)
                })
        {
            return;
        }
        let target = self.resolve_target(display, layout);
        self.target = target.map(|(target, _)| target);
        self.activation_rect = target.map(|(_, rect)| rect);
    }

    fn resolve_target(
        &self,
        display: &BarDisplay,
        layout: &BarLayout,
    ) -> Option<(DropTarget, Rect)> {
        let (space_id, space_rect) = layout.items.iter().find_map(|item| match item.kind {
            ItemKind::Space {
                space_id,
                fullscreen: false,
                ..
            } if item.rect.contains(self.pointer.0, self.pointer.1) => Some((space_id, item.rect)),
            _ => None,
        })?;
        if display
            .spaces
            .iter()
            .find(|space| space.id == space_id)?
            .kind
            != SpaceKind::User
        {
            return None;
        }
        if space_id != self.space_id {
            return Some((DropTarget::Space(space_id), space_rect));
        }
        layout.items.iter().rev().find_map(|item| {
            if !item.rect.contains(self.pointer.0, self.pointer.1) {
                return None;
            }
            match item.kind {
                ItemKind::ColumnDrop {
                    space_id,
                    anchor_window_id,
                    placement,
                } if !self.floating
                    && space_id == self.space_id
                    && Some(anchor_window_id) != self.column_window_id =>
                {
                    Some((
                        DropTarget::Column {
                            anchor: anchor_window_id,
                            placement,
                        },
                        item.rect,
                    ))
                }
                ItemKind::Window {
                    window_id,
                    space_id,
                    floating: true,
                    ..
                } if self.floating && space_id == self.space_id && window_id != self.window_id => {
                    let before = self.pointer.0 < item.rect.x + item.rect.width / 2.0;
                    Some((
                        DropTarget::Floating {
                            anchor: window_id,
                            before,
                        },
                        Rect {
                            x: item.rect.x + if before { 0.0 } else { item.rect.width / 2.0 },
                            width: item.rect.width / 2.0,
                            ..item.rect
                        },
                    ))
                }
                _ => None,
            }
        })
    }

    pub fn target_valid(&self, display: &BarDisplay) -> bool {
        let Some(space) = display
            .spaces
            .iter()
            .find(|space| space.id == self.target_space() && space.kind == SpaceKind::User)
        else {
            return false;
        };
        match self.target {
            Some(DropTarget::Space(id)) => id != self.space_id,
            Some(DropTarget::Column { anchor, .. }) => {
                !self.floating
                    && !self.contains_window(anchor)
                    && space
                        .columns
                        .iter()
                        .any(|column| column.anchor_window_id() == Some(anchor))
            }
            Some(DropTarget::Floating { anchor, .. }) => {
                self.floating
                    && anchor != self.window_id
                    && space.floating.iter().any(|window| window.id == anchor)
            }
            None => false,
        }
    }

    /// The action a release means: a click focuses, a drag moves.
    ///
    /// `source_visible` says whether the Space the drag started in is the one
    /// macOS is showing. Focusing a window anywhere else means going to it
    /// first, which cannot be expressed by [`Action::FocusWindow`] alone.
    pub fn action(&self, source_visible: bool) -> Option<Action> {
        if !self.active {
            return Some(if source_visible {
                Action::FocusWindow {
                    window_id: self.window_id,
                }
            } else {
                Action::FocusWindowInSpace {
                    window_id: self.window_id,
                    space_id: self.space_id,
                }
            });
        }
        match self.target? {
            DropTarget::Column { anchor, placement } => Some(Action::ReorderColumn {
                window_id: self.window_id,
                anchor_window_id: anchor,
                placement,
            }),
            DropTarget::Space(space_id) if self.floating => Some(Action::MoveWindowToSpace {
                window_id: self.window_id,
                space_id,
                move_focus: MoveFocus::Follow,
            }),
            DropTarget::Space(space_id) => Some(Action::MoveColumnToSpace {
                window_id: self.window_id,
                space_id,
                move_focus: MoveFocus::Follow,
            }),
            DropTarget::Floating { .. } => None,
        }
    }

    pub fn preview_display(&self, display: &BarDisplay) -> BarDisplay {
        let mut preview = display.clone();
        if !self.active || !self.is_valid(display) || !self.target_valid(display) {
            return preview;
        }
        let source = preview
            .spaces
            .iter()
            .position(|space| space.id == self.space_id)
            .expect("validated source");
        let destination = preview
            .spaces
            .iter()
            .position(|space| space.id == self.target_space())
            .expect("validated target");
        if self.floating {
            let windows = &mut preview.spaces[source].floating;
            let index = windows
                .iter()
                .position(|window| window.id == self.window_id)
                .expect("validated floating window");
            let window = windows.remove(index);
            let windows = &mut preview.spaces[destination].floating;
            let index = match self.target {
                Some(DropTarget::Floating { anchor, before }) => {
                    windows
                        .iter()
                        .position(|window| window.id == anchor)
                        .expect("validated floating anchor")
                        + usize::from(!before)
                }
                _ => windows.len(),
            };
            windows.insert(index, window);
        } else {
            let columns = &mut preview.spaces[source].columns;
            let index = columns
                .iter()
                .position(|column| column.anchor_window_id() == self.column_window_id)
                .expect("validated column");
            let mut column = columns.remove(index);
            if source != destination
                && column.kind == spool_shared_types::windowset::ColumnKind::Fullscreen
            {
                column.kind = spool_shared_types::windowset::ColumnKind::Single;
            }
            let columns = &mut preview.spaces[destination].columns;
            let index = match self.target {
                Some(DropTarget::Column { anchor, placement }) => {
                    columns
                        .iter()
                        .position(|column| column.anchor_window_id() == Some(anchor))
                        .expect("validated column anchor")
                        + usize::from(placement == Placement::After)
                }
                _ => columns.len(),
            };
            columns.insert(index, column);
        }
        // Expansion is presentation-only. Native Space focus changes on drop, never hover.
        preview.spaces[destination].visible = true;
        preview
    }
}

fn union(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.reduce(|a, b| {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Rect {
            x,
            y,
            width: (a.x + a.width).max(b.x + b.width) - x,
            height: (a.y + a.height).max(b.y + b.height) - y,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::motion::BarMotion;
    use std::time::Instant;

    fn setup() -> (BarDisplay, BarLayout, BarDrag) {
        let display = crate::bar::layout::tests::display();
        let layout = BarLayout::resolve(&display, 1200.0);
        let frame = BarMotion::new(&layout, Instant::now()).presented;
        let item = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap();
        let drag = BarDrag::begin(item, &frame, (item.rect.x + 3.0, item.rect.y + 4.0)).unwrap();
        (display, layout, drag)
    }

    #[test]
    fn entire_column_ghost_follows_pointer_without_changing_member_offsets() {
        let (_, _, mut drag) = setup();
        let before = drag.ghost_items();
        drag.move_pointer((drag.start.0 + 50.0, drag.start.1 + 17.0));
        assert!(drag.active);
        let after = drag.ghost_items();
        assert_eq!(after.len(), 2);
        for (a, b) in before.iter().zip(&after) {
            assert!((b.item.rect.x - a.item.rect.x - 50.0).abs() < 0.001);
            assert!((b.item.rect.y - a.item.rect.y - 17.0).abs() < 0.001);
        }
    }

    #[test]
    fn insertion_preview_pushes_neighbor_and_preserves_column_structure() {
        let (display, initial, mut drag) = setup();
        drag.active = true;
        drag.target = Some(DropTarget::Column {
            anchor: 3,
            placement: Placement::After,
        });
        let preview = drag.preview_display(&display);
        assert_eq!(preview.spaces[1].columns[0].windows[0].id, 3);
        assert_eq!(preview.spaces[1].columns[1], display.spaces[1].columns[0]);
        let next = BarLayout::resolve(&preview, 1200.0);
        let x = |layout: &BarLayout| {
            layout
                .items
                .iter()
                .find(|item| matches!(item.kind, ItemKind::Window { window_id: 3, .. }))
                .unwrap()
                .rect
                .x
        };
        assert!(x(&next) < x(&initial));
        assert!((next.height - initial.height).abs() < f64::EPSILON);
        assert_eq!(display.spaces[1].columns[0].windows[0].id, 1);
    }

    #[test]
    fn toolbar_rejects_a_drop_pointer_over_the_fixed_buttons() {
        let (display, _, mut drag) = setup();
        drag.active = true;
        drag.target = Some(DropTarget::Column {
            anchor: 3,
            placement: Placement::After,
        });
        let layout = BarLayout::resolve(&display, 1200.0);
        let frame = BarMotion::new(&layout, Instant::now()).presented;
        // The grip and the two buttons are not part of the Space strip.
        for x in [2.0, frame.content_left / 2.0, frame.content_left - 1.0] {
            drag.move_pointer((x, 17.0));
            drag.update_target(&display, &frame.interaction_layout(&layout), &frame, true);
            assert!(
                drag.target.is_none(),
                "pointer at {x} must not keep a target"
            );
            assert!(drag.action(true).is_none());
        }
    }

    #[test]
    fn insertion_target_stays_stable_through_motion_and_matches_drop_action() {
        let (display, initial, mut drag) = setup();
        let now = Instant::now();
        let mut motion = BarMotion::new(&initial, now);
        let target = initial
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    ItemKind::ColumnDrop {
                        anchor_window_id: 3,
                        placement: Placement::After,
                        ..
                    }
                )
            })
            .unwrap();
        drag.move_pointer((target.rect.x + target.rect.width / 2.0, target.rect.y + 8.0));
        drag.update_target(&display, &initial, &motion.presented, true);
        let expected = Some(DropTarget::Column {
            anchor: 3,
            placement: Placement::After,
        });
        assert_eq!(drag.target, expected);
        let layout = BarLayout::resolve(&drag.preview_display(&display), 1200.0);
        motion.retarget(&layout, now);
        for millis in [0, 30, 70, 120, 240, 400] {
            motion.advance(now + std::time::Duration::from_millis(millis));
            let hits = motion.presented.interaction_layout(&layout);
            drag.update_target(&display, &hits, &motion.presented, true);
            assert_eq!(drag.target, expected);
        }
        assert!(matches!(
            drag.action(true),
            Some(Action::ReorderColumn {
                window_id: 1,
                anchor_window_id: 3,
                placement: Placement::After
            })
        ));
        let gap = drag.gap_rect(&motion.presented).unwrap();
        assert!(gap.width > 0.0);
        assert!(
            motion
                .presented
                .items
                .iter()
                .filter(|item| drag.hides(item))
                .any(|item| matches!(item.item.kind, ItemKind::Focus { .. }))
        );
    }

    #[test]
    fn cross_space_preview_expands_target_without_mutating_native_state() {
        let (display, layout, mut drag) = setup();
        let frame = BarMotion::new(&layout, Instant::now()).presented;
        let target = frame.space_rect(10).unwrap();
        drag.move_pointer((target.x + target.width / 2.0, target.y + 8.0));
        drag.update_target(&display, &layout, &frame, true);
        assert_eq!(drag.target, Some(DropTarget::Space(10)));
        let preview = drag.preview_display(&display);
        assert!(preview.spaces[0].visible);
        assert!(!preview.spaces[0].focused);
        assert!(preview.spaces[1].visible);
        assert_eq!(preview.spaces[0].columns[0], display.spaces[1].columns[0]);
        assert_eq!(preview.spaces[1].columns.len(), 1);
        assert!(!display.spaces[0].visible);
        assert!(display.spaces[0].columns.is_empty());
        assert!(matches!(
            drag.action(true),
            Some(Action::MoveColumnToSpace {
                window_id: 1,
                space_id: 10,
                ..
            })
        ));
    }

    #[test]
    fn rejected_targets_and_leaving_bar_restore_layout_without_an_action() {
        let (display, layout, mut drag) = setup();
        let frame = BarMotion::new(&layout, Instant::now()).presented;
        for (space, allowed) in [(10, false), (12, true)] {
            let rect = frame.space_rect(space).unwrap();
            drag.move_pointer((rect.x + rect.width / 2.0, rect.y + 8.0));
            drag.update_target(&display, &layout, &frame, allowed);
            assert_eq!(drag.target, None);
            assert_eq!(drag.preview_display(&display), display);
            assert!(drag.action(true).is_none());
        }
        drag.target = Some(DropTarget::Space(10));
        drag.move_pointer((50.0, 200.0));
        drag.update_target(&display, &layout, &frame, true);
        assert_eq!(drag.target, None);
        assert_eq!(drag.preview_display(&display), display);
        assert!(drag.action(true).is_none());
    }

    #[test]
    fn a_click_asks_to_go_to_the_window_when_its_space_is_not_up() {
        let (_, _, mut drag) = setup();
        drag.active = false;
        assert_eq!(
            drag.action(true),
            Some(Action::FocusWindow {
                window_id: drag.window_id
            }),
            "on the visible Space a click is just a focus"
        );
        assert_eq!(
            drag.action(false),
            Some(Action::FocusWindowInSpace {
                window_id: drag.window_id,
                space_id: drag.space_id,
            }),
            "anywhere else it has to say which Space to go to"
        );
    }

    #[test]
    fn removed_window_changed_column_or_space_switch_invalidates_gesture() {
        let (display, _, mut drag) = setup();
        drag.active = true;
        drag.target = Some(DropTarget::Space(10));
        let mut closed = display.clone();
        closed.spaces[1].columns[0].windows.pop();
        assert!(!drag.is_valid(&closed));
        assert_eq!(drag.preview_display(&closed), closed);
        // The Space the gesture started in stops being the visible one: that is
        // no longer fatal — only the window leaving the Space is.
        let mut switched = display.clone();
        switched.spaces[1].visible = false;
        assert!(drag.is_valid(&switched));
        let mut removed_target = display.clone();
        removed_target.spaces.remove(0);
        assert!(!drag.target_valid(&removed_target));
        assert_eq!(drag.preview_display(&removed_target), removed_target);
    }

    #[test]
    fn floating_preview_moves_one_icon_and_leaves_columns_untouched() {
        let (mut display, _, _) = setup();
        let mut second = display.spaces[1].floating[0].clone();
        second.id = 8;
        display.spaces[1].floating.push(second);
        let layout = BarLayout::resolve(&display, 1200.0);
        let frame = BarMotion::new(&layout, Instant::now()).presented;
        let item = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 4, .. }))
            .unwrap();
        let mut drag =
            BarDrag::begin(item, &frame, (item.rect.x + 3.0, item.rect.y + 3.0)).unwrap();
        let target = layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 8, .. }))
            .unwrap();
        drag.move_pointer((target.rect.x + target.rect.width - 2.0, target.rect.y + 3.0));
        drag.update_target(&display, &layout, &frame, true);
        assert_eq!(drag.ghost_items().len(), 1);
        assert_eq!(
            drag.target,
            Some(DropTarget::Floating {
                anchor: 8,
                before: false
            })
        );
        let preview = drag.preview_display(&display);
        assert_eq!(
            preview.spaces[1]
                .floating
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![8, 4]
        );
        assert_eq!(preview.spaces[1].columns, display.spaces[1].columns);
        drag.target = Some(DropTarget::Space(10));
        let preview = drag.preview_display(&display);
        assert_eq!(preview.spaces[0].floating[0].id, 4);
        assert!(preview.spaces[0].columns.is_empty());
        assert!(matches!(
            drag.action(true),
            Some(Action::MoveWindowToSpace {
                window_id: 4,
                space_id: 10,
                ..
            })
        ));
    }

    #[test]
    fn ghost_preserves_grab_offset_outside_bar_and_stays_on_owning_display() {
        let (_, _, mut drag) = setup();
        let screen = Rect {
            x: -1200.0,
            y: 100.0,
            width: 1200.0,
            height: 800.0,
        };
        drag.move_pointer((drag.start.0 + 50.0, drag.start.1 + 150.0));
        let (first, icons) = drag.ghost_geometry((-600.0, 500.0), screen);
        let (second, moved_icons) = drag.ghost_geometry((-580.0, 470.0), screen);
        assert!((second.x - first.x - 20.0).abs() < 0.001);
        assert!((second.y - first.y + 30.0).abs() < 0.001);
        assert_eq!(icons, moved_icons);
        let grabbed = icons
            .iter()
            .find(|item| matches!(item.item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap()
            .item
            .rect;
        assert!((first.x + grabbed.x + 3.0 + 600.0).abs() < 0.001);
        assert!((first.y + first.height - grabbed.y - 4.0 - 500.0).abs() < 0.001);
        for pointer in [(-1300.0, 50.0), (100.0, 1000.0)] {
            let (rect, _) = drag.ghost_geometry(pointer, screen);
            assert_eq!(rect.intersection(screen), rect);
        }
    }

    #[test]
    fn click_threshold_and_fullscreen_focus_are_preserved() {
        let (mut display, _, mut drag) = setup();
        drag.move_pointer((drag.start.0 + 2.0, drag.start.1 + 1.0));
        assert!(!drag.active);
        assert_eq!(drag.preview_display(&display), display);
        assert!(matches!(
            drag.action(true),
            Some(Action::FocusWindow { window_id: 1 })
        ));
        display.spaces[1].kind = SpaceKind::Fullscreen;
        assert!(drag.is_valid(&display));
        assert!(matches!(
            drag.action(true),
            Some(Action::FocusWindow { window_id: 1 })
        ));
    }
}

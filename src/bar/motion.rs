use std::time::Instant;

use super::layout::{BarLayout, ItemKind, NotchSplit, PlacedItem, Rect};

// Shared by Space resizing, deck movement and selection. No overshoot at clip edges.
const DURATION: f64 = 0.24;

#[derive(Clone, Debug, PartialEq)]
pub struct VisualItem {
    pub item: PlacedItem,
    pub opacity: f64,
    pub emphasis: f64,
    pub deck: f64,
}

impl VisualItem {
    pub fn icon_decoration_opacity(&self) -> f64 {
        if matches!(
            self.item.kind,
            ItemKind::Window {
                collapsed: true,
                ..
            } | ItemKind::Surface { .. }
        ) {
            0.0
        } else {
            self.deck * self.opacity
        }
    }

    pub fn space_id(&self) -> u64 {
        match self.item.kind {
            ItemKind::Space { space_id, .. }
            | ItemKind::Window { space_id, .. }
            | ItemKind::Surface { space_id, .. }
            | ItemKind::Label { space_id, .. }
            | ItemKind::Placeholder { space_id }
            | ItemKind::Focus { space_id }
            | ItemKind::ColumnDrop { space_id, .. } => space_id,
        }
    }

    fn key(&self) -> (u64, u8, i32, i32) {
        let (kind, id, owner) = match self.item.kind {
            ItemKind::Space { .. } => (0, 0, 0),
            ItemKind::Window { window_id, .. } => (1, window_id, 0),
            ItemKind::Label { .. } => (2, 0, 0),
            ItemKind::Placeholder { .. } => (3, 0, 0),
            ItemKind::Focus { .. } => (4, 0, 0),
            ItemKind::Surface {
                window_id,
                owner_pid,
                ..
            } => (5, window_id, owner_pid),
            ItemKind::ColumnDrop { .. } => unreachable!("drop targets are not painted"),
        };
        (self.space_id(), kind, id, owner)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    pub width: f64,
    pub height: f64,
    pub content_left: f64,
    pub split: Option<NotchSplit>,
    pub items: Vec<VisualItem>,
}

impl Presentation {
    fn from_layout(layout: &BarLayout) -> Self {
        Self {
            width: layout.width,
            height: layout.height,
            content_left: layout.content_left,
            split: layout.split.clone(),
            items: layout
                .items
                .iter()
                .filter(|item| !matches!(item.kind, ItemKind::ColumnDrop { .. }))
                .map(|item| VisualItem {
                    emphasis: match item.kind {
                        ItemKind::Space { focused: true, .. } => 1.0,
                        ItemKind::Space { visible: true, .. } => 0.75,
                        _ => 0.0,
                    },
                    deck: f64::from(matches!(
                        item.kind,
                        ItemKind::Window {
                            collapsed: true,
                            ..
                        } | ItemKind::Window { stacked: true, .. }
                            | ItemKind::Surface {
                                collapsed: true,
                                ..
                            }
                    )),
                    item: item.clone(),
                    opacity: 1.0,
                })
                .collect(),
        }
    }

    pub fn space_rect(&self, space_id: u64) -> Option<Rect> {
        self.items.iter().find_map(|visual| match visual.item.kind {
            ItemKind::Space { space_id: id, .. } if id == space_id => Some(visual.item.rect),
            _ => None,
        })
    }

    pub fn viewport(&self, space_id: u64) -> Rect {
        let lane = self.split.as_ref().map_or(
            Rect {
                x: self.content_left,
                width: (self.width - self.content_left).max(0.0),
                height: self.height,
                ..Rect::default()
            },
            |split| split.viewport(space_id, self.width, self.height, self.content_left),
        );
        self.space_rect(space_id)
            .unwrap_or_default()
            .intersection(lane)
    }

    pub fn scroll_lane(&self, point: (f64, f64)) -> Option<usize> {
        if point.0 < self.content_left
            || point.0 > self.width
            || point.1 < 0.0
            || point.1 > self.height
        {
            return None;
        }
        match &self.split {
            Some(split) if point.0 >= split.gap.x && point.0 <= split.gap.x + split.gap.width => {
                None
            }
            Some(split) if point.0 > split.gap.x => Some(1),
            _ => Some(0),
        }
    }

    // Use current command semantics with presented geometry. Fading-out windows
    // that no longer belong to the target layout can never receive an action.
    pub fn interaction_layout(&self, target: &BarLayout) -> BarLayout {
        let mut layout = target.clone();
        layout.width = self.width;
        layout.height = self.height;
        layout.content_left = self.content_left;
        layout.split.clone_from(&self.split);
        for item in &mut layout.items {
            if let ItemKind::ColumnDrop {
                space_id,
                anchor_window_id,
                ..
            } = item.kind
            {
                let original = target.items.iter().find(|item| matches!(item.kind, ItemKind::Window { window_id, space_id: id, .. } if window_id == anchor_window_id && id == space_id));
                let presented = self.items.iter().find(|visual| visual.opacity > 0.1 && matches!(visual.item.kind, ItemKind::Window { window_id, space_id: id, .. } if window_id == anchor_window_id && id == space_id));
                if let Some((original, anchor)) = original.zip(presented) {
                    let rect = anchor.item.rect;
                    let space = self.viewport(space_id);
                    let scale = rect.width / original.rect.width.max(1.0);
                    item.rect = Rect {
                        x: rect.x + (item.rect.x - original.rect.x) * scale,
                        y: space.y,
                        width: item.rect.width * scale,
                        height: space.height,
                    }
                    .intersection(space);
                } else {
                    item.rect = Rect::default();
                }
                continue;
            }
            let key = VisualItem {
                item: item.clone(),
                opacity: 1.0,
                emphasis: 0.0,
                deck: 0.0,
            };
            if let Some(visual) = self
                .items
                .iter()
                .find(|visual| visual.opacity > 0.1 && visual.key() == key.key())
            {
                item.rect = visual
                    .item
                    .rect
                    .intersection(self.viewport(visual.space_id()));
            } else {
                item.rect = Rect::default();
            }
        }
        layout
    }
}

#[derive(Debug)]
pub struct BarMotion {
    from: Presentation,
    target: Presentation,
    pub presented: Presentation,
    started: Instant,
    active: bool,
}

impl BarMotion {
    pub fn new(layout: &BarLayout, now: Instant) -> Self {
        let presented = Presentation::from_layout(layout);
        Self {
            from: presented.clone(),
            target: presented.clone(),
            presented,
            started: now,
            active: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn retarget(&mut self, layout: &BarLayout, now: Instant) {
        let target = Presentation::from_layout(layout);
        if self.target == target {
            return;
        }
        // Native toolbar controls and the menu-bar height update immediately.
        // Their clip region must not lag behind a configuration/display change.
        // A rebalanced Space relocates immediately instead of crossing the camera.
        if (self.target.content_left - target.content_left).abs() > f64::EPSILON
            || (self.target.height - target.height).abs() > f64::EPSILON
            || self.target.split != target.split
        {
            self.from = target.clone();
            self.presented = target.clone();
            self.target = target;
            self.active = false;
            return;
        }
        // Retarget from the last frame actually submitted, including unfinished fades.
        self.from = self.presented.clone();
        self.target = target;
        self.started = now;
        self.active = true;
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.started).as_secs_f64() / DURATION;
        if elapsed >= 1.0 {
            self.presented = self.target.clone();
            self.active = false;
            return true;
        }
        let t = 1.0 - (1.0 - elapsed).powi(3);
        let mut items = Vec::new();
        for old in &self.from.items {
            if self
                .target
                .items
                .iter()
                .any(|target| target.key() == old.key())
                || self.target.space_rect(old.space_id()).is_none()
            {
                continue;
            }
            let mut end = old.clone();
            end.opacity = 0.0;
            end.item.rect = entry_rect(old, &self.target);
            items.push(interpolate(old, &end, t));
        }
        for target in &self.target.items {
            let old = self.from.items.iter().find(|old| old.key() == target.key());
            let mut start = target.clone();
            if old.is_none() {
                start.opacity = 0.0;
                start.item.rect = entry_rect(target, &self.from);
            }
            items.push(interpolate(old.unwrap_or(&start), target, t));
        }
        self.presented = Presentation {
            width: lerp(self.from.width, self.target.width, t),
            height: lerp(self.from.height, self.target.height, t),
            content_left: lerp(self.from.content_left, self.target.content_left, t),
            split: self.target.split.clone(),
            items,
        };
        true
    }
}

fn entry_rect(item: &VisualItem, frame: &Presentation) -> Rect {
    if matches!(
        item.item.kind,
        ItemKind::Window { .. } | ItemKind::Surface { .. }
    ) && let Some(anchor) = frame.items.iter().rev().find(|other| {
        other.space_id() == item.space_id()
            && matches!(
                other.item.kind,
                ItemKind::Window { .. } | ItemKind::Surface { .. }
            )
    }) {
        return Rect {
            x: anchor.item.rect.x,
            ..item.item.rect
        };
    }
    item.item.rect
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

fn interpolate(from: &VisualItem, to: &VisualItem, t: f64) -> VisualItem {
    let a = from.item.rect;
    let b = to.item.rect;
    let mut item = to.item.clone();
    item.rect = Rect {
        x: lerp(a.x, b.x, t),
        y: lerp(a.y, b.y, t),
        width: lerp(a.width, b.width, t),
        height: lerp(a.height, b.height, t),
    };
    VisualItem {
        item,
        opacity: lerp(from.opacity, to.opacity, t),
        emphasis: lerp(from.emphasis, to.emphasis, t),
        deck: lerp(from.deck, to.deck, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn layout(x: f64) -> BarLayout {
        BarLayout {
            width: 200.0 + x,
            height: 34.0,
            content_width: 200.0 + x,
            content_left: 0.0,
            split: None,
            items: vec![
                PlacedItem {
                    rect: Rect {
                        x: 4.0,
                        y: 4.0,
                        width: 190.0 + x,
                        height: 26.0,
                    },
                    kind: ItemKind::Space {
                        space_id: 1,
                        ordinal: 0,
                        visible: true,
                        focused: true,
                        empty: false,
                        fullscreen: false,
                    },
                },
                PlacedItem {
                    rect: Rect {
                        x: 20.0 + x,
                        y: 6.0,
                        width: 22.0,
                        height: 22.0,
                    },
                    kind: ItemKind::Focus { space_id: 1 },
                },
            ],
        }
    }

    #[test]
    fn toolbar_visibility_and_screen_height_update_clip_regions_immediately() {
        let display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 800.0, 0.0);
        let mut motion = BarMotion::new(&initial, now);
        for (height, mission_control, desktop) in [
            (22.0, false, false),
            (37.0, true, false),
            (24.0, true, true),
        ] {
            let preferences = crate::bar::BarPreferences {
                show_mission_control: mission_control,
                show_desktop: desktop,
                ..Default::default()
            }
            .for_menu_height(height);
            let target =
                BarLayout::resolve_with_metrics(&display, 800.0, 50.0, preferences.metrics());
            motion.retarget(&target, now);
            assert!(!motion.is_active());
            assert!((motion.presented.height - height).abs() < f64::EPSILON);
            assert!(
                (motion.presented.content_left - preferences.toolbar_width()).abs() < f64::EPSILON
            );
            let hits = motion.presented.interaction_layout(&target);
            for item in hits.items.iter().filter(|item| item.rect.width > 0.0) {
                assert!(item.rect.x >= target.content_left);
            }
        }
    }

    #[test]
    fn rapid_retarget_starts_at_presented_frame_without_resetting_on_repeat() {
        let now = Instant::now();
        let mut motion = BarMotion::new(&layout(0.0), now);
        motion.retarget(&layout(80.0), now);
        motion.advance(now + Duration::from_millis(70));
        let middle = motion.presented.clone();
        assert!(middle.width > 200.0 && middle.width < 280.0);
        motion.retarget(&layout(10.0), now + Duration::from_millis(70));
        motion.advance(now + Duration::from_millis(70));
        assert_eq!(middle, motion.presented);
        motion.retarget(&layout(10.0), now + Duration::from_millis(200));
        motion.advance(now + Duration::from_millis(311));
        assert!(!motion.is_active());
        assert_eq!(motion.presented, Presentation::from_layout(&layout(10.0)));
        assert!(!motion.advance(now + Duration::from_secs(1)));
    }

    #[test]
    fn focus_interpolates_and_viewport_never_exceeds_bar() {
        let now = Instant::now();
        let mut motion = BarMotion::new(&layout(0.0), now);
        motion.retarget(&layout(100.0), now);
        motion.advance(now + Duration::from_millis(100));
        let focus = motion
            .presented
            .items
            .iter()
            .find(|item| matches!(item.item.kind, ItemKind::Focus { .. }))
            .unwrap();
        assert!(focus.item.rect.x > 20.0 && focus.item.rect.x < 120.0);
        let clip = motion.presented.viewport(1);
        assert!(clip.contains(focus.item.rect.x, focus.item.rect.y));
        assert!(clip.x >= 0.0 && clip.x + clip.width <= motion.presented.width);
        let mut frame = motion.presented;
        frame.items[0].item.rect.x = -50.0;
        assert!(frame.viewport(1).x.abs() < f64::EPSILON);
    }

    #[test]
    fn space_switch_interpolates_decks_and_clips_every_frame() {
        let now = Instant::now();
        let mut display = crate::bar::layout::tests::display();
        let initial = BarLayout::resolve(&display, 160.0, 0.0);
        display.spaces[1].visible = false;
        display.spaces[2].visible = true;
        let target = BarLayout::resolve(&display, 160.0, 0.0);
        let mut motion = BarMotion::new(&initial, now);
        motion.retarget(&target, now);
        for millis in [0, 30, 80, 140, 240] {
            motion.advance(now + Duration::from_millis(millis));
            for item in &motion.presented.items {
                let clip = motion.presented.viewport(item.space_id());
                assert!(clip.x >= 0.0);
                assert!(clip.x + clip.width <= motion.presented.width);
                assert!(clip.y + clip.height <= motion.presented.height);
            }
        }
        assert_eq!(motion.presented, Presentation::from_layout(&target));
    }

    #[test]
    fn hit_testing_uses_presented_positions_and_excludes_departed_windows() {
        let now = Instant::now();
        let mut display = crate::bar::layout::tests::display();
        let initial = BarLayout::resolve(&display, 1200.0, 0.0);
        display.spaces[1].columns.swap(0, 1);
        display.spaces[1].floating.clear();
        let target = BarLayout::resolve(&display, 1200.0, 0.0);
        let mut motion = BarMotion::new(&initial, now);
        motion.retarget(&target, now);
        motion.advance(now + Duration::from_millis(70));
        let hits = motion.presented.interaction_layout(&target);
        assert!(
            !hits
                .items
                .iter()
                .any(|item| matches!(item.kind, ItemKind::Window { window_id: 4, .. }))
        );
        for hit in hits
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Window { space_id: 11, .. }))
        {
            let visual = motion
                .presented
                .items
                .iter()
                .find(|visual| visual.item.kind == hit.kind)
                .unwrap();
            assert_eq!(hit.rect, visual.item.rect);
            let destination = target
                .items
                .iter()
                .find(|item| item.kind == hit.kind)
                .unwrap();
            assert_ne!(hit.rect, destination.rect);
        }
    }

    #[test]
    fn collapse_and_interrupted_expansion_preserve_single_window_vertical_geometry() {
        let now = Instant::now();
        let mut display = crate::bar::layout::tests::display();
        let mut extra = display.spaces[1].columns[1].clone();
        extra.windows[0].id = 8;
        display.spaces[1].columns.push(extra);
        let initial = BarLayout::resolve(&display, 1200.0, 0.0);
        let mut motion = BarMotion::new(&initial, now);
        let original = motion.presented.clone();
        display.spaces[1].visible = false;
        let collapsed = BarLayout::resolve(&display, 1200.0, 0.0);
        motion.retarget(&collapsed, now);
        for millis in [0, 30, 70, 120, 240] {
            motion.advance(now + Duration::from_millis(millis));
            for visual in &motion.presented.items {
                if !matches!(visual.item.kind, ItemKind::Window { space_id: 11, .. }) {
                    continue;
                }
                let before = original
                    .items
                    .iter()
                    .find(|item| item.key() == visual.key())
                    .unwrap();
                if matches!(before.item.kind, ItemKind::Window { stacked: true, .. }) {
                    continue;
                }
                assert!((visual.item.rect.y - before.item.rect.y).abs() < 0.001);
                assert!((visual.item.rect.height - before.item.rect.height).abs() < 0.001);
            }
            if millis == 70 {
                motion.retarget(&initial, now + Duration::from_millis(millis));
            }
        }
    }

    #[test]
    fn stack_collapse_and_interrupted_expansion_converge_to_target_geometry() {
        let now = Instant::now();
        let mut display = crate::bar::layout::tests::display();
        let expanded = BarLayout::resolve(&display, 1200.0, 0.0);
        let mut motion = BarMotion::new(&expanded, now);
        let original = motion.presented.clone();
        display.spaces[1].visible = false;
        let collapsed = BarLayout::resolve(&display, 1200.0, 0.0);
        let target = Presentation::from_layout(&collapsed);
        motion.retarget(&collapsed, now);
        for millis in [0, 30, 70, 120, 240] {
            motion.advance(now + Duration::from_millis(millis));
            for visual in &motion.presented.items {
                if !matches!(visual.item.kind, ItemKind::Window { space_id: 11, .. }) {
                    continue;
                }
                let before = original
                    .items
                    .iter()
                    .find(|item| item.key() == visual.key())
                    .unwrap();
                let after = target
                    .items
                    .iter()
                    .find(|item| item.key() == visual.key())
                    .unwrap();
                for (value, from, to) in [
                    (visual.item.rect.y, before.item.rect.y, after.item.rect.y),
                    (
                        visual.item.rect.width,
                        before.item.rect.width,
                        after.item.rect.width,
                    ),
                    (
                        visual.item.rect.height,
                        before.item.rect.height,
                        after.item.rect.height,
                    ),
                ] {
                    assert!(value >= from.min(to) - 0.001 && value <= from.max(to) + 0.001);
                }
            }
        }
        assert_eq!(motion.presented, target);
        for visual in &motion.presented.items {
            if matches!(visual.item.kind, ItemKind::Window { space_id: 11, .. }) {
                assert!((visual.item.rect.y - 6.0).abs() < f64::EPSILON);
                assert!((visual.item.rect.width - 22.0).abs() < f64::EPSILON);
                assert!((visual.item.rect.height - 22.0).abs() < f64::EPSILON);
            }
        }

        motion.retarget(&expanded, now + Duration::from_millis(300));
        motion.advance(now + Duration::from_millis(370));
        let interrupted = motion.presented.clone();
        motion.retarget(&collapsed, now + Duration::from_millis(370));
        motion.advance(now + Duration::from_millis(370));
        for visual in &motion.presented.items {
            let before = interrupted
                .items
                .iter()
                .find(|item| item.key() == visual.key())
                .unwrap();
            assert_eq!(visual.item.rect, before.item.rect);
        }
        motion.advance(now + Duration::from_millis(610));
        assert_eq!(motion.presented, target);
        motion.retarget(&expanded, now + Duration::from_millis(700));
        motion.advance(now + Duration::from_millis(940));
        assert_eq!(motion.presented, original);
    }

    #[test]
    fn collapsed_icons_have_no_backing_or_outline_but_expanded_stacks_keep_theirs() {
        let mut display = crate::bar::layout::tests::display();
        let layout = BarLayout::resolve(&display, 1200.0, 0.0);
        let frame = Presentation::from_layout(&layout);
        let stack = frame
            .items
            .iter()
            .find(|visual| matches!(visual.item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap();
        assert!(stack.icon_decoration_opacity() > 0.0);
        display.spaces[1].visible = false;
        let layout = BarLayout::resolve(&display, 1200.0, 0.0);
        let frame = Presentation::from_layout(&layout);
        for visual in &frame.items {
            if matches!(
                visual.item.kind,
                ItemKind::Window {
                    collapsed: true,
                    ..
                }
            ) {
                assert!(visual.icon_decoration_opacity().abs() < f64::EPSILON);
            }
        }
    }

    #[test]
    fn collapsing_never_reintroduces_dark_backing_during_motion() {
        let now = Instant::now();
        let mut display = crate::bar::layout::tests::display();
        let expanded = BarLayout::resolve(&display, 1200.0, 0.0);
        let mut motion = BarMotion::new(&expanded, now);
        display.spaces[1].visible = false;
        motion.retarget(&BarLayout::resolve(&display, 1200.0, 0.0), now);
        for millis in [0, 30, 100, 240] {
            motion.advance(now + Duration::from_millis(millis));
            for visual in &motion.presented.items {
                if matches!(
                    visual.item.kind,
                    ItemKind::Window {
                        collapsed: true,
                        ..
                    }
                ) {
                    assert!(visual.icon_decoration_opacity().abs() < f64::EPSILON);
                }
            }
        }
    }
}

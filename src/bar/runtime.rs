//! The Bar's decisions, separated from the `AppKit` effects that realise them.
//!
//! [`Bar`] owns the per-display interaction state ([`ViewState`]) and the
//! update/animate/collapse coordination. Everything it needs from the machine
//! arrives as data through [`BarSurface`], which the `AppKit` adapter satisfies;
//! everything it asks the machine to do leaves through the same port. See
//! [ADR 0008](../../docs/adr/0008-bar-surface-seam.md).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use spool_shared_types::commands::Action;

use super::drag::{BarDrag, DRAG_RELEASE_GRACE, DropTarget};
use super::layout::{
    BarLayout, BarMetrics, BarSurfaceGeometry, ItemKind, Rect, space_at, window_at,
};
use super::model::{BarDisplay, BarSnapshot};
use super::motion::{BarMotion, EasedProgress, Presentation, VisualItem};
use super::placement;
use super::preferences::BarPreferences;
use super::toolbar::{self, ToolbarButton};

/// Which interactions this Bar's capabilities allow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BarCapabilities {
    pub focus_spaces: bool,
    pub move_windows: bool,
}

/// The geometry the adapter read for one display, in `AppKit` coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BarScreenGeometry {
    pub frame: Rect,
    pub visible_frame: Rect,
    pub safe_area_top: f64,
    pub menu_bar_thickness: f64,
    pub notch_left: Rect,
    pub notch_right: Rect,
    /// The widest label the adapter measured among this display's Spaces.
    pub label_width: f64,
}

/// Pointer and queued-input snapshot, taken at the adapter edge once a frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct BarPoll {
    pub pointer: (f64, f64),
    pub mouse_button_down: bool,
    pub inputs: Vec<BarInput>,
}

/// One translated `AppKit` interaction, queued by the adapter and applied by
/// [`Bar::animate`]. The display it belongs to travels with it, so a secondary
/// display's Bar never routes through the active one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BarInput {
    Press {
        display_id: u32,
        point: (f64, f64),
    },
    Drag {
        display_id: u32,
        point: (f64, f64),
    },
    Release {
        display_id: u32,
        point: (f64, f64),
    },
    Scroll {
        display_id: u32,
        point: (f64, f64),
        delta: f64,
    },
    Cancel {
        display_id: u32,
    },
    Activate {
        display_id: u32,
        action: Action,
    },
}

impl BarInput {
    fn display_id(&self) -> u32 {
        match self {
            BarInput::Press { display_id, .. }
            | BarInput::Drag { display_id, .. }
            | BarInput::Release { display_id, .. }
            | BarInput::Scroll { display_id, .. }
            | BarInput::Cancel { display_id }
            | BarInput::Activate { display_id, .. } => *display_id,
        }
    }
}

/// What the Bar decided, on its way back out to the action bus.
#[derive(Debug, Default)]
pub(crate) struct BarOutcome {
    pub actions: Vec<Action>,
}

/// A live drag's floating preview: where it sits and what it shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DragPreview {
    pub rect: Rect,
    pub items: Vec<VisualItem>,
}

/// Everything the view needs to draw one Bar, with no state of its own.
#[derive(Clone, Debug)]
pub(crate) struct RenderFrame {
    pub presentation: Presentation,
    pub preferences: BarPreferences,
    pub band: Rect,
    pub handle: Rect,
    pub chrome_progress: f64,
    pub drag: Option<BarDrag>,
}

/// The seam. The pure [`Bar`] never reaches past this interface. It is
/// main-thread-only, matching the `NonSend` resource that owns it, so it must
/// not require `Send + Sync`.
pub(crate) trait BarSurface {
    /// Read one display's screen geometry, including a measured label width.
    fn screen_geometry(
        &self,
        display: &BarDisplay,
        preferences: &BarPreferences,
    ) -> Option<BarScreenGeometry>;

    /// Drain queued interactions and read the pointer this frame.
    fn poll(&mut self) -> BarPoll;

    /// Whether the pointer is on some Bar panel's own chrome.
    fn pointer_is_on_chrome(&self) -> bool;

    /// Create or reconcile the panel for a display.
    fn ensure_panel(&mut self, display_id: u32, preferences: &BarPreferences, frame: Rect);

    /// Close and forget a display's panel.
    fn remove_panel(&mut self, display_id: u32);

    /// Draw one Bar.
    fn present(&mut self, display_id: u32, frame: &RenderFrame);

    /// Let clicks through everywhere the Bar is not drawn.
    fn set_interactive(&mut self, display_id: u32, interactive: bool);

    /// Reconcile the toolbar buttons to exactly this list.
    fn sync_toolbar(
        &mut self,
        display_id: u32,
        preferences: &BarPreferences,
        origin: f64,
        slide: f64,
        buttons: &[ToolbarButton],
        hidden: bool,
    );

    /// Breathe the button under the pointer and settle the rest.
    fn set_button_highlight(&mut self, display_id: u32, action: Option<Action>);

    /// Show or move the floating drag preview.
    fn show_drag_preview(&mut self, display_id: u32, preview: &DragPreview);

    /// Take the floating drag preview away.
    fn hide_drag_preview(&mut self, display_id: u32);
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent capability/style flags.
pub(crate) struct ViewState {
    display: BarDisplay,
    layout: BarLayout,
    motion: BarMotion,
    surface: BarSurfaceGeometry,
    metrics: BarMetrics,
    /// The panel window's frame, used to turn the adapter's global pointer into
    /// the viewport coordinates the hit tests and hover use.
    panel_frame: Rect,
    /// The display's frame, which the drag preview clamps itself to.
    screen_frame: Rect,
    /// The last global pointer the adapter read, kept so a publish can place the
    /// drag preview without asking the surface again.
    pointer: Option<(f64, f64)>,
    space_scroll: HashMap<u64, f64>,
    can_focus_spaces: bool,
    can_move_windows: bool,
    pressed: Option<BarDrag>,
    release_observed_at: Option<Instant>,
    floating_order: HashMap<u64, Vec<i32>>,
    preferences: BarPreferences,
    /// Runtime-only collapse state: every Bar starts expanded and nothing
    /// persists this.
    collapsed: bool,
    /// Whether the pointer is on the Bar at all: its band while expanded, its
    /// handle either way. This is what makes the panel interactive, so the menu
    /// bar underneath keeps everything the Bar is not drawing.
    hovered: bool,
    /// Whether the pointer is on the handle, which is the one control that
    /// breathes.
    handle_hovered: bool,
    /// Which toolbar button the pointer is over, if any.
    hover_button: Option<Action>,
    /// Set when the panel's own rect must be recomputed: collapse and hover
    /// change the panel, not just its content.
    chrome_dirty: bool,
    /// Eased motion of the Bar between expanded and collapsed.
    chrome: EasedProgress,
    /// How far the handle has grown under the pointer. 0 is its resting size.
    handle_grow: EasedProgress,
}

impl ViewState {
    pub(crate) fn new(display: BarDisplay, prepared: &Prepared) -> Self {
        let layout = BarLayout::resolve_surface(
            &display,
            prepared.surface,
            &mut HashMap::new(),
            prepared.metrics,
        );
        let now = Instant::now();
        Self {
            display,
            motion: BarMotion::new(&layout, now),
            layout,
            surface: prepared.surface,
            metrics: prepared.metrics,
            panel_frame: prepared.panel_frame,
            screen_frame: prepared.screen_frame,
            pointer: None,
            space_scroll: HashMap::new(),
            can_focus_spaces: false,
            can_move_windows: false,
            pressed: None,
            release_observed_at: None,
            floating_order: HashMap::new(),
            preferences: prepared.preferences.clone(),
            collapsed: false,
            hovered: false,
            handle_hovered: false,
            hover_button: None,
            chrome_dirty: false,
            chrome: EasedProgress::new(true, now),
            handle_grow: EasedProgress::new(false, now),
        }
    }

    pub(crate) fn reconcile(
        &mut self,
        display: BarDisplay,
        prepared: &Prepared,
        capabilities: BarCapabilities,
    ) {
        self.display = display;
        self.can_focus_spaces = capabilities.focus_spaces;
        self.can_move_windows = capabilities.move_windows;
        self.surface = prepared.surface;
        self.metrics = prepared.metrics;
        self.panel_frame = prepared.panel_frame;
        self.screen_frame = prepared.screen_frame;
        self.preferences.clone_from(&prepared.preferences);
        if self
            .pressed
            .as_ref()
            .is_some_and(|drag| !drag.is_valid(&self.display))
        {
            self.pressed = None;
        }
        if !self.can_move_windows
            && let Some(drag) = &mut self.pressed
        {
            drag.target = None;
        }
        let target_missing = self
            .pressed
            .as_ref()
            .is_some_and(|drag| !drag.target_valid(&self.display));
        if target_missing && let Some(drag) = &mut self.pressed {
            drag.target = None;
        }
        self.sync_floating_order();
        self.relayout();
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.motion.is_active()
            || self.chrome.is_active()
            || self.handle_grow.is_active()
            || self.drag_active()
    }

    pub(crate) fn drag_active(&self) -> bool {
        self.pressed.as_ref().is_some_and(|drag| drag.active)
    }

    pub(crate) fn display_active(&self) -> bool {
        self.display.active
    }

    /// Records the global pointer a later publish uses to place the drag
    /// preview, without recomputing hover.
    pub(crate) fn set_pointer(&mut self, pointer: (f64, f64)) {
        self.pointer = Some(pointer);
    }

    /// Converts the adapter's global pointer into viewport coordinates and
    /// recomputes hover from it.
    pub(crate) fn update_pointer(&mut self, pointer: (f64, f64), now: Instant) {
        self.pointer = Some(pointer);
        let frame = self.panel_frame;
        let local = (pointer.0 - frame.x, frame.y + frame.height - pointer.1);
        self.update_hover(local, now);
    }

    pub(crate) fn drag_release_expired(&mut self, mouse_down: bool, now: Instant) -> bool {
        if mouse_down || !self.pressed.as_ref().is_some_and(|drag| drag.active) {
            self.release_observed_at = None;
            return false;
        }
        // Global button state can lead the queued mouseUp. This is only a
        // missing-event watchdog; normal releases must finish via mouseUp.
        let released = self.release_observed_at.get_or_insert(now);
        now.saturating_duration_since(*released) >= DRAG_RELEASE_GRACE
    }

    /// The whole menu-bar band, in viewport coordinates.
    pub(crate) fn panel_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: self.surface.width,
            height: self.layout.height,
        }
    }

    /// The band at the current progress: what is left of the Bar's own rect on
    /// screen. It slides up out of the panel as the Bar collapses.
    fn band_rect(&self) -> Rect {
        super::placement::band_rect(
            (self.surface.width, self.layout.height),
            self.chrome.progress(),
        )
    }

    /// The Bar Handle at the current progress, in viewport coordinates,
    /// including however far it has grown under the pointer.
    ///
    /// The one description the drawn shape and the hit target both come from, so
    /// they cannot drift apart. The grown rect contains the resting one and
    /// keeps its glued top edge, so a pointer that is inside the small shape is
    /// still inside the large one: growth can never drop the pointer out of
    /// hover and flip the handle back and forth every frame.
    fn handle_rect(&self) -> Rect {
        let handle = self.preferences.handle_metrics();
        let resting = super::placement::handle_rect(
            (self.surface.width, self.layout.height),
            self.surface.notch,
            self.chrome.progress(),
            handle,
        );
        let grown =
            super::placement::grown_handle_rect(resting, handle, self.surface.notch.is_some());
        let growth = self.handle_grow.progress();
        Rect {
            x: super::motion::lerp(resting.x, grown.x, growth),
            y: super::motion::lerp(resting.y, grown.y, growth),
            width: super::motion::lerp(resting.width, grown.width, growth),
            height: super::motion::lerp(resting.height, grown.height, growth),
        }
    }

    /// Whether a point grabs the handle, which toggles the Bar both ways.
    fn handle_at(&self, point: (f64, f64)) -> bool {
        self.handle_rect().contains(point.0, point.1)
    }

    fn set_collapsed(&mut self, collapsed: bool, now: Instant) {
        if self.collapsed == collapsed {
            return;
        }
        self.collapsed = collapsed;
        self.chrome.set_target(!collapsed, now);
        self.chrome_dirty = true;
    }

    fn take_chrome_dirty(&mut self) -> bool {
        std::mem::take(&mut self.chrome_dirty)
    }

    pub(crate) fn press_at(&mut self, point: (f64, f64)) -> Option<Action> {
        self.pressed = None;
        self.release_observed_at = None;
        if self.handle_at(point) {
            // The handle is always visible — it hangs below the band while
            // expanded — so one click toggles, with no reveal first.
            self.set_collapsed(!self.collapsed, Instant::now());
            return None;
        }
        if self.collapsed {
            // The rest of the band belongs to the menu bar underneath while the
            // Bar is collapsed, so nothing else here is live.
            return None;
        }
        let hit_layout = self.motion.presented.interaction_layout(&self.layout);
        if let Some(item) = window_at(&hit_layout, point)
            && let ItemKind::Window { space_id, .. } = item.kind
            && self.space_shows_windows(space_id)
        {
            // A drag can start in any Space that draws its windows; what a
            // release means is settled in `release_drag`, once it is known
            // whether the pointer moved.
            self.pressed = BarDrag::begin(&item, &self.motion.presented, point);
            return None;
        }
        space_at(&hit_layout, point)
            .filter(|_| self.can_focus_spaces)
            .map(|space_id| Action::FocusSpace { space_id })
    }

    fn relayout(&mut self) {
        let preview = self
            .pressed
            .as_ref()
            .map(|drag| drag.preview_display(&self.display));
        self.layout = BarLayout::resolve_surface(
            preview.as_ref().unwrap_or(&self.display),
            self.surface,
            &mut self.space_scroll,
            self.metrics,
        );
        self.motion.retarget(&self.layout, Instant::now());
    }

    pub(crate) fn drag_to(&mut self, point: (f64, f64), inside_view: bool) {
        self.release_observed_at = None;
        let hit_layout = self.motion.presented.interaction_layout(&self.layout);
        if let Some(drag) = &mut self.pressed {
            drag.move_pointer(point);
            drag.update_target(
                &self.display,
                &hit_layout,
                &self.motion.presented,
                self.can_move_windows && inside_view,
            );
        }
        self.relayout();
    }

    pub(crate) fn release_drag(&mut self, point: (f64, f64), inside_view: bool) -> Option<Action> {
        self.drag_to(point, inside_view);
        let pressed = self.pressed.take()?;
        let allowed = pressed.is_valid(&self.display)
            && (!pressed.active || self.can_move_windows && pressed.target_valid(&self.display));
        let action = if allowed {
            if pressed.active
                && let Some(DropTarget::Floating { anchor, before }) = pressed.target
            {
                self.reorder_floating(pressed.space_id, pressed.window_id, anchor, before);
            }
            pressed.action(self.space_is_visible(pressed.space_id))
        } else {
            None
        };
        self.relayout();
        action
    }

    pub(crate) fn scroll(&mut self, point: (f64, f64), delta: f64) -> bool {
        let hit_layout = self.motion.presented.interaction_layout(&self.layout);
        let Some(space_id) = space_at(&hit_layout, point) else {
            return false;
        };
        let max_scroll = self.layout.max_space_scroll(space_id);
        if max_scroll <= 0.0 {
            return false;
        }
        let offset = self.space_scroll.entry(space_id).or_insert(0.0);
        *offset = (*offset + delta).clamp(0.0, max_scroll);
        self.relayout();
        true
    }

    pub(crate) fn cancel_drag(&mut self) {
        self.pressed = None;
        self.relayout();
    }

    pub(crate) fn toggle_collapse(&mut self, now: Instant) {
        self.set_collapsed(!self.collapsed, now);
    }

    /// Recomputes hover from a pointer already in viewport coordinates and
    /// retargets the handle's growth.
    pub(crate) fn update_hover(&mut self, point: (f64, f64), now: Instant) {
        let handle = self.handle_rect();
        let band = self.panel_rect();
        let handle_hovered = handle.contains(point.0, point.1);
        let band_hovered = !self.collapsed && band.contains(point.0, point.1);
        let hovered = handle_hovered || band_hovered;
        let hover_button = if band_hovered {
            let controls = toolbar::configured_buttons(
                self.layout.height,
                self.preferences.show_mission_control,
                self.preferences.show_desktop,
            );
            let slide = self.band_rect().y;
            controls
                .iter()
                .find(|control| {
                    let rect = Rect {
                        x: control.rect.x + self.motion.presented.toolbar_origin,
                        y: control.rect.y + slide,
                        ..control.rect
                    };
                    point.0 >= rect.x
                        && point.0 <= rect.x + rect.width
                        && point.1 >= rect.y
                        && point.1 <= rect.y + rect.height
                })
                .map(|control| control.action.clone())
        } else {
            None
        };
        if hovered != self.hovered
            || handle_hovered != self.handle_hovered
            || hover_button != self.hover_button
        {
            self.hovered = hovered;
            self.handle_hovered = handle_hovered;
            self.hover_button = hover_button;
            self.chrome_dirty = true;
        }
        self.handle_grow.set_target(handle_hovered, now);
    }

    /// Advances the pause-free parts: motion, the dirty panel, and every eased
    /// transition. Returns whether anything on screen changed.
    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let mut changed = self.motion.advance(now);
        changed |= self.take_chrome_dirty();
        changed |= self.chrome.advance(now);
        changed |= self.handle_grow.advance(now);
        changed
    }

    pub(crate) fn render_frame(&self) -> RenderFrame {
        RenderFrame {
            presentation: self.motion.presented.clone(),
            preferences: self.preferences.clone(),
            band: self.band_rect(),
            handle: self.handle_rect(),
            chrome_progress: self.chrome.progress(),
            drag: self
                .pressed
                .clone()
                .filter(|drag| drag.active && self.can_move_windows),
        }
    }

    /// Sends every non-drawing effect for this display.
    pub(crate) fn publish(&self, display_id: u32, surface: &mut dyn BarSurface) {
        let buttons = toolbar::configured_buttons(
            self.layout.height,
            self.preferences.show_mission_control,
            self.preferences.show_desktop,
        );
        surface.sync_toolbar(
            display_id,
            &self.preferences,
            self.motion.presented.toolbar_origin,
            self.band_rect().y,
            &buttons,
            self.chrome.progress() <= 0.01,
        );
        surface.set_button_highlight(display_id, self.hover_button.clone());
        surface.set_interactive(display_id, self.hovered);
        match self.drag_preview() {
            Some(preview) => surface.show_drag_preview(display_id, &preview),
            None => surface.hide_drag_preview(display_id),
        }
    }

    fn drag_preview(&self) -> Option<DragPreview> {
        let drag = self
            .pressed
            .as_ref()
            .filter(|drag| drag.active && self.can_move_windows)?;
        let pointer = self.pointer?;
        let (rect, items) = drag.ghost_geometry(pointer, self.screen_frame);
        Some(DragPreview { rect, items })
    }

    fn sync_floating_order(&mut self) {
        for space in &mut self.display.spaces {
            let observed = space
                .floating
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>();
            let order = self.floating_order.entry(space.id).or_default();
            order.retain(|window_id| observed.contains(window_id));
            for window_id in observed {
                if !order.contains(&window_id) {
                    order.push(window_id);
                }
            }
            space.floating.sort_by_key(|window| {
                order
                    .iter()
                    .position(|window_id| *window_id == window.id)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    fn reorder_floating(&mut self, space_id: u64, window_id: i32, anchor: i32, before: bool) {
        let Some(order) = self.floating_order.get_mut(&space_id) else {
            return;
        };
        let Some(source) = order.iter().position(|candidate| *candidate == window_id) else {
            return;
        };
        order.remove(source);
        let Some(anchor) = order.iter().position(|candidate| *candidate == anchor) else {
            return;
        };
        order.insert(if before { anchor } else { anchor + 1 }, window_id);
        self.sync_floating_order();
        self.relayout();
    }

    fn space_is_visible(&self, space_id: u64) -> bool {
        self.display
            .spaces
            .iter()
            .any(|space| space.id == space_id && space.visible)
    }

    /// Whether a Space draws its windows, and so whether they are worth aiming
    /// at. A collapsed Space draws a deck instead: clicking it means "take me
    /// there", not "this icon".
    fn space_shows_windows(&self, space_id: u64) -> bool {
        !self.preferences.collapse_inactive_spaces || self.space_is_visible(space_id)
    }
}

/// Everything derived once per display update that both a fresh and an existing
/// [`ViewState`] need: the resolved placement and the measured metrics.
pub(crate) struct Prepared {
    surface: BarSurfaceGeometry,
    metrics: BarMetrics,
    preferences: BarPreferences,
    panel_frame: Rect,
    screen_frame: Rect,
}

impl Prepared {
    fn from(geometry: BarScreenGeometry, base: &BarPreferences, display: &BarDisplay) -> Self {
        let menu_height = placement::menu_height(
            geometry.frame,
            geometry.visible_frame,
            geometry.safe_area_top,
            geometry.menu_bar_thickness,
        );
        let preferences = base.for_menu_height(menu_height);
        let panel = placement::panel_rect(geometry.frame, menu_height);
        let gap = placement::notch_gap(geometry.notch_left, geometry.notch_right, menu_height);
        let surface = placement::surface(panel, gap, preferences.notch_side);
        let panel_frame = placement::window_rect(panel, preferences.handle_metrics());
        let metrics = super::layout::display_metrics(display, &preferences, geometry.label_width);
        Self {
            surface,
            metrics,
            preferences,
            panel_frame,
            screen_frame: geometry.frame,
        }
    }
}

/// The Bar's decision core: every display's interaction state plus the update,
/// animate and collapse coordination. It holds no native handles.
#[derive(Debug, Default)]
pub(crate) struct Bar {
    preferences: BarPreferences,
    displays: HashMap<u32, ViewState>,
}

impl Bar {
    pub(crate) fn update(
        &mut self,
        snapshot: BarSnapshot,
        preferences: BarPreferences,
        surface: &mut dyn BarSurface,
    ) -> BarOutcome {
        self.preferences = preferences;
        let capabilities = BarCapabilities {
            focus_spaces: snapshot.can_focus_spaces,
            move_windows: snapshot.can_move_windows,
        };
        let mut retained = HashSet::new();
        for display in &snapshot.displays {
            let Some(geometry) = surface.screen_geometry(display, &self.preferences) else {
                continue;
            };
            retained.insert(display.id);
            let prepared = Prepared::from(geometry, &self.preferences, display);
            let state = self
                .displays
                .entry(display.id)
                .or_insert_with(|| ViewState::new(display.clone(), &prepared));
            state.reconcile(display.clone(), &prepared, capabilities);
            surface.ensure_panel(display.id, &prepared.preferences, prepared.panel_frame);
            state.publish(display.id, surface);
            surface.present(display.id, &state.render_frame());
        }

        let gone = self
            .displays
            .keys()
            .copied()
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for display_id in gone {
            self.displays.remove(&display_id);
            surface.remove_panel(display_id);
        }
        BarOutcome::default()
    }

    /// Applies queued interactions, advances every animation, and presents only
    /// the frames that changed.
    pub(crate) fn animate(&mut self, now: Instant, surface: &mut dyn BarSurface) -> BarOutcome {
        let poll = surface.poll();
        let mut outcome = BarOutcome::default();
        for input in poll.inputs {
            outcome
                .actions
                .extend(self.handle(input, poll.pointer, surface));
        }
        let ids = self.displays.keys().copied().collect::<Vec<_>>();
        for display_id in ids {
            let Some(state) = self.displays.get_mut(&display_id) else {
                continue;
            };
            state.update_pointer(poll.pointer, now);
            if state.drag_active() && state.drag_release_expired(poll.mouse_button_down, now) {
                state.cancel_drag();
            }
            let changed = state.advance(now);
            state.publish(display_id, surface);
            if changed {
                surface.present(display_id, &state.render_frame());
            }
        }
        outcome
    }

    /// Collapses or expands the Bar on the display the user is working on.
    pub(crate) fn toggle_collapse(
        &mut self,
        now: Instant,
        surface: &mut dyn BarSurface,
    ) -> BarOutcome {
        let Some(display_id) = self.active_display() else {
            return BarOutcome::default();
        };
        let Some(state) = self.displays.get_mut(&display_id) else {
            return BarOutcome::default();
        };
        state.toggle_collapse(now);
        state.publish(display_id, surface);
        surface.present(display_id, &state.render_frame());
        BarOutcome::default()
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.displays.values().any(ViewState::is_animating)
    }

    fn active_display(&self) -> Option<u32> {
        self.displays
            .iter()
            .find(|(_, state)| state.display_active())
            .map(|(display_id, _)| *display_id)
            .or_else(|| self.displays.keys().copied().min())
    }

    fn handle(
        &mut self,
        input: BarInput,
        pointer: (f64, f64),
        surface: &mut dyn BarSurface,
    ) -> Vec<Action> {
        let display_id = input.display_id();
        let Some(state) = self.displays.get_mut(&display_id) else {
            return Vec::new();
        };
        state.set_pointer(pointer);
        let mut actions = Vec::new();
        match input {
            BarInput::Press { point, .. } => {
                if let Some(action) = state.press_at(point) {
                    actions.push(action);
                }
            }
            BarInput::Drag { point, .. } => {
                let inside = state.panel_rect().contains(point.0, point.1);
                state.drag_to(point, inside);
            }
            BarInput::Release { point, .. } => {
                let inside = state.panel_rect().contains(point.0, point.1);
                if let Some(action) = state.release_drag(point, inside) {
                    actions.push(action);
                }
            }
            BarInput::Scroll { point, delta, .. } => {
                state.scroll(point, delta);
            }
            BarInput::Cancel { .. } => state.cancel_drag(),
            BarInput::Activate { action, .. } => {
                // The toolbar's own actions take the drag out from under them,
                // exactly as the view used to before dispatching.
                state.cancel_drag();
                actions.push(action);
            }
        }
        state.publish(display_id, surface);
        surface.present(display_id, &state.render_frame());
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout::BarAlign;
    use super::super::preferences::NotchSide;
    use super::*;
    use std::time::Duration;

    fn drag_state() -> (ViewState, (f64, f64)) {
        let display = crate::bar::layout::tests::display();
        // No AppKit font calls: this exercises the real event-state path offscreen.
        let preferences = BarPreferences {
            show_workspace_labels: false,
            ..BarPreferences::default()
        };
        let surface = BarSurfaceGeometry {
            width: 1200.0,
            notch: None,
            bias: NotchSide::Balanced,
        };
        let mut state = ViewState::new(
            display,
            &Prepared {
                surface,
                metrics: preferences.metrics(),
                preferences,
                panel_frame: Rect {
                    x: 0.0,
                    y: 100.0,
                    width: 1200.0,
                    height: 600.0,
                },
                screen_frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1200.0,
                    height: 800.0,
                },
            },
        );
        state.can_focus_spaces = true;
        state.can_move_windows = true;
        let item = state
            .layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap();
        let pressed = BarDrag::begin(
            item,
            &state.motion.presented,
            (item.rect.x + 2.0, item.rect.y + 2.0),
        );
        state.pressed = pressed;
        let target = state
            .layout
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    ItemKind::ColumnDrop {
                        anchor_window_id: 3,
                        placement: spool_shared_types::commands::Placement::After,
                        ..
                    }
                )
            })
            .unwrap();
        let point = (target.rect.x + target.rect.width / 2.0, target.rect.y + 8.0);
        (state, point)
    }

    /// Re-derives the layout metrics after a test mutates a preference; the
    /// update path would have passed these in with the new preferences.
    fn reload_metrics(state: &mut ViewState) {
        state.metrics =
            super::super::layout::display_metrics(&state.display, &state.preferences, 0.0);
    }

    #[test]
    fn space_blank_padding_is_clickable_throughout_animation() {
        let display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 1200.0);
        let mut motion = BarMotion::new(&initial, now);
        let mut expanded = display.clone();
        expanded.spaces[2].visible = true;
        let target = BarLayout::resolve(&expanded, 1200.0);
        motion.retarget(&target, now);
        for millis in [0, 30, 120, 240] {
            motion.advance(now + std::time::Duration::from_millis(millis));
            let frame = &motion.presented;
            let hits = frame.interaction_layout(&target);
            for space in &display.spaces {
                let rect = frame.space_rect(space.id).unwrap();
                for y in [1.0, frame.height - 1.0] {
                    let point = (rect.x + rect.width / 2.0, y);
                    assert!(window_at(&hits, point).is_none());
                    assert_eq!(
                        space_at(&hits, point),
                        Some(space.id),
                        "Space {} blank padding at y={y} must be clickable",
                        space.id
                    );
                }
            }
        }
    }

    #[test]
    fn space_background_leaves_menu_bar_gaps_without_shrinking_click_regions() {
        let display = crate::bar::layout::tests::display();
        for height in [22.0, 24.0, 37.0] {
            let prefs = BarPreferences::default().for_menu_height(height);
            let layout = BarLayout::resolve_with_metrics(
                &display,
                1200.0,
                &mut HashMap::new(),
                prefs.metrics(),
                BarAlign::Center,
            );
            let motion = BarMotion::new(&layout, Instant::now());
            let hits = motion.presented.interaction_layout(&layout);
            for item in &layout.items {
                let ItemKind::Space { space_id, .. } = item.kind else {
                    continue;
                };
                let background = item.space_background_rect();
                assert!((background.y - 3.0).abs() < f64::EPSILON);
                assert!((height - background.y - background.height - 3.0).abs() < f64::EPSILON);
                for y in [1.0, height - 1.0] {
                    let point = (item.rect.x + item.rect.width / 2.0, y);
                    assert!(!background.contains(point.0, point.1));
                    assert_eq!(space_at(&hits, point), Some(space_id));
                }
            }
            assert!(
                BarPreferences::rgba(&prefs.background_color, [1.0; 4])[3].abs() < f64::EPSILON
            );
        }
    }

    #[test]
    fn blank_space_clicks_dispatch_focus_without_changing_window_clicks() {
        let (mut state, _) = drag_state();
        for space_id in [10, 11, 12] {
            let rect = state.motion.presented.space_rect(space_id).unwrap();
            for y in [1.0, state.layout.height - 1.0] {
                let point = (rect.x + rect.width / 2.0, y);
                assert_eq!(state.press_at(point), Some(Action::FocusSpace { space_id }));
                assert!(state.pressed.is_none());
                state.can_focus_spaces = false;
                assert!(state.press_at(point).is_none());
                state.can_focus_spaces = true;
            }
        }
        let icon = state
            .layout
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap()
            .rect;
        let point = (icon.x + 2.0, icon.y + 2.0);
        assert!(state.press_at(point).is_none());
        assert_eq!(state.pressed.as_ref().unwrap().window_id, 1);
        assert_eq!(
            state.release_drag(point, true),
            Some(Action::FocusWindow { window_id: 1 })
        );
    }

    #[test]
    fn mouse_drag_path_animates_preview_and_only_returns_command_on_release() {
        let (mut state, point) = drag_state();
        let original = state.display.clone();
        let original_layout = state.layout.clone();
        state.drag_to(point, true);
        assert_ne!(state.layout, original_layout);
        assert!(state.motion.is_active());
        assert_eq!(state.display, original);
        state
            .motion
            .advance(Instant::now() + std::time::Duration::from_secs(1));
        assert!(
            state
                .pressed
                .as_ref()
                .unwrap()
                .gap_rect(&state.motion.presented)
                .is_some()
        );
        assert!(matches!(
            state.release_drag(point, true),
            Some(Action::ReorderColumn {
                window_id: 1,
                anchor_window_id: 3,
                ..
            })
        ));
        assert!(state.pressed.is_none());
        assert_eq!(state.display, original);
        assert_eq!(state.layout, original_layout);
    }

    #[test]
    fn drag_survives_button_release_before_the_native_mouse_up_arrives() {
        for cross_space in [false, true] {
            let (mut state, reorder_point) = drag_state();
            let source = state
                .layout
                .items
                .iter()
                .find(|item| matches!(item.kind, ItemKind::Window { window_id: 1, .. }))
                .unwrap()
                .rect;
            state.press_at((source.x + 2.0, source.y + 2.0));
            let point = if cross_space {
                let target = state.motion.presented.space_rect(10).unwrap();
                (target.x + target.width / 2.0, target.y + 2.0)
            } else {
                reorder_point
            };
            state.drag_to(point, true);
            assert!(state.pressed.as_ref().unwrap().target.is_some());
            let now = Instant::now();
            for millis in [0, 16, 100, 249] {
                assert!(
                    !state.drag_release_expired(false, now + Duration::from_millis(millis)),
                    "global button state must not cancel a native mouseUp that has not arrived yet"
                );
            }
            let expected = if cross_space {
                Action::MoveColumnToSpace {
                    window_id: 1,
                    space_id: 10,
                    move_focus: spool_shared_types::commands::MoveFocus::Follow,
                }
            } else {
                Action::ReorderColumn {
                    window_id: 1,
                    anchor_window_id: 3,
                    placement: spool_shared_types::commands::Placement::After,
                }
            };
            assert_eq!(state.release_drag(point, true), Some(expected));
            assert!(
                state.release_drag(point, true).is_none(),
                "release dispatches only once"
            );
        }
    }

    #[test]
    fn missing_mouse_up_expires_but_native_pointer_events_reset_the_watchdog() {
        let (mut state, point) = drag_state();
        state.drag_to(point, true);
        let now = Instant::now();
        assert!(!state.drag_release_expired(false, now));
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(249)));
        assert!(state.drag_release_expired(false, now + DRAG_RELEASE_GRACE));

        assert!(!state.drag_release_expired(true, now + Duration::from_millis(300)));
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(600)));
        state.drag_to(point, true);
        assert!(!state.drag_release_expired(false, now + Duration::from_millis(900)));

        let source = state
            .motion
            .presented
            .items
            .iter()
            .find(|item| matches!(item.item.kind, ItemKind::Window { window_id: 1, .. }))
            .unwrap()
            .item
            .rect;
        state.press_at((source.x + 2.0, source.y + 2.0));
        state.drag_to(point, true);
        assert!(!state.drag_release_expired(false, now + Duration::from_secs(2)));
    }

    #[test]
    fn toolbar_has_no_space_or_window_hit_targets_during_scroll_animation() {
        let display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 150.0);
        let target = BarLayout::resolve(&display, 150.0);
        let mut motion = BarMotion::new(&initial, now);
        motion.retarget(&target, now);
        for millis in [0, 30, 120, 240] {
            motion.advance(now + std::time::Duration::from_millis(millis));
            let frame = &motion.presented;
            let hits = frame.interaction_layout(&target);
            for button in toolbar::buttons(frame.height) {
                let point = (
                    button.rect.x + button.rect.width / 2.0,
                    button.rect.y + button.rect.height / 2.0,
                );
                assert!(window_at(&hits, point).is_none());
                assert!(space_at(&hits, point).is_none());
            }
            for hit in hits.items.iter().filter(|item| item.rect.width > 0.0) {
                assert!(hit.rect.x >= frame.content_left);
            }
        }
    }

    #[test]
    fn notch_clips_each_space_and_has_no_hits_during_scroll_or_rebalance() {
        let mut display = crate::bar::layout::tests::display();
        let surface = BarSurfaceGeometry {
            width: 520.0,
            notch: Some(Rect {
                x: 180.0,
                y: 0.0,
                width: 140.0,
                height: 34.0,
            }),
            bias: NotchSide::Balanced,
        };
        let now = Instant::now();
        let initial = BarLayout::resolve_surface(
            &display,
            surface,
            &mut HashMap::new(),
            BarMetrics::default(),
        );
        let mut motion = BarMotion::new(&initial, now);
        // A Space on each side of the notch scrolls inside its own slot; the
        // notch itself stays dead either way.
        let left = initial
            .spans
            .iter()
            .find(|span| span.rect.x < 180.0)
            .map(|span| span.space_id);
        let right = initial
            .spans
            .iter()
            .rev()
            .find(|span| span.rect.x > 320.0)
            .map(|span| span.space_id);
        for scrolled in [left, right, None] {
            let mut scroll = HashMap::new();
            if let Some(space_id) = scrolled {
                scroll.insert(space_id, f64::MAX);
            }
            let target =
                BarLayout::resolve_surface(&display, surface, &mut scroll, BarMetrics::default());
            motion.retarget(&target, now);
            for millis in [0, 30, 120, 240] {
                motion.advance(now + std::time::Duration::from_millis(millis));
                let frame = &motion.presented;
                let hits = frame.interaction_layout(&target);
                for x in [181.0, 250.0, 319.0] {
                    assert!(!frame.over_space_strip((x, 17.0)));
                    assert!(window_at(&hits, (x, 17.0)).is_none());
                    assert!(space_at(&hits, (x, 17.0)).is_none());
                }
                for visual in &frame.items {
                    let viewport = frame.viewport(visual.space_id());
                    let lane = frame.split.as_ref().unwrap().viewport(
                        visual.space_id(),
                        frame.width,
                        frame.height,
                        frame.content_left,
                    );
                    if viewport.width > 0.0 {
                        assert_eq!(viewport, viewport.intersection(lane));
                    }
                }
            }
        }
        display.spaces.remove(0);
        let target = BarLayout::resolve_surface(
            &display,
            surface,
            &mut HashMap::new(),
            BarMetrics::default(),
        );
        motion.retarget(&target, now);
        assert!(
            !motion.is_active(),
            "membership changes must not animate across the notch"
        );
        assert_eq!(motion.presented.split, target.split);
    }

    #[test]
    fn releasing_over_notch_cancels_an_existing_drop_preview() {
        let (mut state, _) = drag_state();
        state.surface = BarSurfaceGeometry {
            width: 1000.0,
            notch: Some(Rect {
                x: 440.0,
                y: 0.0,
                width: 120.0,
                height: state.layout.height,
            }),
            bias: NotchSide::Balanced,
        };
        state.relayout();
        let target = state
            .layout
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    ItemKind::ColumnDrop {
                        anchor_window_id: 3,
                        placement: spool_shared_types::commands::Placement::After,
                        ..
                    }
                )
            })
            .unwrap()
            .rect;
        // Activate the gesture before aiming at the drop slot: the slot can sit
        // inside the 4pt drag threshold of the grab point, and `active` is
        // sticky once set, so moving away first is enough.
        state.drag_to((900.0, target.y + 2.0), true);
        state.drag_to((target.x + target.width / 2.0, target.y + 2.0), true);
        assert!(state.pressed.as_ref().unwrap().target.is_some());
    }

    #[test]
    fn releasing_a_window_over_either_toolbar_button_cancels_the_drag() {
        for button in toolbar::buttons(34.0) {
            let (mut state, target) = drag_state();
            let original = state.display.clone();
            state.drag_to(target, true);
            assert!(state.pressed.as_ref().unwrap().target.is_some());
            let point = (
                button.rect.x + button.rect.width / 2.0,
                button.rect.y + button.rect.height / 2.0,
            );
            assert!(state.release_drag(point, true).is_none());
            assert!(state.pressed.is_none());
            assert_eq!(state.display, original);
        }
    }

    #[test]
    fn release_clears_preview_on_outside_drop_capability_loss_or_stale_source() {
        for scenario in 0..3 {
            let (mut state, point) = drag_state();
            state.drag_to(point, true);
            match scenario {
                1 => state.can_move_windows = false,
                2 => {
                    state.display.spaces[1].columns.remove(0);
                }
                _ => {}
            }
            assert!(state.release_drag(point, scenario != 0).is_none());
            assert!(state.pressed.is_none());
            let mut scroll = state.space_scroll.clone();
            assert_eq!(
                state.layout,
                BarLayout::resolve_with_metrics(
                    &state.display,
                    1200.0,
                    &mut scroll,
                    state.preferences.metrics(),
                    BarAlign::Center,
                )
            );
        }
    }

    #[test]
    fn scrolled_icons_are_not_hittable_outside_their_space() {
        let mut display = crate::bar::layout::tests::display();
        let template = display.spaces[1].floating[0].clone();
        for id in 20..40 {
            let mut window = template.clone();
            window.id = id;
            display.spaces[1].floating.push(window);
        }
        let mut scroll = HashMap::new();
        scroll.insert(11, 60.0);
        let layout = BarLayout::resolve_with_metrics(
            &display,
            700.0,
            &mut scroll,
            BarMetrics::default(),
            BarAlign::Center,
        );
        let slot = layout
            .spans
            .iter()
            .find(|span| span.space_id == 11)
            .expect("focused slot")
            .rect;
        let escaped = layout
            .items
            .iter()
            .find(|item| {
                matches!(item.kind, ItemKind::Window { space_id: 11, .. }) && item.rect.x < slot.x
            })
            .expect("an icon is scrolled out of its slot");
        let outside = (
            escaped.rect.x + 1.0,
            escaped.rect.y + escaped.rect.height / 2.0,
        );
        assert!(outside.0 < slot.x, "the probe point is left of the slot");
        let hit = window_at(&layout, outside);
        assert_ne!(
            hit.as_ref().map(|item| item.kind.space_id()),
            Some(11),
            "a clipped icon must not be hit through its raw rect"
        );

        let visible = layout
            .items
            .iter()
            .find(|item| {
                matches!(item.kind, ItemKind::Window { space_id: 11, .. })
                    && item.rect.x >= slot.x
                    && item.rect.x + item.rect.width <= slot.x + slot.width
            })
            .expect("an icon remains inside the slot");
        assert!(
            window_at(
                &layout,
                (
                    visible.rect.x + 1.0,
                    visible.rect.y + visible.rect.height / 2.0
                )
            )
            .is_some(),
            "icons inside the slot stay hittable"
        );
    }

    #[test]
    fn collapsing_slides_the_band_out_and_leaves_the_handle_behind() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        assert_eq!(state.band_rect(), band, "expanded, the band is the band");
        let expanded_handle = state.handle_rect();
        assert!(
            (expanded_handle.y - band.height).abs() < f64::EPSILON,
            "the handle is glued to the band's bottom edge, below it"
        );
        assert!(
            expanded_handle.y + expanded_handle.height
                <= band.height
                    + super::super::placement::window_overhang(state.preferences.handle_metrics(),)
                    + f64::EPSILON,
            "the panel window has room for it"
        );

        state.set_collapsed(true, Instant::now());
        state
            .chrome
            .advance(Instant::now() + Duration::from_secs(1));
        let collapsed_band = state.band_rect();
        assert!(
            (collapsed_band.y + collapsed_band.height).abs() < f64::EPSILON,
            "the band has left through the screen's top edge"
        );
        let collapsed_handle = state.handle_rect();
        assert!(
            collapsed_handle.y.abs() < f64::EPSILON,
            "the handle comes to rest flush with the screen top"
        );
        assert!((collapsed_handle.width - expanded_handle.width).abs() < f64::EPSILON);
        assert!((collapsed_handle.height - expanded_handle.height).abs() < f64::EPSILON);
    }

    #[test]
    fn the_drawn_shape_and_the_hit_target_are_one_geometry() {
        let (state, _) = drag_state();
        let handle = state.handle_rect();
        assert!(state.handle_at((
            handle.x + handle.width / 2.0,
            handle.y + handle.height / 2.0
        )));
        assert!(!state.handle_at((handle.x - 1.0, handle.y + 1.0)));
        assert!(!state.handle_at((
            handle.x + handle.width / 2.0,
            handle.y + handle.height + 1.0
        )));
    }

    #[test]
    fn the_handle_toggles_collapse_and_the_rest_of_the_band_does_not() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        assert!(!state.collapsed, "every Bar starts expanded");
        // The band is not a collapse target: its clicks belong to Spaces and
        // windows, exactly as they did before.
        state.press_at((2.0, 2.0));
        assert!(!state.collapsed, "the band does not collapse the Bar");

        // The handle does, in one click, with no reveal first.
        let handle = state.handle_rect();
        assert!(
            state
                .press_at((handle.x + handle.width / 2.0, handle.y + 1.0))
                .is_none()
        );
        assert!(state.collapsed, "the handle collapses the Bar");
        assert!(state.take_chrome_dirty(), "the panel must be rewritten");

        // Collapsed, only the handle is live: the rest of the band stays the
        // menu bar's.
        state
            .chrome
            .advance(Instant::now() + Duration::from_secs(1));
        let collapsed = state.handle_rect();
        assert!(collapsed.y.abs() < f64::EPSILON);
        assert!(state.press_at((band.width - 4.0, 2.0)).is_none());
        assert!(state.collapsed, "the far end of the band is the menu bar's");
        assert!(
            state
                .press_at((collapsed.x + 2.0, collapsed.y + 2.0))
                .is_none()
        );
        assert!(!state.collapsed, "the handle expands the Bar again");
    }

    #[test]
    fn a_window_in_another_space_is_clicked_and_dragged_like_one_here() {
        // Space 12 is not the one macOS is showing, and with
        // `bar.collapse_inactive_spaces` off its windows are drawn, so they are
        // aimed at rather than treated as one "take me there" target.
        let (mut state, _) = drag_state();
        let point_of = |state: &ViewState, window_id: i32| {
            let visual = state
                .motion
                .presented
                .items
                .iter()
                .find(|visual| {
                    matches!(
                        visual.item.kind,
                        ItemKind::Window {
                            window_id: id,
                            space_id: 12,
                            ..
                        } if id == window_id
                    )
                })
                .expect("a window drawn in the inactive Space");
            (
                visual.item.rect.x + visual.item.rect.width / 2.0,
                visual.item.rect.y + visual.item.rect.height / 2.0,
            )
        };
        let point = point_of(&state, 7);

        // Pressing starts a drag, so the release decides what it meant. Which
        // member of a stacked column is on top is the layout's business: take
        // the one the press actually grabbed.
        assert!(state.press_at(point).is_none());
        let pressed = state
            .pressed
            .clone()
            .expect("a drag can start in any Space that draws its windows");
        assert_eq!(pressed.space_id, 12);
        let window_id = pressed.window_id;
        assert_eq!(
            state.release_drag(point, true),
            Some(Action::FocusWindowInSpace {
                window_id,
                space_id: 12,
            }),
            "a click there has to say which Space to go to"
        );

        // With inactive Spaces collapsed the same card is the Space: a deck
        // shows what is in a Space, it is not a set of separate targets.
        state.preferences.collapse_inactive_spaces = true;
        reload_metrics(&mut state);
        state.relayout();
        let point = point_of(&state, window_id);
        assert_eq!(
            state.press_at(point),
            Some(Action::FocusSpace { space_id: 12 }),
            "the press switches Spaces"
        );
        assert!(state.pressed.is_none(), "a deck card is not a drag handle");
    }

    #[test]
    fn no_space_sits_under_the_notch_collar_even_when_it_grows() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        state.surface.notch = Some(Rect {
            x: 500.0,
            y: 0.0,
            width: 180.0,
            height: band.height,
        });
        // Taller than the horizontal padding: a lane that only kept clear of the
        // Notch itself would now show through the collar's ears.
        state.preferences.handle_height = 12.0;
        reload_metrics(&mut state);
        state.relayout();
        assert!(!state.layout.spans.is_empty(), "there are Spaces to place");

        for grown in [false, true] {
            state.handle_grow = EasedProgress::new(grown, Instant::now());
            let collar = state.handle_rect();
            assert!(
                collar.width >= 180.0 + state.preferences.handle_height * 2.0 - f64::EPSILON,
                "the collar reaches past the notch: {collar:?}"
            );
            for span in &state.layout.spans {
                let left_of_it = span.rect.x + span.rect.width <= collar.x + f64::EPSILON;
                let right_of_it = span.rect.x >= collar.x + collar.width - f64::EPSILON;
                assert!(
                    left_of_it || right_of_it,
                    "grown={grown}: Space {} ({:?}) sits under the collar {collar:?}",
                    span.space_id,
                    span.rect
                );
            }
        }
    }

    #[test]
    fn a_collapsed_collar_is_one_target_across_the_notch() {
        let (mut state, _) = drag_state();
        let band = state.panel_rect();
        state.surface.notch = Some(Rect {
            x: 500.0,
            y: 0.0,
            width: 180.0,
            height: band.height,
        });
        let handle = state.preferences.handle_metrics();
        state.set_collapsed(true, Instant::now());
        let collar = state.handle_rect();
        assert!(
            (collar.width - (180.0 + handle.height * 2.0)).abs() < f64::EPSILON,
            "the collar clears the notch"
        );
        assert!(
            (collar.height - (band.height + handle.height)).abs() < f64::EPSILON,
            "and reaches below the band, where its chin shows"
        );
        // The middle of the collar sits behind the camera housing, so it is
        // invisible — but it is still one target, because the ears and the chin
        // belong to the same control.
        assert!(
            state
                .press_at((collar.x + collar.width / 2.0, band.height + 2.0))
                .is_none()
        );
        assert!(!state.collapsed, "the collar expands the Bar");
    }

    #[test]
    fn overlapping_stack_hit_testing_follows_focused_paint_layer() {
        let mut display = crate::bar::layout::tests::display();
        let now = Instant::now();
        let initial = BarLayout::resolve(&display, 1200.0);
        let mut motion = BarMotion::new(&initial, now);
        for (step, focused) in [Some(2), None, Some(1)].into_iter().enumerate() {
            for window in &mut display.spaces[1].columns[0].windows {
                window.focused = focused == Some(window.id);
            }
            let target = BarLayout::resolve(&display, 1200.0);
            let start = now + std::time::Duration::from_millis(u64::try_from(step).unwrap() * 100);
            motion.retarget(&target, start);
            motion.advance(start + std::time::Duration::from_millis(50));
            let hits = motion.presented.interaction_layout(&target);
            let front = hits
                .items
                .iter()
                .rev()
                .find(|item| {
                    matches!(
                        item.kind,
                        ItemKind::Window {
                            space_id: 11,
                            column_window_id: Some(1),
                            ..
                        }
                    )
                })
                .unwrap();
            let point = (
                front.rect.x + front.rect.width / 2.0,
                front.rect.y + front.rect.height / 2.0,
            );
            let hit = window_at(&hits, point).unwrap();
            assert!(
                matches!(hit.kind, ItemKind::Window { window_id, column_window_id: Some(1), .. } if window_id == focused.unwrap_or(1))
            );
            let exposed_top =
                window_at(&hits, (front.rect.x + front.rect.width / 2.0, 7.0)).unwrap();
            assert!(matches!(
                exposed_top.kind,
                ItemKind::Window { window_id: 1, .. }
            ));
            assert_eq!(
                display.spaces[1].columns[0]
                    .windows
                    .iter()
                    .map(|window| window.id)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }
    }

    /// An in-memory [`BarSurface`] that records every effect and replays queued
    /// inputs, so the seam is exercised without a window server.
    #[derive(Default)]
    struct RecordingSurface {
        geometries: HashMap<u32, BarScreenGeometry>,
        ensured: Vec<(u32, Rect)>,
        removed: Vec<u32>,
        presents: Vec<(u32, RenderFrame)>,
        interactive: Vec<(u32, bool)>,
        toolbars: Vec<(u32, Vec<Action>, bool)>,
        highlights: Vec<(u32, Option<Action>)>,
        previews: Vec<u32>,
        hidden_previews: Vec<u32>,
        pointer: (f64, f64),
        mouse_button_down: bool,
        inputs: Vec<BarInput>,
        on_chrome: bool,
    }

    impl BarSurface for RecordingSurface {
        fn screen_geometry(
            &self,
            display: &BarDisplay,
            _preferences: &BarPreferences,
        ) -> Option<BarScreenGeometry> {
            self.geometries.get(&display.id).copied()
        }

        fn poll(&mut self) -> BarPoll {
            BarPoll {
                pointer: self.pointer,
                mouse_button_down: self.mouse_button_down,
                inputs: std::mem::take(&mut self.inputs),
            }
        }

        fn pointer_is_on_chrome(&self) -> bool {
            self.on_chrome
        }

        fn ensure_panel(&mut self, display_id: u32, _preferences: &BarPreferences, frame: Rect) {
            self.ensured.push((display_id, frame));
        }

        fn remove_panel(&mut self, display_id: u32) {
            self.removed.push(display_id);
        }

        fn present(&mut self, display_id: u32, frame: &RenderFrame) {
            self.presents.push((display_id, frame.clone()));
        }

        fn set_interactive(&mut self, display_id: u32, interactive: bool) {
            self.interactive.push((display_id, interactive));
        }

        fn sync_toolbar(
            &mut self,
            display_id: u32,
            _preferences: &BarPreferences,
            _origin: f64,
            _slide: f64,
            buttons: &[ToolbarButton],
            hidden: bool,
        ) {
            self.toolbars.push((
                display_id,
                buttons.iter().map(|button| button.action.clone()).collect(),
                hidden,
            ));
        }

        fn set_button_highlight(&mut self, display_id: u32, action: Option<Action>) {
            self.highlights.push((display_id, action));
        }

        fn show_drag_preview(&mut self, display_id: u32, _preview: &DragPreview) {
            self.previews.push(display_id);
        }

        fn hide_drag_preview(&mut self, display_id: u32) {
            self.hidden_previews.push(display_id);
        }
    }

    fn geometry() -> BarScreenGeometry {
        BarScreenGeometry {
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
            },
            visible_frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 776.0,
            },
            safe_area_top: 0.0,
            menu_bar_thickness: 24.0,
            notch_left: Rect::default(),
            notch_right: Rect::default(),
            label_width: 0.0,
        }
    }

    fn snapshot(displays: Vec<BarDisplay>) -> BarSnapshot {
        BarSnapshot {
            can_focus_spaces: true,
            can_move_windows: true,
            displays,
        }
    }

    /// A second display whose Space ids differ from the first, so a routing
    /// mistake cannot hide behind identical geometry.
    fn second_display() -> BarDisplay {
        let mut display = crate::bar::layout::tests::display();
        display.id = 2;
        display.active = false;
        display.spaces[0].id = 20;
        display.spaces[1].id = 21;
        display.spaces[2].id = 22;
        display
    }

    fn recording(displays: &[BarDisplay]) -> RecordingSurface {
        let mut surface = RecordingSurface::default();
        for display in displays {
            surface.geometries.insert(display.id, geometry());
        }
        surface
    }

    fn window_frame(surface: &RecordingSurface, display_id: u32) -> Rect {
        surface
            .ensured
            .iter()
            .rev()
            .find(|(id, _)| *id == display_id)
            .unwrap()
            .1
    }

    fn presented(surface: &RecordingSurface, display_id: u32) -> RenderFrame {
        surface
            .presents
            .iter()
            .rev()
            .find(|(id, _)| *id == display_id)
            .unwrap()
            .1
            .clone()
    }

    /// Viewport coordinates (Y-down, origin at the panel's top-left) into the
    /// global coordinates the adapter reads the pointer in.
    fn to_global(frame: Rect, local: (f64, f64)) -> (f64, f64) {
        (frame.x + local.0, frame.y + frame.height - local.1)
    }

    fn icon_point(frame: &RenderFrame, window_id: i32) -> (f64, f64) {
        let rect = frame
            .presentation
            .items
            .iter()
            .find(|visual| matches!(visual.item.kind, ItemKind::Window { window_id: id, .. } if id == window_id))
            .unwrap()
            .item
            .rect;
        (rect.x + 2.0, rect.y + 2.0)
    }

    #[test]
    fn update_keeps_one_panel_per_display_and_removes_the_vanished() {
        let displays = vec![crate::bar::layout::tests::display(), second_display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(
            snapshot(displays.clone()),
            BarPreferences::default(),
            &mut surface,
        );
        assert_eq!(
            surface
                .ensured
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        bar.update(
            snapshot(vec![second_display()]),
            BarPreferences::default(),
            &mut surface,
        );
        assert_eq!(surface.removed, vec![1]);
    }

    #[test]
    fn toggle_collapse_presents_only_the_active_display() {
        let displays = vec![crate::bar::layout::tests::display(), second_display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        surface.presents.clear();
        bar.toggle_collapse(Instant::now(), &mut surface);
        assert_eq!(
            surface
                .presents
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn a_secondary_displays_input_routes_to_its_own_state() {
        let displays = vec![crate::bar::layout::tests::display(), second_display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let frame = window_frame(&surface, 2);
        let render = presented(&surface, 2);
        let space = render.presentation.space_rect(20).unwrap();
        let point = (space.x + space.width / 2.0, 2.0);
        surface.pointer = to_global(frame, point);
        surface.inputs.push(BarInput::Press {
            display_id: 2,
            point,
        });
        let outcome = bar.animate(Instant::now(), &mut surface);
        assert_eq!(
            outcome.actions,
            vec![Action::FocusSpace { space_id: 20 }],
            "the press belongs to display 2, not active_display()"
        );
    }

    #[test]
    fn a_window_press_starts_a_drag_and_the_release_returns_its_action() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let frame = window_frame(&surface, 1);
        let render = presented(&surface, 1);
        let point = icon_point(&render, 1);
        surface.pointer = to_global(frame, point);
        surface.inputs.push(BarInput::Press {
            display_id: 1,
            point,
        });
        let started = bar.animate(Instant::now(), &mut surface);
        assert!(started.actions.is_empty(), "a press does not dispatch yet");
        surface.inputs.push(BarInput::Release {
            display_id: 1,
            point,
        });
        let released = bar.animate(Instant::now(), &mut surface);
        assert_eq!(released.actions, vec![Action::FocusWindow { window_id: 1 }]);
    }

    #[test]
    fn a_toolbar_activate_returns_its_action_and_no_input_is_silently_dropped() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        surface.inputs.push(BarInput::Activate {
            display_id: 1,
            action: Action::MissionControl,
        });
        let outcome = bar.animate(Instant::now(), &mut surface);
        assert_eq!(outcome.actions, vec![Action::MissionControl]);

        surface.inputs.push(BarInput::Cancel { display_id: 1 });
        assert!(bar.animate(Instant::now(), &mut surface).actions.is_empty());
    }

    #[test]
    fn an_invalid_release_returns_no_action_and_clears_nothing() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        surface.inputs.push(BarInput::Release {
            display_id: 1,
            point: (2.0, 2.0),
        });
        assert!(bar.animate(Instant::now(), &mut surface).actions.is_empty());
    }

    #[test]
    fn sync_toolbar_receives_the_configured_buttons_and_hidden_flag() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let (display_id, buttons, hidden) = surface.toolbars.last().unwrap();
        assert_eq!(*display_id, 1);
        assert_eq!(buttons, &vec![Action::MissionControl, Action::ShowDesktop]);
        assert!(!hidden, "an expanded Bar keeps its toolbar");
    }

    #[test]
    fn set_interactive_tracks_the_pointer_on_the_band() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let frame = window_frame(&surface, 1);
        let render = presented(&surface, 1);
        let band = render.band;
        let local = (band.x + band.width / 2.0, band.y + band.height / 2.0);
        surface.pointer = to_global(frame, local);
        bar.animate(Instant::now(), &mut surface);
        assert_eq!(surface.interactive.last(), Some(&(1, true)));
    }

    #[test]
    fn set_button_highlight_follows_the_pointer_onto_a_button() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let frame = window_frame(&surface, 1);
        let render = presented(&surface, 1);
        let origin = render.presentation.toolbar_origin;
        let button = toolbar::configured_buttons(render.presentation.height, true, true)
            .into_iter()
            .next()
            .unwrap();
        let local = (
            origin + button.rect.x + button.rect.width / 2.0,
            button.rect.y + button.rect.height / 2.0,
        );
        surface.pointer = to_global(frame, local);
        bar.animate(Instant::now(), &mut surface);
        assert_eq!(
            surface.highlights.last(),
            Some(&(1, Some(Action::MissionControl)))
        );
    }

    #[test]
    fn a_drag_begin_shows_the_preview_and_the_release_hides_it() {
        let displays = vec![crate::bar::layout::tests::display()];
        let mut bar = Bar::default();
        let mut surface = recording(&displays);
        bar.update(snapshot(displays), BarPreferences::default(), &mut surface);
        let frame = window_frame(&surface, 1);
        let render = presented(&surface, 1);
        let start = icon_point(&render, 1);
        let moved = (start.0 + 8.0, start.1 + 8.0);
        surface.pointer = to_global(frame, moved);
        surface.inputs.push(BarInput::Press {
            display_id: 1,
            point: start,
        });
        bar.animate(Instant::now(), &mut surface);
        surface.inputs.push(BarInput::Drag {
            display_id: 1,
            point: moved,
        });
        bar.animate(Instant::now(), &mut surface);
        assert!(surface.previews.contains(&1), "a live drag shows a preview");
        surface.inputs.push(BarInput::Release {
            display_id: 1,
            point: moved,
        });
        bar.animate(Instant::now(), &mut surface);
        assert!(
            surface.hidden_previews.contains(&1),
            "the release takes the preview away"
        );
    }
}

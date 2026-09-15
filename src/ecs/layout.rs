#[path = "height_intent.rs"]
mod height;
pub use height::{StackItemId, StackItemState};

#[path = "layout_intent.rs"]
mod intent;
pub use intent::{
    ColumnId, ColumnState, EffectiveColumnWidth, WidthConstraint, WidthIntent,
    WidthProjectionBlocked, project_column_width,
};

use bevy::app::{App, Plugin, Update};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut, Ref};
use bevy::ecs::component::Component;
use bevy::ecs::entity::{Entity, EntityHashMap, EntityHashSet};
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::{Changed, Has, Or, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::schedule::common_conditions::{not, resource_exists};
use bevy::ecs::system::{Commands, Populated, Query, Res};
use bevy::math::IRect;
use spool_shared_types::commands::Placement;
use std::collections::VecDeque;
use stdext::function_name;
use tracing::{Level, instrument, trace};

use crate::config::Config;
use crate::ecs::params::Windows;
use crate::ecs::window_frame::{checked_frame_size, checked_window_frame};
use crate::ecs::workspace::{PendingSpaceDestruction, WindowSpaceReassignmentPending};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DesiredWindowFrame, DockPosition, EnsureVisibleMarker,
    Initializing, LayoutPosition, Position, PresentedWindowFrame, RepositionMarker,
    ReshuffleAroundMarker, Scrolling, SpawnCommandsExt,
};
use crate::errors::{Error, Result};
use crate::manager::{Display, Origin, Size, Window};
use crate::platform::WorkspaceId;
use crate::util::round_px;

use super::reconcile::WindowUnavailable;

pub struct LayoutEventsPlugin;

/// A strip, its entity, origin, display, and whether it's the active one.
/// Shared by [`reshuffle_layout_strip`] and [`ensure_visible_in_strip`].
type StripPlacements<'w, 's> = Query<
    'w,
    's,
    (
        &'static LayoutStrip,
        Entity,
        &'static Position,
        &'static ChildOf,
        Option<Ref<'static, ActiveWorkspaceMarker>>,
    ),
    Without<PendingSpaceDestruction>,
>;

/// Displays paired with the Dock's current edge, which is what turns a display's
/// raw bounds into the usable viewport.
type DisplayViewports<'w, 's> = Query<'w, 's, (&'static Display, Option<&'static DockPosition>)>;

#[derive(Component)]
struct LayoutViewport(IRect);

type LayoutViewports<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Option<&'static ChildOf>,
        &'static mut LayoutStrip,
        &'static mut Position,
        Option<&'static mut RepositionMarker>,
        Option<&'static mut LayoutViewport>,
    ),
    Without<PendingSpaceDestruction>,
>;

/// Windows whose logical size changed this tick. Presentation and observed
/// geometry are projections and must never invalidate the layout by themselves.
type ResizedWindows<'w, 's> = Populated<
    'w,
    's,
    Entity,
    (
        (Changed<Bounds>, With<Window>),
        Without<WindowSpaceReassignmentPending>,
    ),
>;

/// Window frames as the layout writes them: the current origin, and the size and
/// slot the layout pass is free to overwrite.
type WindowFrames<'w, 's> = Query<
    'w,
    's,
    (
        &'static Position,
        &'static mut Bounds,
        &'static mut LayoutPosition,
        Option<&'static WindowUnavailable>,
    ),
    (
        Without<LayoutStrip>,
        With<Window>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

/// Windows the layout just assigned a new slot to, with the frame fields that
/// slot has to be translated into.
type RepositionedWindows<'w, 's> = Populated<
    'w,
    's,
    (
        Entity,
        &'static Window,
        &'static LayoutPosition,
        &'static mut Position,
        &'static Bounds,
        Option<&'static mut DesiredWindowFrame>,
        Option<&'static mut PresentedWindowFrame>,
        Option<&'static super::tiled_visibility::ParkedTile>,
    ),
    (
        Or<(Changed<LayoutPosition>, Changed<Bounds>)>,
        With<Window>,
        Without<LayoutStrip>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type ChangedLayoutStrips<'w, 's> = Populated<
    'w,
    's,
    (&'static LayoutStrip, &'static ChildOf),
    (Changed<LayoutStrip>, Without<PendingSpaceDestruction>),
>;

type ReshuffleMarkers<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static LayoutPosition),
    (
        With<ReshuffleAroundMarker>,
        Without<WindowSpaceReassignmentPending>,
        Without<super::tiled_visibility::ParkedTile>,
        Without<super::tiled_visibility::RestoringTile>,
    ),
>;

type EnsureVisibleMarkers<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static LayoutPosition),
    (
        With<EnsureVisibleMarker>,
        Without<WindowSpaceReassignmentPending>,
        Without<super::tiled_visibility::ParkedTile>,
        Without<super::tiled_visibility::RestoringTile>,
    ),
>;

type StableLayoutPositions<'w, 's> = Query<
    'w,
    's,
    &'static mut LayoutPosition,
    (
        With<Window>,
        Without<LayoutStrip>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type StableWorkspacePlacements<'w, 's> = Query<
    'w,
    's,
    (
        &'static LayoutStrip,
        &'static Position,
        Has<Scrolling>,
        &'static ChildOf,
    ),
    (With<LayoutStrip>, Without<PendingSpaceDestruction>),
>;

/// Clamp a window origin to the range where it still touches both viewport
/// edges. For an oversized window this range is reversed: from right-aligned
/// to left-aligned, which lets the strip pan across the hidden content.
pub(crate) fn clamp_origin_to_viewport(origin: Origin, size: Size, viewport: IRect) -> Origin {
    Origin::new(
        clamp_axis_origin(i64::from(origin.x), size.x, viewport.min.x, viewport.max.x),
        clamp_axis_origin(i64::from(origin.y), size.y, viewport.min.y, viewport.max.y),
    )
}

/// Preserves integer center rounding without overflowing the intermediate sum.
pub(crate) fn centered_origin_in_viewport(frame: IRect, size: Size, viewport: IRect) -> Origin {
    let center_x = i64::from(frame.min.x).midpoint(i64::from(frame.max.x));
    let center_y = i64::from(frame.min.y).midpoint(i64::from(frame.max.y));
    Origin::new(
        clamp_axis_origin(
            center_x - i64::from(size.x / 2),
            size.x,
            viewport.min.x,
            viewport.max.x,
        ),
        clamp_axis_origin(
            center_y - i64::from(size.y / 2),
            size.y,
            viewport.min.y,
            viewport.max.y,
        ),
    )
}

fn clamp_axis_origin(origin: i64, size: i32, near: i32, far: i32) -> i32 {
    let near = i64::from(near);
    let far = i64::from(far) - i64::from(size);
    let minimum = near.min(far).max(i64::from(i32::MIN));
    let maximum = near
        .max(far)
        .min(i64::from(i32::MAX) - i64::from(size).max(0));
    // Both the origin and its positive-size endpoint remain representable.
    #[allow(clippy::cast_possible_truncation)]
    {
        origin.clamp(minimum, maximum) as i32
    }
}

impl Plugin for LayoutEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // Wait for finish_setup before tiling: until then every window
                // sits in the active strip regardless of its real display.
                (
                    super::window_frame::apply_window_frame_requests,
                    super::tiled_visibility::release_detached,
                    display_viewport_changed,
                    super::triggers::refresh_column_width_defaults,
                    layout_sizes_changed,
                    layout_strip_changed,
                    reshuffle_layout_strip,
                    ensure_visible_in_strip,
                    position_layout_strips,
                    position_layout_windows,
                    super::tiled_visibility::finish_restore,
                )
                    .chain()
                    .after(super::systems::finish_setup)
                    .after(super::display::reconcile_displays)
                    .after(super::native_space::reconcile_native_spaces)
                    .after(super::workspace::reconcile_fullscreen_spaces)
                    .run_if(not(resource_exists::<Initializing>)),
            ),
        );
    }
}

/// Keep the strip's scroll offset local to its display, including while a
/// strip animation is running. Relative window sizes need not change when a
/// monitor moves, so viewport translation must invalidate global positions.
fn display_viewport_changed(
    displays: DisplayViewports,
    mut strips: LayoutViewports,
    config: Res<Config>,
    mut commands: Commands,
) {
    let inherited = WidthIntent::ViewportRatio(
        config
            .preset_column_widths()
            .first()
            .copied()
            .unwrap_or(0.5),
    );
    for (entity, child, mut strip, mut position, pending, previous) in &mut strips {
        let Some((display, dock)) = child.and_then(|child| displays.get(child.parent()).ok())
        else {
            let changed = strip
                .bypass_change_detection()
                .set_width_context(None, inherited);
            let height_changed = strip.bypass_change_detection().set_height_context(None);
            if changed || height_changed {
                strip.set_changed();
            }
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);
        let size = checked_frame_size(viewport);
        let changed = strip
            .bypass_change_detection()
            .set_width_context(size.map(|size| size.x), inherited);
        let height_changed = strip
            .bypass_change_detection()
            .set_height_context(size.map(|size| size.y));
        if changed || height_changed {
            strip.set_changed();
        }
        if checked_frame_size(viewport).is_none() {
            continue;
        }
        if let Some(mut previous) = previous {
            if previous.0 == viewport {
                continue;
            }
            let translate = |origin: Origin| -> Option<Origin> {
                Some(Origin::new(
                    i32::try_from(
                        i64::from(origin.x) + i64::from(viewport.min.x)
                            - i64::from(previous.0.min.x),
                    )
                    .ok()?,
                    i32::try_from(
                        i64::from(origin.y) + i64::from(viewport.min.y)
                            - i64::from(previous.0.min.y),
                    )
                    .ok()?,
                ))
            };
            let Some(next_position) = translate(position.0) else {
                continue;
            };
            let next_pending = if let Some(pending) = pending.as_ref() {
                let Some(next) = translate(pending.0) else {
                    continue;
                };
                Some(next)
            } else {
                None
            };
            if position.0 != next_position {
                position.0 = next_position;
            }
            if let (Some(mut pending), Some(next)) = (pending, next_pending)
                && pending.0 != next
            {
                pending.0 = next;
            }
            previous.0 = viewport;
        } else {
            commands.entity(entity).insert(LayoutViewport(viewport));
        }
        if !strip.is_fullscreen() {
            strip.set_changed();
        }
    }
}

/// Represents an item within a stack, which can either be a single window or a group of tabs.
#[derive(Clone, Debug, PartialEq)]
pub enum StackItem {
    /// A single window within the stack.
    Single(Entity),
    /// A group of tabs within the stack.
    Tabs(Vec<Entity>),
}

impl StackItem {
    /// Returns the top window entity in the item.
    pub fn top(&self) -> Option<Entity> {
        match self {
            StackItem::Single(id) => Some(*id),
            StackItem::Tabs(tabs) => tabs.first().copied(),
        }
    }

    /// Returns true if the item contains the specified entity.
    pub fn contains(&self, entity: Entity) -> bool {
        match self {
            StackItem::Single(id) => *id == entity,
            StackItem::Tabs(tabs) => tabs.contains(&entity),
        }
    }

    /// Returns an iterator over all window entities in this stack item.
    pub fn window_iter(&self) -> StackItemIter<'_> {
        match self {
            StackItem::Single(entity) => StackItemIter::Single(std::iter::once(*entity)),
            StackItem::Tabs(tabs) => StackItemIter::Tabs(tabs.iter().copied()),
        }
    }
}

pub enum StackItemIter<'a> {
    Single(std::iter::Once<Entity>),
    Tabs(std::iter::Copied<std::slice::Iter<'a, Entity>>),
}

impl Iterator for StackItemIter<'_> {
    type Item = Entity;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            StackItemIter::Single(iter) => iter.next(),
            StackItemIter::Tabs(iter) => iter.next(),
        }
    }
}

impl DoubleEndedIterator for StackItemIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            StackItemIter::Single(iter) => iter.next_back(),
            StackItemIter::Tabs(iter) => iter.next_back(),
        }
    }
}

/// Represents a single panel within a `LayoutStrip`, which can either hold a single window, a stack of items, or a group of tabs.
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    /// A panel containing a single window, identified by its `Entity`.
    Single(Entity),
    /// A panel containing a stack of items (windows or tabs), ordered from top to bottom.
    Stack(Vec<StackItem>),
    /// A panel containing a group of native tabs.
    Tabs(Vec<Entity>),
    /// A panel holding a window that macOS made natively fullscreen.
    Fullscreen(Entity),
}

impl Column {
    #[cfg(feature = "lua")]
    fn item_containing(&self, entity: Entity) -> Option<StackItem> {
        match self {
            Self::Single(id) => (*id == entity).then_some(StackItem::Single(*id)),
            Self::Tabs(tabs) => tabs
                .contains(&entity)
                .then(|| StackItem::Tabs(tabs.clone())),
            Self::Stack(items) => items.iter().find(|item| item.contains(entity)).cloned(),
            Self::Fullscreen(_) => None,
        }
    }

    /// Returns the top window entity in the panel.
    /// For a `Single` panel, it's the contained window.
    /// For a `Stack` or `Tabs`, it's the first window in the vector.
    pub fn top(&self) -> Option<Entity> {
        match self {
            Column::Single(id) | Column::Fullscreen(id) => Some(*id),
            Column::Stack(stack) => stack.first().and_then(StackItem::top),
            Column::Tabs(tabs) => tabs.first().copied(),
        }
    }

    /// Returns an iterator over all window entities in this column
    pub fn window_iter(&self) -> ColumnWindowIter<'_> {
        match self {
            Column::Single(entity) | Column::Fullscreen(entity) => {
                ColumnWindowIter::Single(std::iter::once(*entity))
            }
            Column::Tabs(tabs) => ColumnWindowIter::Tabs(tabs.iter().copied()),
            Column::Stack(items) => {
                ColumnWindowIter::Stack(items.iter().flat_map(StackItem::window_iter))
            }
        }
    }

    /// Returns the entity at the given index, or the last entity if the index exceeds the size.
    pub fn at_or_last(&self, index: usize) -> Option<Entity> {
        match self {
            Column::Single(id) | Column::Fullscreen(id) => Some(*id),
            Column::Stack(stack) => stack
                .get(index)
                .or_else(|| stack.last())
                .and_then(StackItem::top),
            Column::Tabs(tabs) => tabs.first().copied(),
        }
    }

    /// Returns the position of an entity within this column (0 for Single/Tabs, index for Stack).
    pub fn position_of(&self, entity: Entity) -> Option<usize> {
        match self {
            Column::Single(id) | Column::Fullscreen(id) => (*id == entity).then_some(0),
            Column::Stack(stack) => stack.iter().position(|item| item.contains(entity)),
            Column::Tabs(tabs) => tabs.contains(&entity).then_some(0),
        }
    }

    /// Moves the specified entity to the front of stack-local ordering.
    /// Native tab ordering is stable; the focused tab is tracked by `FocusedMarker`.
    pub fn move_to_front(&mut self, entity: Entity) {
        match self {
            Column::Single(_) | Column::Fullscreen(_) => {}
            Column::Stack(stack) => {
                if let Some(StackItem::Tabs(tabs)) =
                    stack.iter_mut().find(|item| item.contains(entity))
                    && let Some(pos) = tabs.iter().position(|&e| e == entity)
                {
                    tabs.swap(0, pos);
                }
            }
            Column::Tabs(tabs) => {
                if let Some(pos) = tabs.iter().position(|&e| e == entity) {
                    tabs.swap(0, pos);
                }
            }
        }
    }
}

pub enum ColumnWindowIter<'a> {
    Single(std::iter::Once<Entity>),
    Tabs(std::iter::Copied<std::slice::Iter<'a, Entity>>),
    Stack(
        std::iter::FlatMap<
            std::slice::Iter<'a, StackItem>,
            StackItemIter<'a>,
            fn(&'a StackItem) -> StackItemIter<'a>,
        >,
    ),
}

impl Iterator for ColumnWindowIter<'_> {
    type Item = Entity;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(iter) => iter.next(),
            Self::Tabs(iter) => iter.next(),
            Self::Stack(iter) => iter.next(),
        }
    }
}

/// `LayoutStrip` manages a horizontal strip of `Panel`s, where each panel can contain a single window or a stack of windows.
/// It provides methods for manipulating the arrangement and access to windows within the pane.
#[derive(Clone, Component, Debug, Default)]
pub struct LayoutStrip {
    id: WorkspaceId,
    columns: VecDeque<Column>,
    column_states: VecDeque<ColumnState>,
    structure_revision: u64,
    viewport_width: Option<i32>,
    viewport_height: Option<i32>,
    inherited_width: WidthIntent,
}

impl LayoutStrip {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            columns: VecDeque::new(),
            ..Self::default()
        }
    }

    pub fn fullscreen(id: WorkspaceId, entity: Entity) -> Self {
        let mut columns = VecDeque::new();
        columns.push_back(Column::Fullscreen(entity));
        let mut strip = Self {
            id,
            columns,
            column_states: VecDeque::from([ColumnState::default()]),
            ..Self::default()
        };
        strip.sync_height_items();
        strip
    }

    pub fn column_states(&self) -> impl Iterator<Item = &ColumnState> {
        self.column_states.iter()
    }
    pub fn column_state(&self, index: usize) -> Option<&ColumnState> {
        self.column_states.get(index)
    }
    pub fn column_id(&self, entity: Entity) -> Option<ColumnId> {
        self.column_state(self.index_of(entity).ok()?)
            .map(|state| state.id)
    }
    pub fn structure_revision(&self) -> u64 {
        self.structure_revision
    }

    pub fn set_width_intent(&mut self, id: ColumnId, width: WidthIntent) -> Result<bool> {
        width.validate()?;
        let state = self
            .column_states
            .iter_mut()
            .find(|state| state.id == id)
            .ok_or_else(|| Error::NotFound("column identity no longer exists".into()))?;
        if state.width == width {
            // An explicit inherit/reset before first projection is also an
            // initialization choice, even when its value matches the default.
            state.width_initialized = true;
            return Ok(false);
        }
        let revision = state
            .intent_revision
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("column intent revision exhausted".into()))?;
        state.width = width;
        state.width_initialized = true;
        state.restore_width = None;
        state.intent_revision = revision;
        Ok(true)
    }

    pub fn set_column_rule(
        &mut self,
        id: ColumnId,
        source: Entity,
        width: Option<WidthIntent>,
        initial_width: i32,
    ) -> Result<bool> {
        let mut changed = self.set_column_default(id, width)?;
        if let Some(state) = self.column_states.iter_mut().find(|state| state.id == id) {
            if !state.width_initialized {
                if width.is_none()
                    && state.width == WidthIntent::InheritConfig
                    && state.intent_revision == 0
                    && initial_width > 0
                {
                    state.width = WidthIntent::Absolute(f64::from(initial_width));
                    changed = true;
                }
                state.width_initialized = true;
            }
            state.config_source = Some(source);
        }
        Ok(changed)
    }

    pub fn set_column_default(&mut self, id: ColumnId, width: Option<WidthIntent>) -> Result<bool> {
        if let Some(width) = width {
            width.validate()?;
        }
        let state = self
            .column_states
            .iter_mut()
            .find(|state| state.id == id)
            .ok_or_else(|| Error::NotFound("column identity no longer exists".into()))?;
        if state.configured_width == width {
            return Ok(false);
        }
        state.configured_width = width;
        Ok(true)
    }

    /// Replace only trusted current constraints; this never edits intent revision.
    #[allow(
        dead_code,
        reason = "trusted capability input seam, deliberately not inferred from failed writes"
    )]
    pub fn set_column_constraints(
        &mut self,
        id: ColumnId,
        constraints: Vec<WidthConstraint>,
    ) -> Result<bool> {
        let state = self
            .column_states
            .iter_mut()
            .find(|state| state.id == id)
            .ok_or_else(|| Error::NotFound("column identity no longer exists".into()))?;
        if state.constraints == constraints {
            return Ok(false);
        }
        state.constraints = constraints;
        Ok(true)
    }

    pub fn toggle_full_width(&mut self, id: ColumnId) -> Result<bool> {
        let state = self
            .column_states
            .iter()
            .find(|state| state.id == id)
            .ok_or_else(|| Error::NotFound("column identity no longer exists".into()))?;
        let old = state.width;
        let restored = state.restore_width;
        let next = restored.unwrap_or(WidthIntent::ViewportRatio(1.0));
        let changed = self.set_width_intent(id, next)?;
        if let Some(state) = self.column_states.iter_mut().find(|state| state.id == id) {
            state.restore_width = if restored.is_some() { None } else { Some(old) };
        }
        Ok(changed)
    }

    /// Context is derived from this Space's display, never a member's frame.
    pub fn set_width_context(&mut self, viewport: Option<i32>, inherited: WidthIntent) -> bool {
        if self.viewport_width == viewport && self.inherited_width == inherited {
            return false;
        }
        self.viewport_width = viewport;
        self.inherited_width = inherited;
        true
    }

    /// Read projection of original width. Unknown viewport never invents a ratio.
    pub fn width_ratio(&self, index: usize) -> Option<f64> {
        let state = self.column_state(index)?;
        let intent = match state.width {
            WidthIntent::InheritConfig => state.configured_width.unwrap_or(self.inherited_width),
            explicit => explicit,
        };
        match intent {
            WidthIntent::ViewportRatio(ratio) => Some(ratio),
            WidthIntent::Absolute(points) => {
                Some(points / f64::from(self.viewport_width.filter(|width| *width > 0)?))
            }
            WidthIntent::InheritConfig => None,
        }
    }

    pub fn effective_column_width(
        &self,
        index: usize,
    ) -> std::result::Result<EffectiveColumnWidth, WidthProjectionBlocked> {
        let state = self
            .column_states
            .get(index)
            .ok_or(WidthProjectionBlocked::InvalidIntent)?;
        let width = if matches!(self.columns[index], Column::Fullscreen(_)) {
            WidthIntent::ViewportRatio(1.0)
        } else {
            state.width
        };
        project_column_width(
            width,
            state.configured_width.unwrap_or(self.inherited_width),
            self.viewport_width,
            state.constraints.iter().copied(),
        )
    }

    /// Admission checks known coordinate overflow without conflating an
    /// unavailable viewport/constraint projection with an invalid state edit.
    /// Effect eligibility is stricter than admission: an unavailable or
    /// conflicting derivation must never execute a previous geometry target.
    pub fn projection_is_blocked(&self) -> bool {
        self.column_states.len() != self.columns.len()
            || self
                .columns()
                .enumerate()
                .try_fold(0_i32, |total, (index, _)| {
                    total.checked_add(self.effective_column_width(index).ok()?.slot)
                })
                .is_none()
    }

    pub fn width_budget_is_valid(&self) -> bool {
        let mut total = 0_i32;
        for index in 0..self.len() {
            match self.effective_column_width(index) {
                Ok(width) => {
                    let Some(next) = total.checked_add(width.slot) else {
                        return false;
                    };
                    total = next;
                }
                Err(
                    WidthProjectionBlocked::Unrepresentable | WidthProjectionBlocked::InvalidIntent,
                ) => return false,
                Err(_) => {}
            }
        }
        true
    }

    pub fn width_for_viewport(
        &self,
        index: usize,
        viewport: Option<i32>,
    ) -> std::result::Result<EffectiveColumnWidth, WidthProjectionBlocked> {
        let state = self
            .column_states
            .get(index)
            .ok_or(WidthProjectionBlocked::InvalidIntent)?;
        project_column_width(
            state.width,
            state.configured_width.unwrap_or(self.inherited_width),
            viewport,
            state.constraints.iter().copied(),
        )
    }

    fn structure_changed(&mut self) {
        self.sync_height_items();
        self.structure_revision = self
            .structure_revision
            .checked_add(1)
            .expect("layout structure revision exhausted");
    }

    /// Finds the index of a window within the pane.
    /// If the window is part of a stack, it returns the index of the panel containing the stack.
    ///
    /// # Arguments
    ///
    /// * `entity` - Entity of the window to find.
    ///
    /// # Returns
    ///
    /// `Ok(usize)` with the index if found, otherwise `Err(Error)`.
    pub fn index_of(&self, entity: Entity) -> Result<usize> {
        self.columns
            .iter()
            .position(|column| match column {
                Column::Single(id) | Column::Fullscreen(id) => *id == entity,
                Column::Stack(stack) => stack.iter().any(|item| item.contains(entity)),
                Column::Tabs(stack) => stack.contains(&entity),
            })
            .ok_or(Error::NotFound(format!(
                "{}: can not find window {entity} in the current pane.",
                function_name!()
            )))
    }

    /// Returns `true` if the strip contains the given entity.
    pub fn contains(&self, entity: Entity) -> bool {
        self.columns.iter().any(|column| match column {
            Column::Single(id) | Column::Fullscreen(id) => *id == entity,
            Column::Stack(stack) => stack.iter().any(|item| item.contains(entity)),
            Column::Tabs(stack) => stack.contains(&entity),
        })
    }

    /// Inserts a window ID into the pane at a specified position.
    /// The new window will be placed as a `Single` panel.
    ///
    /// # Arguments
    ///
    /// * `after` - The index at which to insert the window. If `after` is greater than or equal to the entity length,
    ///   the window is appended to the end.
    /// * `entity` - Entity of the window to insert.
    pub fn insert_at(&mut self, index: usize, entity: Entity) {
        self.column_states
            .insert(index.min(self.len()), ColumnState::default());
        if index >= self.len() {
            self.columns.push_back(Column::Single(entity));
        } else {
            self.columns.insert(index, Column::Single(entity));
        }
        self.structure_changed();
    }

    /// Appends a window ID as a `Single` panel to the end of the pane.
    ///
    /// # Arguments
    ///
    /// * `entity` - Entity of the window to append.
    pub fn append(&mut self, entity: Entity) {
        if self.contains(entity) {
            return;
        }
        self.columns.push_back(Column::Single(entity));
        self.column_states.push_back(ColumnState::default());
        self.structure_changed();
    }

    /// Moves the complete column containing `entity` to one side of the
    /// complete column containing `anchor`. Returns whether the order changed.
    pub fn move_column_relative(
        &mut self,
        entity: Entity,
        anchor: Entity,
        placement: Placement,
    ) -> bool {
        let (Ok(source), Ok(anchor)) = (self.index_of(entity), self.index_of(anchor)) else {
            return false;
        };
        if source == anchor {
            return false;
        }

        let Some(column) = self.columns.remove(source) else {
            return false;
        };
        let state = self.column_states.remove(source).expect("column metadata");
        let anchor = if source < anchor { anchor - 1 } else { anchor };
        let destination = match placement {
            Placement::Before => anchor,
            Placement::After => anchor + 1,
        };
        self.columns.insert(destination, column);
        self.column_states.insert(destination, state);
        if source != destination {
            self.structure_changed();
        }
        source != destination
    }

    pub(crate) fn column_containing(&self, entity: Entity) -> Option<Column> {
        self.index_of(entity)
            .ok()
            .and_then(|index| self.columns.get(index).cloned())
    }

    /// Appends an already grouped column, normalizing native fullscreen into a
    /// regular tiled column when it leaves its fullscreen Space.
    #[cfg(test)]
    pub(crate) fn append_column(&mut self, column: Column) {
        self.append_column_with_state(column, ColumnState::default());
    }

    pub(crate) fn append_column_with_state(&mut self, column: Column, state: ColumnState) {
        let column = match column {
            Column::Fullscreen(entity) => Column::Single(entity),
            other => other,
        };
        for entity in column.window_iter().collect::<Vec<_>>() {
            self.remove(entity);
        }
        self.columns.push_back(column);
        self.column_states.push_back(state);
        self.structure_changed();
    }

    pub(crate) fn append_strip(&mut self, other: &mut Self) {
        if other.columns.is_empty() {
            return;
        }
        self.columns.append(&mut other.columns);
        self.column_states.append(&mut other.column_states);
        self.structure_changed();
        other.structure_changed();
    }

    /// Removes `selected` windows and returns the same columns with stacks and
    /// tab groups preserved. A fullscreen column becomes a normal single
    /// column when it leaves the native fullscreen Space.
    pub(crate) fn take_windows_preserving_layout(
        &mut self,
        selected: &std::collections::HashSet<Entity>,
    ) -> Self {
        let mut extracted = Self {
            id: self.id,
            columns: self.columns.clone(),
            column_states: self.column_states.clone(),
            ..self.clone()
        };
        let split_item_ids = self
            .column_states
            .iter()
            .flat_map(|state| &state.height_items)
            .filter(|item| {
                item.members.iter().any(|member| selected.contains(member))
                    && item.members.iter().any(|member| !selected.contains(member))
            })
            .map(|item| item.id)
            .collect::<std::collections::HashSet<_>>();
        let split_ids = self
            .columns
            .iter()
            .zip(&self.column_states)
            .filter(|(column, _)| {
                let members = column.window_iter().collect::<Vec<_>>();
                members.iter().any(|e| selected.contains(e))
                    && members.iter().any(|e| !selected.contains(e))
            })
            .map(|(_, state)| state.id)
            .collect::<std::collections::HashSet<_>>();
        for entity in extracted.all_windows() {
            if !selected.contains(&entity) {
                extracted.remove(entity);
            }
        }
        for entity in selected {
            self.remove(*entity);
        }
        for state in self
            .column_states
            .iter_mut()
            .chain(extracted.column_states.iter_mut())
        {
            if split_ids.contains(&state.id) {
                *state = state.split();
            }
            for item in &mut state.height_items {
                if split_item_ids.contains(&item.id) {
                    *item = item.split();
                }
            }
        }
        for column in &mut extracted.columns {
            if let Column::Fullscreen(entity) = column {
                *column = Column::Single(*entity);
            }
        }
        extracted
    }

    pub fn append_tab_group(&mut self, entities: &[Entity]) {
        let group = dedup_entities(entities);
        if group.is_empty() {
            return;
        }

        // Re-grouping existing members keeps them at their lowest current index;
        // a foreign group lands at the end.
        let index = group
            .iter()
            .filter_map(|entity| self.index_of(*entity).ok())
            .min()
            .unwrap_or(self.len());

        self.insert_tab_group_at(index, &group);
    }

    /// Inserts `entities` as a single column at `index` (clamped to the column
    /// count), after removing any existing occurrences. A one-element group
    /// becomes a `Single` column, more than one a `Tabs` column.
    pub fn insert_tab_group_at(&mut self, index: usize, entities: &[Entity]) {
        let group = dedup_entities(entities);
        if group.is_empty() {
            return;
        }

        if self
            .columns
            .get(index)
            .is_some_and(|column| column.window_iter().eq(group.iter().copied()))
        {
            return;
        }
        let preserved = group.iter().find_map(|e| {
            self.index_of(*e)
                .ok()
                .and_then(|i| self.column_states.get(i).cloned())
        });
        for entity in &group {
            self.remove(*entity);
        }
        let preserved = preserved.map(|state| {
            if self
                .column_states
                .iter()
                .any(|existing| existing.id == state.id)
            {
                state.split()
            } else {
                state
            }
        });

        let index = index.min(self.len());
        if group.len() == 1 {
            self.insert_at(index, group[0]);
            if let Some(preserved) = preserved {
                self.column_states[index] = preserved;
            }
        } else if index >= self.len() {
            self.column_states
                .push_back(preserved.clone().unwrap_or_default());
            self.columns.push_back(Column::Tabs(group));
        } else {
            self.column_states
                .insert(index, preserved.clone().unwrap_or_default());
            self.columns.insert(index, Column::Tabs(group));
        }
        self.structure_changed();
    }

    /// Converts a column containing `leader` to a `Tabs` column and adds `follower`.
    #[cfg(test)]
    pub fn convert_to_tabs(&mut self, leader: Entity, follower: Entity) -> Result<()> {
        if self
            .index_of(leader)
            .ok()
            .zip(self.index_of(follower).ok())
            .is_some_and(|(left, right)| left == right)
        {
            return Ok(());
        }
        self.remove(follower);
        let index = self.index_of(leader)?;
        let column = self.columns.remove(index).unwrap();
        match column {
            Column::Single(id) | Column::Fullscreen(id) => {
                self.columns.insert(index, Column::Tabs(vec![follower, id]));
            }
            Column::Stack(mut items) => {
                if let Some(pos) = items.iter().position(|item| item.contains(leader)) {
                    match &mut items[pos] {
                        StackItem::Single(id) => {
                            let id = *id;
                            items[pos] = StackItem::Tabs(vec![follower, id]);
                        }
                        StackItem::Tabs(tabs) => {
                            if !tabs.contains(&follower) {
                                tabs.insert(0, follower);
                            }
                        }
                    }
                }
                self.columns.insert(index, Column::Stack(items));
            }
            Column::Tabs(mut tabs) => {
                if !tabs.contains(&follower) {
                    tabs.insert(0, follower);
                }
                self.columns.insert(index, Column::Tabs(tabs));
            }
        }
        self.structure_changed();
        Ok(())
    }

    /// Removes a window ID from the pane.
    /// If the window is part of a stack or tabs, it is removed from the vector.
    /// If the collection becomes empty or contains only one window, the panel type adjusts accordingly.
    ///
    /// # Arguments
    ///
    /// * `entity` - Entity of the window to remove.
    pub fn remove(&mut self, entity: Entity) {
        let removed = self
            .index_of(entity)
            .ok()
            .and_then(|index| self.columns.remove(index).zip(Some(index)));

        if let Some((column, index)) = removed {
            let count_after_removal = self.columns.len();
            match column {
                Column::Single(_) | Column::Fullscreen(_) => {
                    // Already removed from self.columns.
                }
                Column::Stack(mut stack) => {
                    for item in &mut stack {
                        match item {
                            StackItem::Single(_) => {}
                            StackItem::Tabs(tabs) => {
                                tabs.retain(|id| *id != entity);
                            }
                        }
                    }
                    stack.retain(|item| match item {
                        StackItem::Single(id) => *id != entity,
                        StackItem::Tabs(tabs) => !tabs.is_empty(),
                    });
                    if stack.len() > 1 {
                        self.columns.insert(index, Column::Stack(stack));
                    } else if let Some(remaining_item) = stack.first() {
                        match remaining_item {
                            StackItem::Single(id) => {
                                self.columns.insert(index, Column::Single(*id));
                            }
                            StackItem::Tabs(tabs) => {
                                self.columns.insert(index, Column::Tabs(tabs.clone()));
                            }
                        }
                    }
                }
                Column::Tabs(mut tabs) => {
                    tabs.retain(|id| *id != entity);
                    if tabs.len() > 1 {
                        self.columns.insert(index, Column::Tabs(tabs));
                    } else if let Some(remaining_id) = tabs.first() {
                        self.columns.insert(index, Column::Single(*remaining_id));
                    }
                }
            }
            if self.columns.len() == count_after_removal {
                self.column_states.remove(index);
            } else if self.column_states[index].config_source == Some(entity) {
                self.column_states[index].config_source = None;
                self.column_states[index].configured_width = None;
            }
            self.structure_changed();
        }
    }

    /// Retrieves the `Panel` at a specified index in the pane.
    ///
    /// # Arguments
    ///
    /// * `at` - The index from which to retrieve the panel.
    ///
    /// # Returns
    ///
    /// `Ok(Panel)` with the panel if the index is valid, otherwise `Err(Error)`.
    pub fn get(&self, at: usize) -> Result<Column> {
        self.columns
            .get(at)
            .cloned()
            .ok_or(Error::InvalidInput(format!(
                "{}: {at} out of bounds",
                function_name!()
            )))
    }

    /// Swaps the positions of two panels within the pane.
    ///
    /// # Arguments
    ///
    /// * `left` - The index of the first panel.
    /// * `right` - The index of the second panel.
    pub fn swap(&mut self, left: usize, right: usize) {
        if left == right {
            return;
        }
        self.columns.swap(left, right);
        self.column_states.swap(left, right);
        self.structure_changed();
    }

    /// Returns the number of panels in the pane.
    ///
    /// # Returns
    ///
    /// The number of panels as `usize`.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// Returns the first `Panel` in the pane.
    ///
    /// # Returns
    ///
    /// `Ok(Panel)` with the first panel, otherwise `Err(Error)` if the pane is empty.
    pub fn first(&self) -> Result<Column> {
        self.columns.front().cloned().ok_or(Error::NotFound(format!(
            "{}: can not find first element.",
            function_name!()
        )))
    }

    /// Returns the last `Panel` in the pane.
    ///
    /// # Returns
    ///
    /// `Ok(Panel)` with the last panel, otherwise `Err(Error)` if the pane is empty.
    pub fn last(&self) -> Result<Column> {
        self.columns.back().cloned().ok_or(Error::NotFound(format!(
            "{}: can not find last element.",
            function_name!()
        )))
    }

    pub fn right_neighbour(&self, entity: Entity) -> Option<Entity> {
        let index = self.index_of(entity).ok()?;
        let stack_pos = self.columns.get(index)?.position_of(entity)?;
        (index < self.columns.len())
            .then_some(index + 1)
            .and_then(|i| self.columns.get(i))
            .and_then(|col| col.at_or_last(stack_pos))
    }

    pub fn left_neighbour(&self, entity: Entity) -> Option<Entity> {
        let index = self.index_of(entity).ok()?;
        let stack_pos = self.columns.get(index)?.position_of(entity)?;
        (index > 0)
            .then(|| index - 1)
            .and_then(|i| self.columns.get(i))
            .and_then(|col| col.at_or_last(stack_pos))
    }

    /// Stacks the containing column onto the column to its left, keeping native
    /// tab entries intact. Leftmost and native fullscreen endpoints are unchanged.
    ///
    /// # Arguments
    ///
    /// * `entity` - Entity of the window to stack.
    ///
    /// # Returns
    ///
    /// `Ok(true)` when changed, `Ok(false)` for an ineligible endpoint, or an
    /// error when the window is not found. Rejected operations preserve the strip.
    pub fn stack(&mut self, entity: Entity) -> Result<bool> {
        let index = self.index_of(entity)?;
        if index == 0 {
            // Can not stack to the left if left most window already.
            return Ok(false);
        }
        if [index - 1, index]
            .into_iter()
            .any(|index| matches!(self.columns[index], Column::Fullscreen(_)))
        {
            return Ok(false);
        }

        let column_to_stack = self.columns.remove(index).unwrap();
        let source_state = self.column_states.remove(index).expect("column metadata");
        self.column_states[index - 1]
            .height_items
            .extend(source_state.height_items);
        let items_to_stack = match column_to_stack {
            Column::Fullscreen(_) => unreachable!("fullscreen endpoints rejected before removal"),
            Column::Single(id) => vec![StackItem::Single(id)],
            Column::Tabs(tabs) => vec![StackItem::Tabs(tabs)],
            Column::Stack(items) => items,
        };

        let target_column = self.columns.remove(index - 1).unwrap();
        let new_column = match target_column {
            Column::Fullscreen(_) => unreachable!("fullscreen endpoints rejected before removal"),
            Column::Single(id) => {
                Column::Stack([vec![StackItem::Single(id)], items_to_stack].concat())
            }
            Column::Tabs(tabs) => {
                Column::Stack([vec![StackItem::Tabs(tabs)], items_to_stack].concat())
            }
            Column::Stack(items) => Column::Stack([items, items_to_stack].concat()),
        };

        self.columns.insert(index - 1, new_column);
        self.structure_changed();
        Ok(true)
    }

    /// Moves one stack item onto a named column, keeping native tabs together.
    #[cfg(feature = "lua")]
    pub(crate) fn stack_onto(&mut self, entity: Entity, onto: Entity) -> Result<bool> {
        let source = self.index_of(entity)?;
        let target = self.index_of(onto)?;
        if source == target || matches!(self.columns[target], Column::Fullscreen(_)) {
            return Ok(false);
        }
        let Some(item) = self.columns[source].item_containing(entity) else {
            return Ok(false);
        };

        let moved_height = self.height_state(entity).cloned();
        for member in item.window_iter() {
            self.remove(member);
        }
        // The target is in another column and cannot have been removed above.
        let target = self.index_of(onto)?;
        if let Some(state) = moved_height {
            self.column_states[target].height_items.push(state);
        }
        match &mut self.columns[target] {
            Column::Single(id) => {
                self.columns[target] = Column::Stack(vec![StackItem::Single(*id), item]);
            }
            Column::Tabs(tabs) => {
                let tabs = std::mem::take(tabs);
                self.columns[target] = Column::Stack(vec![StackItem::Tabs(tabs), item]);
            }
            Column::Stack(items) => items.push(item),
            Column::Fullscreen(_) => unreachable!("validated before removing the source"),
        }
        self.structure_changed();
        Ok(true)
    }

    /// Exchanges named stack items without splitting native tab groups.
    #[cfg(feature = "lua")]
    pub(crate) fn swap_items(&mut self, first: Entity, second: Entity) -> Result<bool> {
        let left = self.index_of(first)?;
        let right = self.index_of(second)?;
        let (Some(first_item), Some(second_item)) = (
            self.columns[left].item_containing(first),
            self.columns[right].item_containing(second),
        ) else {
            return Ok(false);
        };
        let (Some(first_slot), Some(second_slot)) = (
            self.columns[left].position_of(first),
            self.columns[right].position_of(second),
        ) else {
            return Ok(false);
        };
        if left == right {
            if first_slot == second_slot {
                return Ok(false);
            }
            if let Column::Stack(items) = &mut self.columns[left] {
                items.swap(first_slot, second_slot);
            }
        } else {
            fn replace(column: &mut Column, slot: usize, item: StackItem) {
                if let Column::Stack(items) = column {
                    items[slot] = item;
                } else {
                    *column = match item {
                        StackItem::Single(entity) => Column::Single(entity),
                        StackItem::Tabs(tabs) => Column::Tabs(tabs),
                    };
                }
            }
            replace(&mut self.columns[left], first_slot, second_item);
            replace(&mut self.columns[right], second_slot, first_item);
        }
        self.structure_changed();
        Ok(true)
    }

    /// Unstacks the window with the given ID from its entity stack.
    /// If the window is in a single panel, no action is taken.
    ///
    /// # Arguments
    ///
    /// * `entity` - Entity of the window to unstack.
    ///
    /// # Returns
    ///
    /// `Ok(true)` when changed, `Ok(false)` when not stacked, or an error when
    /// the window is not found. All eligibility checks precede removal.
    pub fn unstack(&mut self, entity: Entity) -> Result<bool> {
        let index = self.index_of(entity)?;
        let Column::Stack(items) = &self.columns[index] else {
            return Ok(false);
        };
        let item_index = items
            .iter()
            .position(|item| item.contains(entity))
            .ok_or_else(|| Error::NotFound(format!("Entity {entity} not in stack")))?;
        let column = self.columns.remove(index).unwrap();
        let new_state = self.column_states[index].split();
        self.column_states.insert(index + 1, new_state);

        if let Column::Stack(mut items) = column {
            let removed_item = items.remove(item_index);

            // Re-insert the unstacked item as a single/tabs panel
            let unstacked_column = match removed_item {
                StackItem::Single(id) => Column::Single(id),
                StackItem::Tabs(tabs) => Column::Tabs(tabs),
            };
            self.columns.insert(index, unstacked_column);

            // Re-insert the modified stack (if not empty) at the original position
            if !items.is_empty() {
                let new_column = if items.len() == 1 {
                    match items.remove(0) {
                        StackItem::Single(id) => Column::Single(id),
                        StackItem::Tabs(tabs) => Column::Tabs(tabs),
                    }
                } else {
                    Column::Stack(items)
                };
                self.columns.insert(index, new_column);
            }
            self.structure_changed();
            Ok(true)
        } else {
            unreachable!("validated before removing the source")
        }
    }

    /// Returns a vector of all window IDs present in all panels within the pane, maintaining their order.
    /// For stacked panels, all windows in the stack are included.
    ///
    /// # Returns
    ///
    /// A `Vec<Entity>` containing all window IDs.
    pub fn all_windows(&self) -> Vec<Entity> {
        self.columns
            .iter()
            .flat_map(|column| match column {
                Column::Single(entity) | Column::Fullscreen(entity) => vec![*entity],
                Column::Stack(items) => items.iter().flat_map(StackItem::window_iter).collect(),
                Column::Tabs(ids) => ids.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn get_column_mut(&mut self, index: usize) -> Option<&mut Column> {
        self.columns.get_mut(index)
    }

    pub fn edit_column(&mut self, index: usize, edit: impl FnOnce(&mut Column)) -> bool {
        let Some(column) = self.columns.get_mut(index) else {
            return false;
        };
        let before = column.clone();
        edit(column);
        if *column == before {
            return false;
        }
        self.structure_changed();
        true
    }

    pub fn all_columns(&self) -> Vec<Entity> {
        self.columns.iter().filter_map(Column::top).collect()
    }

    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    pub fn columns(&self) -> impl Iterator<Item = &Column> {
        self.columns.iter()
    }

    #[instrument(level = Level::TRACE, skip_all, fields(layout_strip_height))]
    pub fn relative_positions<W>(
        &self,
        layout_strip_height: i32,
        get_window_frame: &W,
    ) -> impl Iterator<Item = (Entity, IRect)>
    where
        W: Fn(Entity) -> Option<IRect>,
    {
        // All eligible columns share one validated projection transaction. A
        // blocked stack must not publish only a subset of dependent frames.
        let eligible = |entity| {
            get_window_frame(entity)
                .and_then(checked_frame_size)
                .is_some()
        };
        let projection_ready = self.columns.iter().enumerate().all(|(index, _)| {
            self.effective_stack_heights_for(index, Some(layout_strip_height), &eligible)
                .is_ok()
        });
        self.column_positions(get_window_frame)
            .filter(move |_| projection_ready)
            .filter_map(move |(column, position)| {
                let index = self
                    .columns
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, column))?;
                let column_width = self.effective_column_width(index).ok()?.slot;
                let heights = self
                    .effective_stack_heights_for(index, Some(layout_strip_height), &eligible)
                    .ok()?;
                let items = self.column_height_items(index)?;
                let mut next_y = 0;
                let mut frames = Vec::new();
                for (id, height) in heights {
                    let item = items.iter().find(|item| item.id == id)?;
                    let frame = checked_window_frame(
                        Origin::new(position, next_y),
                        Size::new(column_width, height.slot),
                    )?;
                    next_y = frame.max.y;
                    frames.extend(
                        item.members
                            .iter()
                            .copied()
                            .filter(|entity| eligible(*entity))
                            .map(|entity| (entity, frame)),
                    );
                }
                Some(frames)
            })
            .flatten()
    }

    #[instrument(level = Level::TRACE, skip_all)]
    pub fn column_positions<W>(&self, get_window_frame: &W) -> impl Iterator<Item = (&Column, i32)>
    where
        W: Fn(Entity) -> Option<IRect>,
    {
        // A blocked participating width blocks the entire dependent projection.
        // Retained ordered-out identities keep intent but do not occupy a slot.

        let positions = self.columns().enumerate().try_fold(
            (Vec::new(), 0_i32),
            |(mut positions, left), (index, column)| {
                if !column
                    .window_iter()
                    .filter_map(get_window_frame)
                    .any(|frame| checked_frame_size(frame).is_some())
                {
                    return Some((positions, left));
                }
                let width = self.effective_column_width(index).ok()?.slot;
                let right = left.checked_add(width)?;
                positions.push((column, left));
                Some((positions, right))
            },
        );
        positions.into_iter().flat_map(|(positions, _)| positions)
    }

    pub fn tabbed(&self, entity: Entity) -> bool {
        self.index_of(entity)
            .and_then(|idx| self.get(idx))
            .map(|col| match col {
                Column::Tabs(tabs) => tabs.contains(&entity),
                Column::Stack(items) => items.iter().any(|item| {
                    if let StackItem::Tabs(tabs) = item {
                        tabs.contains(&entity)
                    } else {
                        false
                    }
                }),
                Column::Single(_) | Column::Fullscreen(_) => false,
            })
            .is_ok_and(|t| t)
    }

    pub fn tab_group(&self, entity: Entity) -> Option<Vec<Entity>> {
        self.tab_group_members(entity).map(<[Entity]>::to_vec)
    }

    /// Native tabs retain individual identities but only the selected member
    /// represents the physical window for ordinary geometry writes.
    pub(crate) fn is_inactive_tab(&self, entity: Entity) -> bool {
        self.tab_group_members(entity)
            .is_some_and(|tabs| tabs.first() != Some(&entity))
    }

    fn tab_group_members(&self, entity: Entity) -> Option<&[Entity]> {
        self.columns.iter().find_map(|column| match column {
            Column::Tabs(tabs) if tabs.contains(&entity) && tabs.len() > 1 => Some(tabs.as_slice()),
            Column::Stack(items) => items.iter().find_map(|item| match item {
                StackItem::Tabs(tabs) if tabs.contains(&entity) && tabs.len() > 1 => {
                    Some(tabs.as_slice())
                }
                StackItem::Single(_) | StackItem::Tabs(_) => None,
            }),
            Column::Single(_) | Column::Fullscreen(_) | Column::Tabs(_) => None,
        })
    }

    pub fn is_fullscreen(&self) -> bool {
        self.columns
            .front()
            .is_some_and(|column| matches!(column, Column::Fullscreen(_)))
    }
}

impl std::fmt::Display for LayoutStrip {
    /// Formats the `LayoutStrip` for display, showing the arrangement of its panels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let out = self
            .columns
            .iter()
            .map(|column| format!("{column:?}"))
            .collect::<Vec<_>>();
        write!(f, "[{}]", out.join(", "))
    }
}

/// Deduplicates `entities`, preserving first-seen order.
fn dedup_entities(entities: &[Entity]) -> Vec<Entity> {
    let mut seen = EntityHashSet::default();
    entities
        .iter()
        .copied()
        .filter(|entity| seen.insert(*entity))
        .collect()
}

/// Watches for size changes to windows and if they are changed, signals to the layout strip.
#[instrument(level = Level::DEBUG, skip_all)]
fn layout_sizes_changed(
    changed_sizes: ResizedWindows,
    workspaces: Query<&mut LayoutStrip, Without<PendingSpaceDestruction>>,
) {
    let changed_entities = changed_sizes.iter().collect::<EntityHashSet>();
    workspaces.into_iter().for_each(|mut strip| {
        if strip_has_changed_window(&strip, &changed_entities) {
            strip.set_changed();
        }
    });
}

fn strip_has_changed_window(strip: &LayoutStrip, changed_entities: &EntityHashSet) -> bool {
    strip
        .columns
        .iter()
        .any(|column| column_has_changed_window(column, changed_entities))
}

fn column_has_changed_window(column: &Column, changed_entities: &EntityHashSet) -> bool {
    match column {
        Column::Single(entity) | Column::Fullscreen(entity) => changed_entities.contains(entity),
        Column::Stack(stack) => stack
            .iter()
            .any(|item| stack_item_has_changed_window(item, changed_entities)),
        Column::Tabs(entities) => entities
            .iter()
            .any(|entity| changed_entities.contains(entity)),
    }
}

fn stack_item_has_changed_window(item: &StackItem, changed_entities: &EntityHashSet) -> bool {
    match item {
        StackItem::Single(entity) => changed_entities.contains(entity),
        StackItem::Tabs(entities) => entities
            .iter()
            .any(|entity| changed_entities.contains(entity)),
    }
}

/// Watches for changes to `LayoutStrip` (i.e. a window added or window order changed) and
/// re-calculates the logical positions of all the windows in the layout strip.
#[instrument(level = Level::DEBUG, skip_all)]
fn layout_strip_changed(
    changed_strips: ChangedLayoutStrips,
    mut windows: WindowFrames,
    displays: DisplayViewports,
    config: Res<Config>,
) {
    let get_window_frame = |entity| {
        windows
            .get(entity)
            .ok()
            .filter(|(_, _, _, unavailable)| {
                unavailable.is_none_or(|state| !state.excludes_from_layout_projection())
            })
            .and_then(|(position, bounds, _, _)| checked_window_frame(position.0, bounds.0))
    };

    let changed = changed_strips
        .into_iter()
        .filter_map(|(layout_strip, child_of)| {
            displays
                .get(child_of.parent())
                .map(|(display, dock)| {
                    let height = display.actual_display_bounds(dock, &config).height();
                    layout_strip.relative_positions(height, &get_window_frame)
                })
                .ok()
        })
        .flatten()
        .collect::<Vec<_>>();

    for (entity, frame) in changed {
        if let Ok((_, mut bounds, mut layout_position, _)) = windows.get_mut(entity) {
            if layout_position.0 != frame.min {
                layout_position.0 = frame.min;
            }
            if bounds.0 != frame.size() {
                bounds.0 = frame.size();
            }
        }
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
fn reshuffle_layout_strip(
    markers: ReshuffleMarkers,
    strips: StripPlacements,
    displays: DisplayViewports,
    windows: Windows,
    config: Res<Config>,
    mut commands: Commands,
) {
    markers.into_iter().for_each(|(entity, layout_position)| {
        if let Ok(mut cmd) = commands.get_entity(entity) {
            cmd.try_remove::<ReshuffleAroundMarker>();
        }
        let Some((strip, strip_entity, active_strip, child, active_marker)) =
            strips.into_iter().find(|strip| strip.0.contains(entity))
        else {
            return;
        };

        if active_marker.is_some_and(|m| m.is_added()) {
            trace!("reshuffle_layout_strip: skipping newly active workspace {strip_entity}");
            return;
        }
        let Ok((active_display, dock)) = displays.get(child.parent()) else {
            return;
        };
        let display_bounds = active_display.actual_display_bounds(dock, &config);
        if checked_frame_size(display_bounds).is_none() {
            return;
        }
        let Some(mut frame) = windows.moving_frame(entity) else {
            return;
        };

        let size = frame.size();
        let visible_width = display_bounds.intersect(frame).width();

        // Expose the window by clamping it into the viewport.
        frame.min = clamp_origin_to_viewport(frame.min, size, display_bounds);
        frame.max = frame.min + size;

        let mut strip_x = i64::from(frame.min.x) - i64::from(layout_position.0.x);

        // Enforce the edge invariant when auto-center is off: the leftmost
        // window must touch the left edge and the rightmost the right edge
        // if more than 1 windows in workspace.
        if !config.auto_center()
            && !config.continuous_swipe()
            && let Some((last_x, last_width)) = strip
                .last()
                .ok()
                .and_then(|column| column.top())
                .and_then(|last| {
                    windows
                        .layout_position(last)
                        .map(|position| position.0.x)
                        .zip(windows.moving_frame(last).map(|frame| frame.width()))
                })
        {
            let Some(total_strip_width) = last_x.checked_add(last_width).filter(|width| *width > 0)
            else {
                return;
            };
            strip_x = if display_bounds.width() < total_strip_width {
                strip_x.clamp(
                    i64::from(display_bounds.max.x) - i64::from(total_strip_width),
                    i64::from(display_bounds.min.x),
                )
            } else {
                // Strip fits entirely: pin the leftmost window to the left edge.
                i64::from(display_bounds.min.x)
            };
        }
        let Ok(strip_x) = i32::try_from(strip_x) else {
            return;
        };
        let strip_position = Origin::new(strip_x, display_bounds.min.y);

        // Check how much of the window is hidden. Slivers don't count as
        // meaningfully visible, so subtract sliver_width from the visible
        // portion. If the hidden fraction is within the allowed ratio, skip.
        let hidden_ratio = config.window_hidden_ratio();
        if hidden_ratio > 0.0 {
            let meaningful = (visible_width - config.sliver_width()).max(0);
            let visible_fraction = f64::from(meaningful) / f64::from(frame.width().max(1));
            let hidden_fraction = 1.0 - visible_fraction;

            // Do not move the window if the hidden fraction is lower than threshold
            // or if the layout strip movement is shorter than the hidden width.
            let strip_movement = active_strip.x.abs_diff(strip_position.x);
            if hidden_fraction <= hidden_ratio
                && (frame.width() - visible_width).unsigned_abs() >= strip_movement
            {
                return;
            }
        }

        trace!("reshuffle_layout_strip: triggered for entity {entity}, offset {strip_position}");
        commands.reposition_entity(strip_entity, strip_position);
    });
}

/// Scrolls the strip the minimum amount needed to keep `EnsureVisibleMarker`
/// entities on-screen at their new layout position. If the entity already fits
/// inside the viewport with the strip where it is, the strip is left alone and
/// the per-window animator slides the entity into its slot. Only when the new
/// slot would fall past an edge does the strip translate, and only by the
/// shortfall — never to anchor the entity to a particular position.
#[instrument(level = Level::DEBUG, skip_all)]
fn ensure_visible_in_strip(
    markers: EnsureVisibleMarkers,
    strips: StripPlacements,
    displays: DisplayViewports,
    windows: Windows,
    config: Res<Config>,
    mut commands: Commands,
) {
    for (entity, layout_position) in markers {
        if let Ok(mut cmd) = commands.get_entity(entity) {
            cmd.try_remove::<EnsureVisibleMarker>();
        }
        let Some((_, strip_entity, strip_position, child, active_marker)) =
            strips.into_iter().find(|s| s.0.contains(entity))
        else {
            continue;
        };

        if active_marker.is_some_and(|m| m.is_added()) {
            trace!("ensure_visible_in_strip: skipping newly active workspace {strip_entity}");
            continue;
        }
        let Ok((display, dock)) = displays.get(child.parent()) else {
            continue;
        };
        let Some(size) = windows.size(entity) else {
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);
        if checked_frame_size(viewport).is_none() || size.x <= 0 || size.y <= 0 {
            continue;
        }

        // Where the entity would appear if the strip stays put.
        let candidate_x = i64::from(layout_position.0.x) + i64::from(strip_position.0.x);
        // Clamp into the viewport. If already on-screen, this is a no-op and
        // the strip target equals its current position — no movement.
        let clamped_x = clamp_axis_origin(candidate_x, size.x, viewport.min.x, viewport.max.x);
        let Ok(strip_x) = i32::try_from(i64::from(clamped_x) - i64::from(layout_position.0.x))
        else {
            continue;
        };
        if strip_x == strip_position.0.x {
            continue;
        }
        let strip_target = Origin::new(strip_x, strip_position.0.y);
        trace!("ensure_visible_in_strip: entity {entity}, scroll strip to {strip_target}");
        commands.reposition_entity(strip_entity, strip_target);
    }
}

/// Reacts to changes in the position of the `LayoutStrip` to Display, and if changed,
/// marks all the windows in the strip as requiring re-positioning.
#[instrument(level = Level::DEBUG, skip_all)]
fn position_layout_strips(
    moved_strips: Populated<&LayoutStrip, (Changed<Position>, Without<PendingSpaceDestruction>)>,
    mut windows: StableLayoutPositions,
) {
    for strip in moved_strips {
        for entity in strip.all_windows() {
            if let Ok(mut position) = windows.get_mut(entity) {
                position.set_changed();
            }
        }
    }
}

#[derive(Clone, Copy)]
struct StripWindowContext {
    strip_position: Origin,
    swiping: bool,
    display_entity: Entity,
    stacked: bool,
}

fn projected_window_frame(
    layout_origin: Origin,
    size: Size,
    context: StripWindowContext,
    viewport: IRect,
    horizontal_padding: i32,
    config: &Config,
) -> Option<IRect> {
    let viewport_size = checked_frame_size(viewport)?;
    if size.x <= 0 || size.y <= 0 {
        return None;
    }
    let (_, pad_right, _, pad_left) = config.edge_padding();
    let sliver_width = i64::from(config.sliver_width());
    let h_pad = i64::from(horizontal_padding);
    let width = i64::from(size.x);
    let mut x = i64::from(layout_origin.x) + i64::from(context.strip_position.x);
    let mut y = i64::from(layout_origin.y) + i64::from(context.strip_position.y);
    // Off-screen logical offsets can exceed i32 while their visible sliver is
    // representable. Apply the projection before narrowing the coordinates.
    let mut offscreen = false;
    if x + width <= i64::from(viewport.min.x) + h_pad {
        x = i64::from(viewport.min.x) - width + sliver_width - i64::from(pad_left) + h_pad;
        offscreen = true;
    } else if x >= i64::from(viewport.max.x) - h_pad {
        x = i64::from(viewport.max.x) - sliver_width + i64::from(pad_right) - h_pad;
        offscreen = true;
    }
    // Keep stacked proportions and full-height swipe behavior unchanged.
    if !context.swiping && offscreen && !context.stacked {
        let inset = round_px(f64::from(viewport_size.y) * (1.0 - config.sliver_height()) / 2.0);
        y += i64::from(inset);
    }
    checked_window_frame(
        Origin::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?),
        size,
    )
}

fn insert_strip_window_contexts(
    contexts: &mut EntityHashMap<StripWindowContext>,
    strip: &LayoutStrip,
    strip_position: Origin,
    swiping: bool,
    display_entity: Entity,
) {
    for column in &strip.columns {
        insert_column_window_contexts(
            contexts,
            column,
            strip_position,
            swiping,
            display_entity,
            matches!(column, Column::Stack(_)),
        );
    }
}

fn insert_column_window_contexts(
    contexts: &mut EntityHashMap<StripWindowContext>,
    column: &Column,
    strip_position: Origin,
    swiping: bool,
    display_entity: Entity,
    stacked: bool,
) {
    match column {
        Column::Single(entity) | Column::Fullscreen(entity) => {
            contexts.insert(
                *entity,
                StripWindowContext {
                    strip_position,
                    swiping,
                    display_entity,
                    stacked,
                },
            );
        }
        Column::Stack(items) => {
            for item in items {
                insert_stack_item_window_contexts(
                    contexts,
                    item,
                    strip_position,
                    swiping,
                    display_entity,
                    stacked,
                );
            }
        }
        Column::Tabs(entities) => {
            for entity in entities {
                contexts.insert(
                    *entity,
                    StripWindowContext {
                        strip_position,
                        swiping,
                        display_entity,
                        stacked,
                    },
                );
            }
        }
    }
}

fn insert_stack_item_window_contexts(
    contexts: &mut EntityHashMap<StripWindowContext>,
    item: &StackItem,
    strip_position: Origin,
    swiping: bool,
    display_entity: Entity,
    stacked: bool,
) {
    match item {
        StackItem::Single(entity) => {
            contexts.insert(
                *entity,
                StripWindowContext {
                    strip_position,
                    swiping,
                    display_entity,
                    stacked,
                },
            );
        }
        StackItem::Tabs(entities) => {
            for entity in entities {
                contexts.insert(
                    *entity,
                    StripWindowContext {
                        strip_position,
                        swiping,
                        display_entity,
                        stacked,
                    },
                );
            }
        }
    }
}

/// Reacts to changes of logical window layout in the strip and any have been changed, reposition
/// the layout strip against the current display viewport.
#[instrument(level = Level::DEBUG, skip_all)]
fn position_layout_windows(
    positioned_windows: RepositionedWindows,
    workspaces: StableWorkspacePlacements,
    displays: DisplayViewports,
    config: Res<Config>,
    mut commands: Commands,
) {
    let mut strip_contexts = EntityHashMap::default();
    for (layout_strip, Position(strip_position), swiping, child_of) in &workspaces {
        insert_strip_window_contexts(
            &mut strip_contexts,
            layout_strip,
            *strip_position,
            swiping,
            child_of.parent(),
        );
    }

    for (
        entity,
        window,
        layout_position,
        mut position,
        bounds,
        mut desired,
        mut presented,
        parked,
    ) in positioned_windows
    {
        let Some(context) = strip_contexts.get(&entity) else {
            continue;
        };
        let Ok((display, dock)) = displays.get(context.display_entity) else {
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);
        let Some(viewport_size) = checked_frame_size(viewport) else {
            continue;
        };
        // Gets 80% of the display height as threshold.
        let Ok(vertical_move_threshold) = u32::try_from(i64::from(viewport_size.y) * 8 / 10) else {
            continue;
        };
        // Keep logical positions untouched: parking only replaces the physical
        // projection, using the same sliver geometry as strip overflow.
        let mut projection = *context;
        let mut layout_origin = layout_position.0;
        if let Some(parked) = parked {
            projection.strip_position.x = 0;
            projection.swiping = false;
            let Some(x) = (match parked.side {
                super::tiled_visibility::ParkingSide::Left => {
                    viewport.min.x.checked_sub(bounds.0.x)
                }
                super::tiled_visibility::ParkingSide::Right => Some(viewport.max.x),
            }) else {
                continue;
            };
            layout_origin.x = x;
        }
        let Some(frame) = projected_window_frame(
            layout_origin,
            bounds.0,
            projection,
            viewport,
            window.horizontal_padding(),
            &config,
        ) else {
            continue;
        };

        // Position remains a compatibility projection for the existing strip
        // math. It is the final target, never the animated/presented origin.
        let offscreen_move = position.0.y.abs_diff(frame.min.y) > vertical_move_threshold;
        if position.0 != frame.min {
            position.0 = frame.min;
        }
        super::window_frame::set_desired_frame(
            entity,
            frame,
            context.swiping || offscreen_move,
            desired.as_deref_mut(),
            presented.as_deref_mut(),
            &mut commands,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;

    impl LayoutStrip {
        /// Historical layout fixtures supplied widths as frames. Translate
        /// those inputs to explicit intent so these tests continue testing
        /// positioning/height behavior without retaining a production writer.
        fn fixture_relative_positions<W>(
            &self,
            height: i32,
            frame: &W,
        ) -> std::vec::IntoIter<(Entity, IRect)>
        where
            W: Fn(Entity) -> Option<IRect>,
        {
            let mut strip = self.clone();
            while strip.column_states.len() < strip.columns.len() {
                strip.column_states.push_back(ColumnState::default());
            }
            for (column, state) in strip.columns.iter().zip(&mut strip.column_states) {
                if let Some(size) = column
                    .window_iter()
                    .filter_map(frame)
                    .find_map(checked_frame_size)
                {
                    state.width = WidthIntent::Absolute(f64::from(size.x));
                } else {
                    state.width = WidthIntent::Absolute(1.0);
                }
            }
            strip.sync_height_items();
            strip
                .relative_positions(height, frame)
                .collect::<Vec<_>>()
                .into_iter()
        }
    }

    #[test]
    fn no_op_layout_edits_do_not_invalidate_structural_revision() {
        let (_, mut strip, entities) = setup_world_and_strip();
        let revision = strip.structure_revision();
        strip.swap(0, 0);
        strip.remove(Entity::PLACEHOLDER);
        strip.move_column_relative(entities[0], entities[1], Placement::Before);
        strip.append_tab_group(&[entities[0]]);
        strip.edit_column(0, |column| column.move_to_front(entities[0]));
        strip.append_strip(&mut LayoutStrip::default());
        assert_eq!(strip.structure_revision(), revision);
    }

    #[test]
    fn regrouping_part_of_a_column_creates_an_independent_identity() {
        let (_, mut strip, entities) = setup_world_and_strip();
        strip.stack(entities[1]).unwrap();
        let original = strip.column_id(entities[0]).unwrap();
        strip
            .set_width_intent(original, WidthIntent::Absolute(800.0))
            .unwrap();
        strip.insert_tab_group_at(1, &[entities[1]]);
        assert_eq!(strip.column_id(entities[0]), Some(original));
        assert_ne!(strip.column_id(entities[1]), Some(original));
        assert_eq!(strip.column_states.len(), strip.len());
        assert_eq!(
            strip.column_state(1).unwrap().width,
            WidthIntent::Absolute(800.0)
        );
    }

    #[test]
    fn column_identity_and_intent_survive_reordering_and_membership_changes() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let mut strip = LayoutStrip::default();
        strip.append(a);
        strip.append(b);
        strip.append(c);
        let id = strip.column_id(a).unwrap();
        strip
            .set_width_intent(id, WidthIntent::Absolute(800.0))
            .unwrap();
        strip.swap(0, 2);
        assert_eq!(strip.column_id(a), Some(id));
        let revision = strip.column_state(2).unwrap().intent_revision;
        assert!(
            !strip
                .set_width_intent(id, WidthIntent::Absolute(800.0))
                .unwrap()
        );
        assert_eq!(strip.column_state(2).unwrap().intent_revision, revision);
        strip.convert_to_tabs(a, b).unwrap();
        assert_eq!(strip.column_id(b), Some(id));
        strip.remove(a);
        assert_eq!(strip.column_id(b), Some(id));
        assert_eq!(
            strip
                .column_state(strip.index_of(b).unwrap())
                .unwrap()
                .width,
            WidthIntent::Absolute(800.0)
        );
    }

    #[test]
    fn split_columns_copy_raw_width_without_sharing_identity_or_constraints() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let mut strip = LayoutStrip::default();
        strip.append(a);
        strip.append(b);
        strip.stack(b).unwrap();
        let id = strip.column_id(a).unwrap();
        strip
            .set_width_intent(id, WidthIntent::Absolute(800.0))
            .unwrap();
        strip
            .set_column_constraints(
                id,
                vec![WidthConstraint::Interval {
                    min: 1000.0,
                    max: 1200.0,
                }],
            )
            .unwrap();
        let extracted = strip.take_windows_preserving_layout(&std::collections::HashSet::from([b]));
        assert_ne!(strip.column_id(a), Some(id));
        assert_ne!(extracted.column_id(b), Some(id));
        assert_ne!(strip.column_id(a), extracted.column_id(b));
        assert_eq!(
            strip.column_state(0).unwrap().width,
            WidthIntent::Absolute(800.0)
        );
        assert_eq!(
            extracted.column_state(0).unwrap().width,
            WidthIntent::Absolute(800.0)
        );
        assert_eq!(extracted.effective_column_width(0).unwrap().slot, 800);
        assert!(
            strip
                .set_width_intent(id, WidthIntent::Absolute(900.0))
                .is_err()
        );
    }

    #[test]
    fn layout_uses_one_effective_width_and_never_derives_intent_from_frames() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let mut strip = LayoutStrip::default();
        strip.append(a);
        strip.append(b);
        strip.set_width_context(Some(1600), WidthIntent::Absolute(800.0));
        let id = strip.column_id(a).unwrap();
        strip
            .set_column_constraints(
                id,
                vec![WidthConstraint::Interval {
                    min: 1000.0,
                    max: 1200.0,
                }],
            )
            .unwrap();
        let output = strip
            .relative_positions(600, &|_| Some(IRect::new(0, 0, 350, 400)))
            .collect::<Vec<_>>();
        assert_eq!(output[0].1.width(), 1000);
        assert_eq!(output[1].1.min.x, 1000);
        assert_eq!(output[1].1.width(), 800);
        assert_eq!(
            strip.column_state(0).unwrap().width,
            WidthIntent::InheritConfig
        );
        assert_eq!(strip.column_state(0).unwrap().intent_revision, 0);
        strip
            .set_column_constraints(id, vec![WidthConstraint::Unsupported])
            .unwrap();
        assert_eq!(
            strip
                .relative_positions(600, &|_| Some(IRect::new(0, 0, 350, 400)))
                .count(),
            0
        );
    }

    #[test]
    fn width_edit_without_viewport_is_accepted_and_full_width_restores_original_mode() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let mut strip = LayoutStrip::default();
        strip.append(a);
        let id = strip.column_id(a).unwrap();
        strip
            .set_width_intent(id, WidthIntent::ViewportRatio(0.4))
            .unwrap();
        assert_eq!(
            strip.effective_column_width(0),
            Err(WidthProjectionBlocked::UnknownViewport)
        );
        strip.toggle_full_width(id).unwrap();
        strip.toggle_full_width(id).unwrap();
        assert_eq!(
            strip.column_state(0).unwrap().width,
            WidthIntent::ViewportRatio(0.4)
        );
        strip.set_width_context(Some(2000), WidthIntent::Absolute(900.0));
        assert_eq!(strip.effective_column_width(0).unwrap().slot, 800);
    }

    #[test]
    fn projection_filters_invalid_frames_without_changing_membership() {
        let (_, mut strip, entities) = setup_world_and_strip();
        strip.stack(entities[1]).unwrap();
        for invalid in [
            IRect::new(0, 0, 0, 400),
            IRect {
                min: Origin::new(100, 0),
                max: Origin::new(0, 400),
            },
            IRect::new(i32::MIN, 0, i32::MAX, 400),
        ] {
            let frame = |entity| {
                Some(if entity == entities[0] {
                    invalid
                } else {
                    IRect::new(0, 0, 400, 300)
                })
            };
            let projected = strip
                .fixture_relative_positions(600, &frame)
                .collect::<Vec<_>>();
            assert_eq!(
                projected,
                vec![
                    (entities[1], IRect::new(0, 0, 400, 600)),
                    (entities[2], IRect::new(400, 0, 800, 600)),
                ]
            );
            assert_eq!(strip.all_windows(), entities);
            for height in [0, -1, i32::MIN] {
                assert_eq!(strip.fixture_relative_positions(height, &frame).count(), 0);
            }
        }
    }

    #[test]
    fn projected_frames_preserve_padding_swipe_and_stacked_height_rules() {
        let config: Config = (
            crate::config::MainOptions {
                padding_left: Some(3),
                padding_right: Some(7),
                sliver_width: Some(16),
                sliver_height: Some(0.5),
                ..Default::default()
            },
            vec![],
        )
            .into();
        let mut world = World::new();
        let display_entity = world.spawn_empty().id();
        for swiping in [false, true] {
            for stacked in [false, true] {
                let context = StripWindowContext {
                    strip_position: Origin::new(-1024, 20),
                    swiping,
                    display_entity,
                    stacked,
                };
                for (x, expected_x) in [
                    (-1000, -1403),
                    (i32::MIN, -1403),
                    (2000, -17),
                    (i32::MAX, -17),
                ] {
                    let frame = projected_window_frame(
                        Origin::new(x, 0),
                        Size::new(400, 748),
                        context,
                        IRect::new(-1024, 20, 0, 768),
                        8,
                        &config,
                    )
                    .unwrap();
                    assert_eq!(
                        frame.min,
                        Origin::new(expected_x, if swiping || stacked { 20 } else { 207 })
                    );
                    assert_eq!(frame.size(), Size::new(400, 748));
                }
            }
        }
    }

    #[test]
    fn projected_frames_reject_unrepresentable_final_endpoints() {
        let mut world = World::new();
        let context = StripWindowContext {
            strip_position: Origin::ZERO,
            swiping: true,
            display_entity: world.spawn_empty().id(),
            stacked: false,
        };
        let config = Config::default();
        for (origin, size, viewport) in [
            (
                Origin::new(0, i32::MAX - 100),
                Size::new(400, 200),
                IRect::new(0, 20, 1024, 768),
            ),
            (
                Origin::new(i32::MAX, 20),
                Size::new(400, 200),
                IRect::new(i32::MAX - 1024, 20, i32::MAX, 768),
            ),
            (
                Origin::ZERO,
                Size::new(0, 200),
                IRect::new(0, 20, 1024, 768),
            ),
        ] {
            assert!(projected_window_frame(origin, size, context, viewport, 0, &config).is_none());
        }
        assert_eq!(
            projected_window_frame(
                Origin::ZERO,
                Size::new(i32::MAX, 200),
                context,
                IRect::new(0, 20, 1024, 768),
                0,
                &config
            ),
            Some(IRect::new(0, 0, i32::MAX, 200))
        );
    }

    #[test]
    fn ensure_visible_continues_after_noop_invalid_or_incomplete_requests() {
        use crate::tests::{TestHarness, find_window_entity};
        for first_x in [Some(0), Some(i32::MIN), None] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(5);
            let first = find_window_entity(0, harness.world());
            let second = find_window_entity(1, harness.world());
            let world = harness.world();
            let system = world.register_system(ensure_visible_in_strip);
            world.run_system(system).unwrap();
            world.entity_mut(first).insert((
                EnsureVisibleMarker,
                LayoutPosition(Origin::new(first_x.unwrap_or(0), 0)),
            ));
            if first_x.is_none() {
                world.entity_mut(first).remove::<Bounds>();
            }
            world
                .entity_mut(second)
                .insert((EnsureVisibleMarker, LayoutPosition(Origin::new(2000, 0))));
            let strip_entity = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .single(world)
                .unwrap();
            world.run_system(system).unwrap();
            assert_eq!(
                world.get::<RepositionMarker>(strip_entity).unwrap().0.x,
                -1376
            );
            assert!(world.get::<EnsureVisibleMarker>(first).is_none());
            assert!(world.get::<EnsureVisibleMarker>(second).is_none());
        }
    }

    #[test]
    fn ensure_visible_handles_a_wide_global_candidate_with_a_representable_target() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        let world = harness.world();
        let system = world.register_system(ensure_visible_in_strip);
        world.run_system(system).unwrap();
        let strip_entity = world
            .query_filtered::<Entity, With<LayoutStrip>>()
            .single(world)
            .unwrap();
        world
            .entity_mut(strip_entity)
            .insert(Position(Origin::new(1000, 20)));
        world.entity_mut(entity).insert((
            EnsureVisibleMarker,
            LayoutPosition(Origin::new(i32::MAX - 400, 0)),
        ));
        world.run_system(system).unwrap();
        assert_eq!(
            world.get::<RepositionMarker>(strip_entity).unwrap().0,
            Origin::new(1024 - i32::MAX, 20)
        );
    }

    #[test]
    fn reshuffle_handles_discarded_y_translation_and_full_range_movement() {
        use crate::tests::{TestHarness, find_window_entity};
        for (logical_y, strip_x) in [(i32::MIN, 0), (0, i32::MIN)] {
            let config = (
                crate::config::MainOptions {
                    window_hidden_ratio: Some(0.5),
                    ..Default::default()
                },
                vec![],
            )
                .into();
            let mut harness = TestHarness::new().with_config(config).with_windows(1);
            harness.pump_frames(5);
            let entity = find_window_entity(0, harness.world());
            let world = harness.world();
            let system = world.register_system(reshuffle_layout_strip);
            world.run_system(system).unwrap();
            let strip_entity = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .single(world)
                .unwrap();
            world
                .entity_mut(strip_entity)
                .insert(Position(Origin::new(strip_x, 20)));
            world.entity_mut(entity).insert((
                ReshuffleAroundMarker,
                LayoutPosition(Origin::new(0, logical_y)),
            ));
            world.run_system(system).unwrap();
            assert!(world.get::<ReshuffleAroundMarker>(entity).is_none());
            if strip_x == i32::MIN {
                assert_eq!(
                    world.get::<RepositionMarker>(strip_entity).unwrap().0,
                    Origin::new(0, 20)
                );
            }
        }
    }

    #[test]
    fn window_projection_clips_before_narrowing_global_coordinates() {
        use crate::tests::{TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let entity = find_window_entity(0, harness.world());
        let world = harness.world();
        let strip_entity = world
            .query_filtered::<Entity, With<LayoutStrip>>()
            .single(world)
            .unwrap();
        world
            .entity_mut(strip_entity)
            .insert(Position(Origin::new(1000, 20)));
        world
            .entity_mut(entity)
            .insert(LayoutPosition(Origin::new(i32::MAX - 400, 0)));
        let config = world.resource::<Config>();
        let expected_x = 1024 - config.sliver_width() + config.edge_padding().1
            - world.get::<Window>(entity).unwrap().horizontal_padding();
        world.run_system_once(position_layout_windows).unwrap();
        let frame = world.get::<DesiredWindowFrame>(entity).unwrap().0;
        assert_eq!(frame.min.x, expected_x);
        assert_eq!(frame.width(), crate::tests::TEST_WINDOW_WIDTH);
        assert!(checked_frame_size(frame).is_some());
    }

    #[test]
    fn viewport_rebase_accepts_a_wide_translation_when_both_results_fit() {
        use crate::tests::TestHarness;
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let world = harness.world();
        let strip_entity = world
            .query_filtered::<Entity, With<LayoutStrip>>()
            .single(world)
            .unwrap();
        world.entity_mut(strip_entity).insert((
            LayoutViewport(IRect::new(i32::MIN, 20, i32::MIN + 1024, 768)),
            Position(Origin::new(i32::MIN + 100, 20)),
            RepositionMarker(Origin::new(i32::MIN + 200, 20)),
        ));
        world.run_system_once(display_viewport_changed).unwrap();
        assert_eq!(
            world.get::<Position>(strip_entity).unwrap().0,
            Origin::new(100, 20)
        );
        assert_eq!(
            world.get::<RepositionMarker>(strip_entity).unwrap().0,
            Origin::new(200, 20)
        );
    }

    #[test]
    fn viewport_rebase_rejects_an_unrepresentable_pending_target_atomically() {
        use crate::tests::TestHarness;
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(5);
        let world = harness.world();
        let strip_entity = world
            .query_filtered::<Entity, With<LayoutStrip>>()
            .single(world)
            .unwrap();
        let previous = IRect::new(-100, 20, 924, 768);
        world.entity_mut(strip_entity).insert((
            LayoutViewport(previous),
            Position(Origin::new(0, 20)),
            RepositionMarker(Origin::new(i32::MAX, 20)),
        ));
        world.run_system_once(display_viewport_changed).unwrap();
        assert_eq!(
            world.get::<Position>(strip_entity).unwrap().0,
            Origin::new(0, 20)
        );
        assert_eq!(
            world.get::<RepositionMarker>(strip_entity).unwrap().0,
            Origin::new(i32::MAX, 20)
        );
        assert_eq!(
            world.get::<LayoutViewport>(strip_entity).unwrap().0,
            previous
        );
        world
            .entity_mut(strip_entity)
            .insert(RepositionMarker(Origin::new(100, 20)));
        world.run_system_once(display_viewport_changed).unwrap();
        assert_eq!(
            world.get::<Position>(strip_entity).unwrap().0,
            Origin::new(100, 20)
        );
        assert_eq!(
            world.get::<RepositionMarker>(strip_entity).unwrap().0,
            Origin::new(200, 20)
        );
        assert_eq!(
            world.get::<LayoutViewport>(strip_entity).unwrap().0,
            IRect::new(0, 20, 1024, 768)
        );
    }

    #[test]
    fn projection_preserves_available_stack_items_when_any_sibling_is_missing() {
        for missing in 0..3 {
            let (mut world, mut strip, entities) = setup_world_and_strip();
            strip.stack(entities[1]).unwrap();
            strip.stack(entities[2]).unwrap();
            let last = world.spawn_empty().id();
            strip.append(last);
            let before = strip.all_windows();
            let frame = |entity| (entity != entities[missing]).then(|| IRect::new(0, 0, 400, 300));
            let projected = strip
                .fixture_relative_positions(600, &frame)
                .collect::<Vec<_>>();
            let expected = before
                .iter()
                .copied()
                .filter(|entity| *entity != entities[missing])
                .collect::<Vec<_>>();
            assert_eq!(
                projected
                    .iter()
                    .map(|(entity, _)| *entity)
                    .collect::<Vec<_>>(),
                expected,
                "missing={missing}"
            );
            assert_eq!(projected[0].1, IRect::new(0, 0, 400, 300));
            assert_eq!(projected[1].1, IRect::new(0, 300, 400, 600));
            assert_eq!(projected[2].1, IRect::new(400, 0, 800, 600));
            assert_eq!(strip.all_windows(), before);
            let restored = strip
                .fixture_relative_positions(600, &|_| Some(IRect::new(0, 0, 400, 200)))
                .collect::<Vec<_>>();
            assert_eq!(
                restored
                    .iter()
                    .map(|(entity, _)| *entity)
                    .collect::<Vec<_>>(),
                before
            );
        }
    }

    #[test]
    fn projection_keeps_available_native_tabs_without_publishing_missing_members() {
        for missing in 0..2 {
            let (_, mut strip, entities) = setup_world_and_strip();
            strip.columns = VecDeque::from([Column::Stack(vec![
                StackItem::Tabs(vec![entities[0], entities[1]]),
                StackItem::Single(entities[2]),
            ])]);
            let frame = |entity| (entity != entities[missing]).then(|| IRect::new(0, 0, 400, 300));
            let projected = strip
                .fixture_relative_positions(600, &frame)
                .collect::<Vec<_>>();
            assert_eq!(
                projected,
                vec![
                    (entities[1 - missing], IRect::new(0, 0, 400, 300)),
                    (entities[2], IRect::new(0, 300, 400, 600)),
                ]
            );
            assert_eq!(strip.all_windows(), entities);
        }
    }

    #[test]
    fn projection_uses_the_same_master_width_for_frames_and_column_offsets() {
        let (_, mut strip, entities) = setup_world_and_strip();
        strip.stack(entities[1]).unwrap();
        let frame = |entity| {
            Some(IRect::new(
                0,
                0,
                if entity == entities[1] { 700 } else { 300 },
                300,
            ))
        };
        let projected = strip
            .fixture_relative_positions(600, &frame)
            .collect::<Vec<_>>();
        assert_eq!(
            projected,
            vec![
                (entities[0], IRect::new(0, 0, 300, 300)),
                (entities[1], IRect::new(0, 300, 300, 600)),
                (entities[2], IRect::new(300, 0, 600, 600)),
            ]
        );
    }

    #[test]
    fn clamped_origins_and_endpoints_remain_representable_at_coordinate_limits() {
        for base in [i32::MIN, -2048, 0, i32::MAX - 1024] {
            let viewport = IRect::new(base, base, base + 1024, base + 768);
            for size in [1, 300, 2048, i32::MAX].map(Size::splat) {
                for origin in [i32::MIN, -999_999, -2048, 0, i32::MAX].map(Origin::splat) {
                    let actual = clamp_origin_to_viewport(origin, size, viewport);
                    assert!(crate::ecs::window_frame::checked_window_frame(actual, size).is_some());
                    assert_eq!(clamp_origin_to_viewport(actual, size, viewport), actual);
                    for (value, near, far, size) in [
                        (actual.x, viewport.min.x, viewport.max.x, size.x),
                        (actual.y, viewport.min.y, viewport.max.y, size.y),
                    ] {
                        let near = i64::from(near);
                        let far = i64::from(far) - i64::from(size);
                        assert!((near.min(far)..=near.max(far)).contains(&i64::from(value)));
                    }
                }
            }
        }
    }

    #[test]
    fn centered_origin_preserves_existing_rounding_for_odd_sizes_and_negative_positions() {
        let viewport = IRect::new(-1024, -1024, 1024, 1024);
        for origin in [-101, -100, 0, 99, 100].map(Origin::splat) {
            for width in [99, 100, 101] {
                let frame = IRect::from_corners(origin, origin + Size::splat(width));
                for size in [200, 201].map(Size::splat) {
                    assert_eq!(
                        centered_origin_in_viewport(frame, size, viewport),
                        clamp_origin_to_viewport(
                            IRect::from_center_size(frame.center(), size).min,
                            size,
                            viewport
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn clamp_origin_supports_oversized_windows() {
        let viewport = IRect::new(0, 20, 1024, 768);
        let size = Size::new(2048, 748);

        assert_eq!(
            clamp_origin_to_viewport(Origin::new(300, 20), size, viewport),
            Origin::new(0, 20)
        );
        assert_eq!(
            clamp_origin_to_viewport(Origin::new(-1600, 20), size, viewport),
            Origin::new(-1024, 20)
        );
        assert_eq!(
            clamp_origin_to_viewport(Origin::new(-600, 20), size, viewport),
            Origin::new(-600, 20)
        );
    }

    #[test]
    fn clamp_origin_keeps_regular_windows_inside_viewport() {
        let viewport = IRect::new(0, 20, 1024, 768);
        let size = Size::new(400, 300);

        assert_eq!(
            clamp_origin_to_viewport(Origin::new(-100, 900), size, viewport),
            Origin::new(0, 468)
        );
    }

    fn setup_world_and_strip() -> (World, LayoutStrip, Vec<Entity>) {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]);
        strip.append(entities[1]);
        strip.append(entities[2]);

        (world, strip, entities)
    }

    #[test]
    fn stacking_a_native_fullscreen_endpoint_preserves_every_column() {
        for fullscreen_index in [0, 1] {
            let (_, mut strip, entities) = setup_world_and_strip();
            strip.columns[fullscreen_index] = Column::Fullscreen(entities[fullscreen_index]);
            let before: Vec<Vec<Entity>> = strip
                .columns()
                .map(|column| column.window_iter().collect())
                .collect();
            strip.stack(entities[1]).unwrap();
            let after: Vec<Vec<Entity>> = strip
                .columns()
                .map(|column| column.window_iter().collect())
                .collect();
            assert_eq!(after, before, "fullscreen column {fullscreen_index}");
            assert!(matches!(
                strip.get(fullscreen_index).unwrap(),
                Column::Fullscreen(_)
            ));
        }
    }

    #[test]
    fn column_width_budget_checks_explicit_intents_without_observed_frames() {
        let (_, mut strip, entities) = setup_world_and_strip();
        strip.set_width_context(None, WidthIntent::Absolute(400.0));
        let id = strip.column_id(entities[0]).unwrap();
        strip
            .set_width_intent(id, WidthIntent::Absolute(f64::from(i32::MAX - 800)))
            .unwrap();
        assert!(strip.width_budget_is_valid());
        strip
            .set_width_intent(id, WidthIntent::Absolute(f64::from(i32::MAX - 799)))
            .unwrap();
        assert!(!strip.width_budget_is_valid());
        strip
            .set_width_intent(id, WidthIntent::ViewportRatio(0.5))
            .unwrap();
        assert!(
            strip.width_budget_is_valid(),
            "unknown viewport blocks projection, not valid intent admission"
        );
    }

    #[test]
    fn overflowing_column_offsets_publish_no_partial_layout_projection() {
        let (_, strip, entities) = setup_world_and_strip();
        let frame = |entity| {
            Some(IRect::new(
                0,
                0,
                if entity == entities[0] {
                    i32::MAX - 1
                } else {
                    1
                },
                500,
            ))
        };
        assert!(
            strip
                .column_positions(&frame)
                .collect::<Vec<_>>()
                .is_empty()
        );
        assert!(
            strip
                .fixture_relative_positions(500, &frame)
                .collect::<Vec<_>>()
                .is_empty()
        );
    }

    #[test]
    fn maximum_representable_column_offsets_preserve_every_window() {
        let (_, strip, entities) = setup_world_and_strip();
        let frame = |entity| {
            Some(IRect::new(
                0,
                0,
                if entity == entities[0] {
                    i32::MAX - 800
                } else {
                    400
                },
                500,
            ))
        };
        let projected = strip
            .fixture_relative_positions(500, &frame)
            .collect::<Vec<_>>();
        assert_eq!(projected.len(), 3);
        assert_eq!(projected[0].1.min.x, 0);
        assert_eq!(projected[1].1.min.x, i32::MAX - 800);
        assert_eq!(projected[2].1.max.x, i32::MAX);
        assert!(
            projected
                .iter()
                .all(|(_, frame)| frame.width() > 0 && frame.height() == 500)
        );
    }

    #[test]
    fn strip_has_changed_window_matches_nested_entities() {
        let (mut world, mut strip, entities) = setup_world_and_strip();
        strip.stack(entities[1]).unwrap();
        strip.convert_to_tabs(entities[0], entities[1]).unwrap();

        let mut changed = EntityHashSet::default();
        changed.insert(entities[1]);

        assert!(strip_has_changed_window(&strip, &changed));

        changed.clear();
        changed.insert(world.spawn_empty().id());

        assert!(!strip_has_changed_window(&strip, &changed));
    }

    #[test]
    fn strip_window_contexts_capture_stack_membership_once() {
        let (mut world, mut strip, entities) = setup_world_and_strip();
        strip.stack(entities[1]).unwrap();
        let display_entity = world.spawn_empty().id();
        let strip_position = Origin::new(10, 20);
        let mut contexts = EntityHashMap::default();

        insert_strip_window_contexts(&mut contexts, &strip, strip_position, true, display_entity);

        let stacked_leader = contexts.get(&entities[0]).unwrap();
        let stacked_follower = contexts.get(&entities[1]).unwrap();
        let single_window = contexts.get(&entities[2]).unwrap();

        assert_eq!(stacked_leader.strip_position, strip_position);
        assert_eq!(stacked_leader.display_entity, display_entity);
        assert!(stacked_leader.swiping);
        assert!(stacked_leader.stacked);
        assert!(stacked_follower.stacked);
        assert!(!single_window.stacked);
    }

    #[test]
    fn test_window_pane_index_of() {
        let (_world, strip, entities) = setup_world_and_strip();
        assert_eq!(strip.index_of(entities[0]).unwrap(), 0);
        assert_eq!(strip.index_of(entities[1]).unwrap(), 1);
        assert_eq!(strip.index_of(entities[2]).unwrap(), 2);
    }

    #[test]
    fn test_window_pane_swap() {
        let (_world, mut strip, entities) = setup_world_and_strip();
        strip.swap(0, 2);
        assert_eq!(strip.index_of(entities[2]).unwrap(), 0);
        assert_eq!(strip.index_of(entities[0]).unwrap(), 2);
    }

    #[test]
    fn test_window_pane_stack_and_unstack() {
        let (_world, mut strip, entities) = setup_world_and_strip();

        // Stack [1] onto [0]
        strip.stack(entities[1]).unwrap();
        assert_eq!(strip.len(), 2);
        assert_eq!(strip.index_of(entities[0]).unwrap(), 0);
        assert_eq!(strip.index_of(entities[1]).unwrap(), 0); // Both in the same panel

        // Check internal structure
        match strip.get(0).unwrap() {
            Column::Stack(stack) => {
                assert_eq!(stack.len(), 2);
                assert_eq!(stack[0], StackItem::Single(entities[0]));
                assert_eq!(stack[1], StackItem::Single(entities[1]));
            }
            Column::Single(_) | Column::Fullscreen(_) | Column::Tabs(_) => {
                panic!("Expected a stack")
            }
        }

        // Unstack [0]
        strip.unstack(entities[0]).unwrap();
        assert_eq!(strip.len(), 3);
        assert_eq!(strip.index_of(entities[1]).unwrap(), 0);
        assert_eq!(strip.index_of(entities[0]).unwrap(), 1);
        assert_eq!(strip.index_of(entities[2]).unwrap(), 2);
    }

    #[test]
    fn test_layout_positioning() {
        let mut world = World::new();
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();
        let sizes = [
            IRect::new(0, 0, 300, 300),
            IRect::new(0, 0, 300, 300),
            IRect::new(0, 0, 300, 300),
            IRect::new(0, 0, 300, 300),
        ];

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]);
        strip.append(entities[1]);
        strip.append(entities[2]);
        strip.append(entities[3]);

        _ = strip.stack(entities[2]);
        let get_window_frame = |_| Some(sizes[0]);
        let out = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect::<Vec<_>>();

        let xpos = out.iter().map(|(_, frame)| frame.min.x).collect::<Vec<_>>();
        assert_eq!(xpos, vec![0, 300, 300, 600]);

        let height = out
            .iter()
            .map(|(_, frame)| frame.height())
            .collect::<Vec<_>>();
        assert_eq!(height, vec![500, 250, 250, 500]);
    }

    /// Every single-column window must fill the full viewport height.
    #[test]
    fn test_layout_singles_get_full_viewport_height() {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        for &e in &entities {
            strip.append(e);
        }

        let get_window_frame = |_| Some(IRect::new(0, 0, 300, 400));
        let out: Vec<_> = strip
            .fixture_relative_positions(800, &get_window_frame)
            .collect();

        assert_eq!(out.len(), 3);
        for (_, f) in &out {
            assert_eq!(f.height(), 800, "single window should fill viewport height");
            assert_eq!(f.min.y, 0);
        }
        // x positions: 0, 300, 600
        let xs: Vec<_> = out.iter().map(|(_, f)| f.min.x).collect();
        assert_eq!(xs, vec![0, 300, 600]);
    }

    /// Stacked windows share the viewport height; all use the top window's width.
    #[test]
    fn test_layout_stack_shares_height_and_width() {
        let mut world = World::new();
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        for &e in &entities {
            strip.append(e);
        }
        // Stack e1, e2 onto e0: [Stack(e0, e1, e2), Single(e3)]
        strip.stack(entities[1]).unwrap();
        strip.stack(entities[2]).unwrap();

        // Give the master (e0) a distinct width so we can verify children
        // adopt it. Here e0 is 500px while its stacked children are 300px;
        // children must widen to the master's 500, not keep their own width.
        let get_window_frame = |e: Entity| {
            if e == entities[0] {
                Some(IRect::new(0, 0, 500, 200))
            } else if e == entities[1] || e == entities[2] {
                Some(IRect::new(0, 0, 300, 200))
            } else {
                Some(IRect::new(0, 0, 400, 500))
            }
        };

        let out: Vec<_> = strip
            .fixture_relative_positions(600, &get_window_frame)
            .collect();
        assert_eq!(out.len(), 4);

        // Every window in the stacked column must adopt the master's width.
        for e in [entities[0], entities[1], entities[2]] {
            let frame = out.iter().find(|(entity, _)| *entity == e).unwrap().1;
            assert_eq!(
                frame.width(),
                500,
                "stacked window must share the master's (top) width"
            );
        }

        // Stacked heights should sum to viewport height.
        let stack_heights: i32 = out
            .iter()
            .filter(|(e, _)| *e != entities[3])
            .map(|(_, f)| f.height())
            .sum();
        assert_eq!(stack_heights, 600, "stack heights must sum to viewport");

        // Stacked y positions should be contiguous from 0.
        let stack_frames: Vec<_> = out
            .iter()
            .filter(|(e, _)| *e != entities[3])
            .map(|(_, f)| *f)
            .collect();
        assert_eq!(stack_frames[0].min.y, 0);
        assert_eq!(stack_frames[0].max.y, stack_frames[1].min.y);
        assert_eq!(stack_frames[1].max.y, stack_frames[2].min.y);
        assert_eq!(stack_frames[2].max.y, 600);

        // e3 (single) gets full viewport height.
        let e3_frame = out.iter().find(|(e, _)| *e == entities[3]).unwrap().1;
        assert_eq!(e3_frame.height(), 600);
    }

    #[test]
    fn test_tabs_in_stack() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let e3 = world.spawn_empty().id();
        let e4 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);
        strip.append(e2);
        strip.append(e3);

        // [Single(e1), Single(e2), Single(e3)]
        strip.stack(e2).unwrap();
        // [Stack([Single(e1), Single(e2)]), Single(e3)]

        // Convert e1 (in stack) to tabs with e4
        strip.convert_to_tabs(e1, e4).unwrap();
        // [Stack([Tabs([e1, e4]), Single(e2)]), Single(e3)]

        assert_eq!(strip.len(), 2);
        match strip.get(0).unwrap() {
            Column::Stack(items) => {
                assert_eq!(items.len(), 2);
                match &items[0] {
                    StackItem::Tabs(tabs) => assert_eq!(tabs, &vec![e4, e1]),
                    StackItem::Single(_) => panic!("Expected Tabs in stack"),
                }
            }
            _ => panic!("Expected Stack"),
        }

        // relative_positions should yield e1, e4 (same frame) and e2
        let get_window_frame = |_| Some(IRect::new(0, 0, 100, 100));
        let out: Vec<_> = strip
            .fixture_relative_positions(400, &get_window_frame)
            .collect();

        // We expect e1, e4, e2 from the first column, and e3 from the second.
        assert_eq!(out.len(), 4);

        let e1_frame = out.iter().find(|(e, _)| *e == e1).unwrap().1;
        let e4_frame = out.iter().find(|(e, _)| *e == e4).unwrap().1;
        let e2_frame = out.iter().find(|(e, _)| *e == e2).unwrap().1;

        assert_eq!(e1_frame, e4_frame);
        assert_eq!(e1_frame.max.y, e2_frame.min.y);
    }

    /// Unstacking a window from a stack gives it its own column with full height.
    #[test]
    fn test_layout_unstack_gives_full_height() {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        for &e in &entities {
            strip.append(e);
        }
        // [Stack(e0, e1), Single(e2)]
        strip.stack(entities[1]).unwrap();

        let get_window_frame = |_| Some(IRect::new(0, 0, 300, 250));

        // Before unstack: e0 and e1 share 500px height.
        let out: Vec<_> = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect();
        let e1_height = out
            .iter()
            .find(|(e, _)| *e == entities[1])
            .unwrap()
            .1
            .height();
        assert!(e1_height < 500, "stacked e1 should not have full height");

        // Unstack e1: [Single(e0), Single(e1), Single(e2)]
        strip.unstack(entities[1]).unwrap();
        assert_eq!(strip.len(), 3);

        let out: Vec<_> = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect();
        for (_, f) in &out {
            assert_eq!(
                f.height(),
                500,
                "after unstack every single column gets full viewport height"
            );
        }
    }

    /// Re-stacking after unstack restores shared height distribution.
    #[test]
    fn test_layout_restack_restores_shared_heights() {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]);
        strip.append(entities[1]);

        let get_window_frame = |_| Some(IRect::new(0, 0, 300, 250));

        // Stack: [Stack(e0, e1)]
        strip.stack(entities[1]).unwrap();
        let out: Vec<_> = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect();
        let heights: Vec<_> = out.iter().map(|(_, f)| f.height()).collect();
        assert_eq!(heights.iter().sum::<i32>(), 500);
        assert_eq!(heights.len(), 2);

        // Unstack: [Single(e0), Single(e1)]
        strip.unstack(entities[1]).unwrap();
        let out: Vec<_> = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect();
        for (_, f) in &out {
            assert_eq!(f.height(), 500);
        }

        // Re-stack: [Stack(e0, e1)] — e1 stacks onto left neighbor e0
        strip.stack(entities[1]).unwrap();
        let out: Vec<_> = strip
            .fixture_relative_positions(500, &get_window_frame)
            .collect();
        let heights: Vec<_> = out.iter().map(|(_, f)| f.height()).collect();
        assert_eq!(heights.iter().sum::<i32>(), 500);
        assert_eq!(heights.len(), 2);
    }

    /// When window frames include padding (logical frame is wider than the visual
    /// window), columns must be placed edge-to-edge using the full logical width.
    /// This ensures the visual gap between windows equals the sum of their padding.
    #[test]
    fn test_column_positions_with_padded_frames() {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        for &e in &entities {
            strip.append(e);
        }

        // Simulate windows with padding=8: logical width = OS_width + 2*8.
        // Window 0: OS width 284, logical width 300
        // Window 1: OS width 384, logical width 400
        // Window 2: OS width 484, logical width 500
        let padded_frames = [
            IRect::new(0, 0, 300, 600), // logical frame with padding included
            IRect::new(0, 0, 400, 600),
            IRect::new(0, 0, 500, 600),
        ];

        let get_window_frame = |e: Entity| {
            if e == entities[0] {
                Some(padded_frames[0])
            } else if e == entities[1] {
                Some(padded_frames[1])
            } else {
                Some(padded_frames[2])
            }
        };

        let out: Vec<_> = strip
            .fixture_relative_positions(600, &get_window_frame)
            .collect();
        assert_eq!(out.len(), 3);

        // Columns must be edge-to-edge: each column starts where the previous ends.
        let xs: Vec<_> = out.iter().map(|(_, f)| f.min.x).collect();
        assert_eq!(
            xs,
            vec![0, 300, 700],
            "columns must be edge-to-edge using logical widths"
        );

        // Right edge of each window must equal left edge of the next.
        for i in 0..out.len() - 1 {
            assert_eq!(
                out[i].1.max.x,
                out[i + 1].1.min.x,
                "window {} right edge must equal window {} left edge",
                i,
                i + 1
            );
        }
    }

    /// Frames with no padding (padding=0) should still produce edge-to-edge layout.
    #[test]
    fn test_column_positions_no_padding() {
        let mut world = World::new();
        let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        for &e in &entities {
            strip.append(e);
        }

        let get_window_frame = |_| Some(IRect::new(0, 0, 300, 600));

        let out: Vec<_> = strip
            .fixture_relative_positions(600, &get_window_frame)
            .collect();
        let xs: Vec<_> = out.iter().map(|(_, f)| f.min.x).collect();
        assert_eq!(xs, vec![0, 300, 600]);

        // No gaps or overlaps.
        for i in 0..out.len() - 1 {
            assert_eq!(out[i].1.max.x, out[i + 1].1.min.x);
        }
    }

    #[test]
    fn test_convert_to_tabs() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let e3 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);
        strip.append(e3);

        // Convert e1 to a tab group with follower e2
        strip.convert_to_tabs(e1, e2).unwrap();

        assert_eq!(strip.len(), 2);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => {
                assert_eq!(tabs, vec![e2, e1]);
            }
            _ => panic!("Expected Tabs column"),
        }

        // Add another tab e4
        let e4 = world.spawn_empty().id();
        strip.convert_to_tabs(e1, e4).unwrap();
        assert_eq!(strip.len(), 2);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => {
                assert_eq!(tabs, vec![e4, e2, e1]);
            }
            _ => panic!("Expected Tabs column"),
        }
    }

    #[test]
    fn test_remove_from_tabs() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let e3 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);
        strip.convert_to_tabs(e1, e2).unwrap();
        strip.convert_to_tabs(e1, e3).unwrap();

        assert_eq!(strip.len(), 1);

        // Remove e2 (follower)
        strip.remove(e2);
        assert_eq!(strip.len(), 1);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => assert_eq!(tabs, vec![e3, e1]),
            _ => panic!(),
        }

        // Remove e1 (leader)
        strip.remove(e1);
        assert_eq!(strip.len(), 1);
        // Should convert back to Single since only e3 remains
        match strip.get(0).unwrap() {
            Column::Single(id) => assert_eq!(id, e3),
            _ => panic!("Expected Single column after removing all but one tab"),
        }
    }

    #[test]
    fn test_tab_group_returns_all_siblings() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let e3 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);
        strip.convert_to_tabs(e1, e2).unwrap();
        strip.append(e3);

        assert_eq!(strip.tab_group(e1), Some(vec![e2, e1]));
        assert_eq!(strip.tab_group(e2), Some(vec![e2, e1]));
        assert_eq!(strip.tab_group(e3), None);
    }

    #[test]
    fn test_append_tab_group_merges_existing_members_without_duplicate_columns() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let e3 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e2);
        strip.append(e3);

        strip.append_tab_group(&[e1, e2]);

        assert_eq!(strip.len(), 2);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => assert_eq!(tabs, vec![e1, e2]),
            _ => panic!("Expected merged Tabs column"),
        }
        assert_eq!(strip.index_of(e1).unwrap(), 0);
        assert_eq!(strip.index_of(e2).unwrap(), 0);
        assert_eq!(strip.index_of(e3).unwrap(), 1);
        assert_eq!(strip.all_windows(), vec![e1, e2, e3]);
    }

    #[test]
    fn test_tab_relative_positions_use_stable_slot_representative() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);
        strip.convert_to_tabs(e1, e2).unwrap();
        strip
            .get_column_mut(0)
            .expect("tab column")
            .move_to_front(e2);

        let get_window_frame = |entity| {
            if entity == e1 {
                Some(IRect::new(0, 0, 300, 600))
            } else {
                Some(IRect::new(900, 0, 1200, 400))
            }
        };

        let out = strip
            .fixture_relative_positions(600, &get_window_frame)
            .collect::<Vec<_>>();

        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .all(|(_, frame)| *frame == IRect::new(0, 0, 300, 600))
        );
    }

    #[test]
    fn test_overlapping_frame_strategy_simulation() {
        let mut world = World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(e1);

        // Simulate detection logic from spawn_window_trigger
        let leader_match = Some(e1); // Mocked match from frame comparison

        if let Some(leader) = leader_match {
            strip.convert_to_tabs(leader, e2).unwrap();
        } else {
            strip.append(e2);
        }

        assert_eq!(strip.len(), 1);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => assert_eq!(tabs, vec![e2, e1]),
            _ => panic!(),
        }
    }

    // Mirrors the real `detect_tabbed_windows` flow: spawn_window_trigger
    // appends the new window to the strip first, and only then does the
    // tab detector merge it into the leader's column. Before the fix, the
    // follower was left in both columns, and right_neighbour from the
    // duplicated entity would self-loop because index_of returned the Tabs
    // column index, while the column at the next index was the orphaned
    // Single(follower).
    #[test]
    fn test_convert_to_tabs_removes_pre_existing_follower_column() {
        let mut world = World::new();
        let leader = world.spawn_empty().id();
        let follower = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(leader);
        strip.append(follower);

        strip.convert_to_tabs(leader, follower).unwrap();

        assert_eq!(
            strip.len(),
            1,
            "follower must not remain in its own column after being tabbed onto leader",
        );
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => assert_eq!(tabs, vec![follower, leader]),
            other => panic!("expected Tabs column, got {other:?}"),
        }
    }

    #[test]
    fn test_convert_to_tabs_no_self_loop_on_neighbour() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let leader = world.spawn_empty().id();
        let follower = world.spawn_empty().id();
        let b = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(a);
        strip.append(leader);
        strip.append(follower);
        strip.append(b);

        strip.convert_to_tabs(leader, follower).unwrap();

        // Both leader-as-focus and follower-as-focus must navigate to `b`
        // east, not back onto themselves.
        assert_eq!(strip.right_neighbour(leader), Some(b));
        assert_eq!(strip.right_neighbour(follower), Some(b));
        assert_eq!(strip.left_neighbour(leader), Some(a));
        assert_eq!(strip.left_neighbour(follower), Some(a));
    }

    #[test]
    fn test_convert_to_tabs_handles_follower_left_of_leader() {
        let mut world = World::new();
        let follower = world.spawn_empty().id();
        let leader = world.spawn_empty().id();
        let b = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(follower);
        strip.append(leader);
        strip.append(b);

        strip.convert_to_tabs(leader, follower).unwrap();

        assert_eq!(strip.len(), 2);
        match strip.get(0).unwrap() {
            Column::Tabs(tabs) => assert_eq!(tabs, vec![follower, leader]),
            other => panic!("expected Tabs column, got {other:?}"),
        }
        assert_eq!(strip.right_neighbour(leader), Some(b));
        assert_eq!(strip.right_neighbour(follower), Some(b));
    }

    #[test]
    fn move_column_relative_moves_the_whole_stack() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let d = world.spawn_empty().id();

        let mut strip = LayoutStrip::default();
        strip.append(a);
        strip.append(b);
        strip.append(c);
        strip.append(d);
        strip.stack(c).expect("stack c onto b");

        assert!(strip.move_column_relative(c, d, Placement::After));
        assert_eq!(strip.all_windows(), vec![a, d, b, c]);
        match strip.get(2).expect("moved column") {
            Column::Stack(items) => {
                assert_eq!(items.len(), 2);
                assert!(items[0].contains(b));
                assert!(items[1].contains(c));
            }
            other => panic!("expected preserved stack, got {other:?}"),
        }
    }

    #[test]
    fn append_column_preserves_tabs_and_normalizes_fullscreen() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();

        let mut source = LayoutStrip::default();
        source.append(a);
        source.convert_to_tabs(a, b).expect("tabs");
        let tabs = source.column_containing(a).expect("tab column");

        let mut target = LayoutStrip::default();
        target.append(c);
        target.append_column(tabs);
        assert_eq!(target.all_windows(), vec![c, b, a]);
        assert!(matches!(target.get(1), Ok(Column::Tabs(_))));

        target.append_column(Column::Fullscreen(a));
        assert!(matches!(target.last(), Ok(Column::Single(entity)) if entity == a));
    }
}

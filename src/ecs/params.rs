use bevy::{
    ecs::{
        entity::Entity,
        hierarchy::ChildOf,
        query::{Has, Or, With, Without},
        system::{Commands, NonSend, Query, Res, ResMut, Single, SystemParam},
    },
    math::IRect,
};
use tracing::warn;

use super::{ActiveDisplayMarker, FocusFollowsMouse, SkipReshuffle};
use crate::{
    config::Config,
    ecs::{
        ActiveWorkspaceMarker, Bounds, DesiredWindowFrame, DockPosition, FlashMessage, Floating,
        FocusedMarker, FullWidthMarker, Initializing, LayoutPosition, NativeFullscreenMarker,
        ObservedWindowFrame, Position, PresentedWindowFrame, RepositionMarker, ResizeMarker,
        Scrolling, WidthRatio, WindowFrameMotion, WindowVisibility, layout::LayoutStrip,
        reconcile::WindowUnavailable,
    },
    manager::{Display, Origin, Size, Window},
    overlay::OverlayManager,
    platform::{WinID, WindowIncarnation},
};

/// A Bevy `SystemParam` that provides access to the application's configuration and related state.
/// It allows systems to query various configuration options and modify flags like `FocusFollowsMouse` or `SkipReshuffle`.
#[derive(SystemParam)]
pub struct GlobalState<'w> {
    /// Resource to manage the window ID for focus-follows-mouse behavior.
    focus_follows_mouse_id: ResMut<'w, FocusFollowsMouse>,
    /// Resource to determine if window reshuffling should be skipped.
    skip_reshuffle: ResMut<'w, SkipReshuffle>,

    initializing: Option<Res<'w, Initializing>>,
}

impl GlobalState<'_> {
    /// Returns the `WinID` of the window currently marked for focus-follows-mouse.
    ///
    /// # Returns
    ///
    /// An `Option<WinID>` if a window is marked, otherwise `None`.
    pub fn ffm_flag(&self) -> Option<WinID> {
        self.focus_follows_mouse_id.0
    }

    /// Sets the `WinID` for the focus-follows-mouse flag.
    ///
    /// # Arguments
    ///
    /// * `flag` - An `Option<WinID>` to set as the focus-follows-mouse target.
    pub fn set_ffm_flag(&mut self, flag: Option<WinID>) {
        self.focus_follows_mouse_id.as_mut().0 = flag;
    }

    /// Sets the `skip_reshuffle` flag.
    /// When `true`, window reshuffling logic will be temporarily bypassed.
    ///
    /// # Arguments
    ///
    /// * `to` - A boolean value to set the `skip_reshuffle` flag to.
    pub fn set_skip_reshuffle(&mut self, to: bool) {
        self.skip_reshuffle.as_mut().0 = to;
    }

    /// Returns `true` if window reshuffling should be skipped.
    ///
    /// # Returns
    ///
    /// `true` if reshuffling is skipped, `false` otherwise.
    pub fn skip_reshuffle(&self) -> bool {
        self.skip_reshuffle.0
    }

    pub fn initializing(&self) -> bool {
        self.initializing.is_some()
    }
}

/// A Bevy `SystemParam` that provides immutable access to the currently active `Display`.
/// It ensures that only one display is marked as active at any given time.
#[derive(SystemParam)]
pub struct ActiveDisplay<'w, 's> {
    strip: Single<
        'w,
        's,
        (
            &'static LayoutStrip,
            Entity,
            Option<&'static NativeFullscreenMarker>,
        ),
        With<ActiveWorkspaceMarker>,
    >,
    /// The single active `Display` component, marked with `ActiveDisplayMarker`.
    display: Single<
        'w,
        's,
        (&'static Display, Entity, Option<&'static DockPosition>),
        With<ActiveDisplayMarker>,
    >,
}

impl ActiveDisplay<'_, '_> {
    /// Returns an immutable reference to the active `Display`.
    pub fn display(&self) -> &Display {
        self.display.0
    }

    pub fn active_strip(&self) -> &LayoutStrip {
        self.strip.0
    }

    pub fn active_strip_entity(&self) -> Entity {
        self.strip.1
    }

    pub fn fullscreen(&self) -> Option<&NativeFullscreenMarker> {
        self.strip.2
    }

    /// Returns the `IRect` representing the bounds of the active display.
    pub fn bounds(&self) -> IRect {
        self.display.0.bounds()
    }

    pub fn dock(&self) -> Option<&DockPosition> {
        self.display.2
    }

    /// Returns the `IRect` representing the bounds of the active display, correctly padded by
    /// potential dock position and or padding configuration.
    pub fn actual_bounds(&self, config: &Config) -> IRect {
        self.display().actual_display_bounds(self.dock(), config)
    }
}

/// A Bevy `SystemParam` that provides mutable access to the currently active `Display` and other displays.
/// It allows systems to modify the active display and its associated `LayoutStrip`s.
#[derive(SystemParam)]
pub struct ActiveDisplayMut<'w, 's> {
    strip: Single<'w, 's, &'static mut LayoutStrip, With<ActiveWorkspaceMarker>>,
    /// The single active `Display` component, marked with `ActiveDisplayMarker`.
    display: Single<
        'w,
        's,
        (&'static mut Display, Entity, Option<&'static DockPosition>),
        With<ActiveDisplayMarker>,
    >,
    /// A query for all other `Display` components that are not marked as active.
    other_displays: Query<
        'w,
        's,
        (Entity, &'static mut Display, Option<&'static DockPosition>),
        Without<ActiveDisplayMarker>,
    >,
}

impl ActiveDisplayMut<'_, '_> {
    pub fn display(&self) -> &Display {
        &self.display.0
    }

    pub fn dock(&self) -> Option<&DockPosition> {
        self.display.2
    }

    pub(crate) fn other_contexts(
        &self,
    ) -> impl Iterator<Item = (Entity, &Display, Option<&DockPosition>)> {
        self.other_displays.iter()
    }

    pub fn active_strip(&mut self) -> &mut LayoutStrip {
        &mut self.strip
    }

    /// Reads the strip for command planning without marking it changed.
    pub fn active_strip_ref(&self) -> &LayoutStrip {
        &self.strip
    }

    /// Returns the `IRect` representing the bounds of the active display, correctly padded by
    /// potential dock position and or padding configuration.
    pub fn actual_bounds(&self, config: &Config) -> IRect {
        self.display().actual_display_bounds(self.dock(), config)
    }
}

/// Work that needs prompt ticks; used by the event pump to bound its sleep.
#[derive(SystemParam)]
pub struct FrameActivity<'w, 's> {
    initializing: Option<Res<'w, Initializing>>,
    repositioning: Query<'w, 's, (), With<RepositionMarker>>,
    resizing: Query<'w, 's, (), With<ResizeMarker>>,
    window_motion: Query<'w, 's, (), With<WindowFrameMotion>>,
    scrolling: Query<'w, 's, (), With<Scrolling>>,
    flash_messages: Query<'w, 's, (), With<FlashMessage>>,
    overlay_manager: Option<NonSend<'w, OverlayManager>>,
    bar_manager: Option<NonSend<'w, crate::bar::BarManager>>,
}

impl FrameActivity<'_, '_> {
    /// Startup retains `Initializing` until incremental discovery settles. Its
    /// cursor must progress even when no window is animating and no event arrives.
    pub fn mid_frame(&self) -> bool {
        self.initializing.is_some()
            || !self.repositioning.is_empty()
            || !self.resizing.is_empty()
            || !self.window_motion.is_empty()
            || !self.scrolling.is_empty()
            || !self.flash_messages.is_empty()
            || self
                .overlay_manager
                .as_ref()
                .is_some_and(|manager| manager.decorations_are_animating())
            || self
                .bar_manager
                .as_ref()
                .is_some_and(|manager| manager.is_animating())
    }
}

/// Bundles the window queries, config, and a command buffer that most
/// window-handling systems need. Only add this to a system that already used
/// all three: granting extra world access can cause a query-conflict panic.
#[derive(SystemParam)]
pub struct WindowCtx<'w, 's> {
    pub windows: Windows<'w, 's>,
    pub config: Res<'w, Config>,
    pub commands: Commands<'w, 's>,
}

/// A window's layout inputs, declarative frame projections, width ratio, and
/// any command requests not yet consumed by the frame pipeline.
type WindowPlacements<'w, 's> = Query<
    'w,
    's,
    (
        &'static LayoutPosition,
        &'static Position,
        &'static Bounds,
        Option<&'static ObservedWindowFrame>,
        Option<&'static PresentedWindowFrame>,
        Option<&'static DesiredWindowFrame>,
        &'static WidthRatio,
        Option<&'static RepositionMarker>,
        Option<&'static ResizeMarker>,
    ),
    With<Window>,
>;

type AvailableWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static Window,
        Entity,
        &'static ChildOf,
        Has<Floating>,
        Option<&'static WindowVisibility>,
    ),
    Without<WindowUnavailable>,
>;

type AllWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static Window,
        Entity,
        &'static ChildOf,
        Has<Floating>,
        Option<&'static WindowVisibility>,
    ),
>;

type FocusedWindows<'w, 's> =
    Query<'w, 's, (&'static Window, Entity), (With<FocusedMarker>, Without<WindowUnavailable>)>;

type ReassignedWindows<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<super::native_space::NativeMoveOwner>,
        With<super::workspace::WindowSpaceReassignmentPending>,
    )>,
>;

#[derive(SystemParam)]
pub struct Windows<'w, 's> {
    all: AllWindows<'w, 's>,
    available: AvailableWindows<'w, 's>,
    focus: FocusedWindows<'w, 's>,
    previous_size: Query<
        'w,
        's,
        (
            &'static Window,
            Entity,
            &'static WidthRatio,
            &'static FullWidthMarker,
        ),
        With<FullWidthMarker>,
    >,
    positions: WindowPlacements<'w, 's>,
    reassigned: ReassignedWindows<'w, 's>,
}

#[derive(Clone, Copy, Debug)]
pub struct TrackedWindowState<'a> {
    floating: bool,
    visibility: Option<&'a WindowVisibility>,
}

impl<'a> TrackedWindowState<'a> {
    pub fn is_floating(self) -> bool {
        self.floating
    }

    pub fn is_tiled(self) -> bool {
        !self.floating
    }

    pub fn is_visible(self) -> bool {
        self.visibility.is_none()
    }

    pub fn visibility(self) -> Option<&'a WindowVisibility> {
        self.visibility
    }
}

impl Windows<'_, '_> {
    /// Navigation is a projection of retained layout, not raw membership.
    /// Removing ineligible members from a copy preserves stacks and tab order.
    pub(crate) fn navigable_strip(&self, strip: &LayoutStrip) -> LayoutStrip {
        let mut projected = strip.clone();
        for entity in strip.all_windows() {
            if !self
                .get_tracked(entity)
                .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
            {
                projected.remove(entity);
            }
        }
        projected
    }

    /// Retained source membership is not permission to mutate a layout while
    /// native reassignment owns its geometry and captured column structure.
    pub(crate) fn layout_is_writable(&self, entity: Entity) -> bool {
        self.available.contains(entity) && !self.reassigned.contains(entity)
    }

    pub(crate) fn layout_column_is_writable(&self, strip: &LayoutStrip, entity: Entity) -> bool {
        strip
            .index_of(entity)
            .and_then(|index| strip.get(index))
            .is_ok_and(|column| {
                column
                    .window_iter()
                    .all(|member| self.layout_is_writable(member))
            })
    }

    pub fn get_tracked(&self, entity: Entity) -> Option<(&Window, Entity, TrackedWindowState<'_>)> {
        let (window, entity, _, floating, visibility) = self
            .available
            .get(entity)
            .inspect_err(|error| {
                if self.all.get(entity).is_err() {
                    warn!("unable to find window: {error}");
                }
            })
            .ok()?;
        Some((
            window,
            entity,
            TrackedWindowState {
                floating,
                visibility,
            },
        ))
    }

    pub fn get(&self, entity: Entity) -> Option<&Window> {
        self.available
            .get(entity)
            .ok()
            .map(|(window, _, _, _, _)| window)
    }

    pub fn find(&self, window_id: WinID) -> Option<(&Window, Entity)> {
        self.available
            .into_iter()
            .find(|(window, _, _, _, _)| window.id() == window_id)
            .map(|(window, entity, _, _, _)| (window, entity))
    }

    /// Identity lookup only: the returned window may be unavailable for AX operations.
    pub(crate) fn find_any(&self, window_id: WinID) -> Option<(&Window, Entity)> {
        self.all
            .iter()
            .find(|(window, _, _, _, _)| window.id() == window_id)
            .map(|(window, entity, _, _, _)| (window, entity))
    }

    pub fn find_incarnation(
        &self,
        window_id: WinID,
        incarnation: WindowIncarnation,
    ) -> Option<(&Window, Entity)> {
        self.available
            .iter()
            .find(|(window, _, _, _, _)| {
                window.id() == window_id && window.incarnation() == incarnation
            })
            .map(|(window, entity, _, _, _)| (window, entity))
    }

    pub fn get_parent(&self, entity: Entity) -> Option<(&Window, Entity, Entity)> {
        self.available
            .get(entity)
            .ok()
            .map(|(window, entity, childof, _, _)| (window, entity, childof.parent()))
    }

    pub fn get_parent_any(&self, entity: Entity) -> Option<(&Window, Entity, Entity)> {
        self.all
            .get(entity)
            .ok()
            .map(|(window, entity, childof, _, _)| (window, entity, childof.parent()))
    }

    pub fn is_available(&self, entity: Entity) -> bool {
        self.available.contains(entity)
    }

    pub fn find_parent_matching(
        &self,
        window_id: WinID,
        mut eligible: impl FnMut(&Window, Entity) -> bool,
    ) -> Option<(&Window, Entity, Entity)> {
        self.available
            .iter()
            .find_map(|(window, entity, childof, _, _)| {
                (window.id() == window_id && eligible(window, childof.parent())).then_some((
                    window,
                    entity,
                    childof.parent(),
                ))
            })
    }

    pub fn find_parent_incarnation_any(
        &self,
        window_id: WinID,
        incarnation: WindowIncarnation,
    ) -> Option<(&Window, Entity, Entity)> {
        self.all.iter().find_map(|(window, entity, childof, _, _)| {
            (window.id() == window_id && window.incarnation() == incarnation).then_some((
                window,
                entity,
                childof.parent(),
            ))
        })
    }

    pub fn find_tiled(&self, window_id: WinID) -> Option<(&Window, Entity)> {
        self.available
            .iter()
            .find_map(|(window, entity, _, floating, visibility)| {
                (!floating && visibility.is_none() && window.id() == window_id)
                    .then_some((window, entity))
            })
    }

    pub fn focused(&self) -> Option<(&Window, Entity)> {
        self.focus.single().ok()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Window, Entity)> {
        self.available
            .iter()
            .map(|(window, entity, _, _, _)| (window, entity))
    }

    pub fn tiled_iter(&self) -> impl Iterator<Item = (&Window, Entity, &ChildOf)> {
        self.available
            .iter()
            .filter_map(|(window, entity, childof, floating, visibility)| {
                (!floating && visibility.is_none()).then_some((window, entity, childof))
            })
    }

    pub fn full_width(&self, entity: Entity) -> Option<&FullWidthMarker> {
        self.previous_size
            .get(entity)
            .map(|(_, _, _, marker)| marker)
            .ok()
    }

    pub fn origin(&self, entity: Entity) -> Option<Origin> {
        self.positions
            .get(entity)
            .ok()
            .map(|(_, origin, _, _, _, _, _, _, _)| origin.0)
    }

    pub fn size(&self, entity: Entity) -> Option<Size> {
        self.positions
            .get(entity)
            .ok()
            .map(|(_, _, size, _, _, _, _, _, _)| size.0)
    }

    pub fn width_ratio(&self, entity: Entity) -> Option<f64> {
        self.positions
            .get(entity)
            .ok()
            .map(|(_, _, _, _, _, _, ratio, _, _)| ratio.0)
    }

    /// Physical facts never fall back to an animation sample or layout intent.
    pub(crate) fn observed_frame(&self, entity: Entity) -> Option<IRect> {
        let (_, _, _, observed, _, _, _, _, _) = self.positions.get(entity).ok()?;
        observed.map(|frame| frame.0)
    }

    /// Best available geometry for legacy layout calculations, not a physical fact.
    pub fn frame(&self, entity: Entity) -> Option<IRect> {
        self.positions
            .get(entity)
            .ok()
            .map(|(_, origin, size, observed, presented, _, _, _, _)| {
                observed
                    .map(|frame| frame.0)
                    .or_else(|| presented.map(|frame| frame.0))
                    .unwrap_or_else(|| IRect::from_corners(origin.0, origin.0 + size.0))
            })
    }

    pub fn moving_frame(&self, entity: Entity) -> Option<IRect> {
        let (_, origin, size, _, _, desired, _, reposition, resize) =
            self.positions.get(entity).ok()?;
        let frame = match desired {
            Some(desired) => desired.0,
            None => super::window_frame::checked_window_frame(origin.0, size.0)?,
        };
        let size = super::window_frame::checked_frame_size(frame)?;
        super::window_frame::checked_window_frame(
            reposition.map_or(frame.min, |request| request.0),
            resize.map_or(size, |request| request.0),
        )
    }

    /// Layout inputs including commands queued earlier in this frame.
    pub fn requested_frame(&self, entity: Entity) -> Option<IRect> {
        let (_, origin, size, _, _, _, _, reposition, resize) = self.positions.get(entity).ok()?;
        super::window_frame::checked_window_frame(
            reposition.map_or(origin.0, |request| request.0),
            resize.map_or(size.0, |request| request.0),
        )
    }

    pub fn layout_position(&self, entity: Entity) -> Option<&LayoutPosition> {
        self.positions
            .get(entity)
            .ok()
            .map(|(layout_position, _, _, _, _, _, _, _, _)| layout_position)
    }
}

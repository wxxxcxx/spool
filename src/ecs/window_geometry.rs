//! Coalesces user- or application-driven tiled-window geometry changes.
//!
//! macOS may emit many interleaved move and resize notifications for one
//! pointer gesture. During that burst the confirmed OS frame is kept current
//! for overlays, while the layout intent stays stable. Once the notifications
//! go quiet, the final frame is committed to the strip exactly once.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::ecs::change_detection::DetectChangesMut as _;
use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, With, Without};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::math::IRect;
use bevy::time::Time;
use tracing::{Level, debug, instrument};

use super::layout::LayoutStrip;
use super::reconcile::WindowUnavailable;
use super::workspace::WindowSpaceReassignmentPending;
use super::{
    Bounds, DesiredWindowFrame, Floating, MouseHeldMarker, ObservedWindowFrame, Position,
    PresentedWindowFrame, RepositionMarker, ResizeMarker, SpawnCommandsExt, WindowFrameMotion,
    WindowVisibility,
};
use crate::events::Event;
use crate::manager::Window;
use crate::platform::{WinID, WindowIncarnation};

const GEOMETRY_QUIET_PERIOD: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug)]
struct PendingGeometry {
    window_id: WinID,
    incarnation: WindowIncarnation,
    start: IRect,
    latest: IRect,
    last_signal: Duration,
    reshuffle_on_settle: bool,
}

/// Session-local ledger of tiled windows whose external geometry is still
/// changing. Callers only need to record a confirmed frame or ask whether an
/// entity is settling; notification ordering and quiet-period policy stay
/// inside this module.
#[derive(Debug, Default, Resource)]
pub(crate) struct WindowGeometrySettling {
    pending: HashMap<Entity, PendingGeometry>,
}

impl WindowGeometrySettling {
    pub(crate) fn record(
        &mut self,
        entity: Entity,
        window: &Window,
        start: IRect,
        latest: IRect,
        now: Duration,
    ) {
        let window_id = window.id();
        let incarnation = window.incarnation();
        self.pending
            .entry(entity)
            .and_modify(|pending| {
                if pending.window_id == window_id && pending.incarnation == incarnation {
                    pending.latest = latest;
                    pending.last_signal = now;
                } else {
                    *pending = PendingGeometry {
                        window_id,
                        incarnation,
                        start,
                        latest,
                        last_signal: now,
                        reshuffle_on_settle: false,
                    };
                }
            })
            .or_insert(PendingGeometry {
                window_id,
                incarnation,
                start,
                latest,
                last_signal: now,
                reshuffle_on_settle: false,
            });
    }

    pub(crate) fn contains(&self, entity: Entity) -> bool {
        self.pending.contains_key(&entity)
    }

    /// Returns `true` when the reshuffle was attached to an active geometry
    /// gesture and will therefore be emitted after the final frame settles.
    pub(crate) fn defer_reshuffle(&mut self, entity: Entity) -> bool {
        let Some(pending) = self.pending.get_mut(&entity) else {
            return false;
        };
        pending.reshuffle_on_settle = true;
        true
    }

    fn take_ready(
        &mut self,
        now: Duration,
        mouse_held: &HashSet<Entity>,
    ) -> Vec<(Entity, PendingGeometry)> {
        let ready = self
            .pending
            .iter()
            .filter_map(|(entity, pending)| {
                (!mouse_held.contains(entity)
                    && now.saturating_sub(pending.last_signal) >= GEOMETRY_QUIET_PERIOD)
                    .then_some(*entity)
            })
            .collect::<Vec<_>>();
        ready
            .into_iter()
            .filter_map(|entity| {
                self.pending
                    .remove(&entity)
                    .map(|pending| (entity, pending))
            })
            .collect()
    }
}

type GeometryWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Window,
        Entity,
        &'static mut Position,
        &'static mut Bounds,
        &'static mut DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
        Option<&'static mut ObservedWindowFrame>,
        Option<&'static WindowVisibility>,
        Has<RepositionMarker>,
        Has<ResizeMarker>,
        Has<WindowFrameMotion>,
        Has<Floating>,
    ),
    (
        Without<LayoutStrip>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

/// Reads move and resize notifications through one seam so their ordering
/// cannot produce two different partial layout updates.
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn observe_external_window_geometry(
    mut messages: MessageReader<Event>,
    mut windows: GeometryWindows,
    layout_strips: Query<&LayoutStrip>,
    time: Res<Time>,
    mut settling: ResMut<WindowGeometrySettling>,
) {
    for event in messages.read() {
        let (window_id, incarnation) = match event {
            Event::WindowMoved {
                window_id,
                incarnation,
            }
            | Event::WindowResized {
                window_id,
                incarnation,
            } => (*window_id, *incarnation),
            _ => continue,
        };

        let Some((
            mut window,
            entity,
            mut position,
            mut bounds,
            mut desired,
            mut presented,
            observed,
            visibility,
            moving,
            resizing,
            presenting,
            floating,
        )) = windows
            .iter_mut()
            .find(|window| window.0.id() == window_id && window.0.incarnation() == incarnation)
        else {
            continue;
        };
        if visibility.is_some() || moving || resizing || presenting {
            continue;
        }
        let Ok(frame) = window.update_frame() else {
            continue;
        };
        let echoed_write = observed
            .as_ref()
            .is_some_and(|observed| super::reconcile::frames_equivalent(observed.0, frame));
        if let Some(mut observed) = observed
            && observed.0 != frame
        {
            observed.0 = frame;
        }
        if echoed_write {
            continue;
        }
        if layout_strips
            .iter()
            .any(|strip| strip.is_fullscreen() && strip.contains(entity))
            || window.try_is_full_screen().unwrap_or(true)
        {
            continue;
        }

        if floating {
            // Floating windows have no neighbours to disturb, so their ECS
            // projection remains live instead of waiting for the quiet period.
            if position.0 != frame.min {
                position.0 = frame.min;
            }
            if bounds.0 != frame.size() {
                bounds.0 = frame.size();
            }
            if desired.0 != frame {
                desired.0 = frame;
            }
            if presented.0 != frame {
                presented.0 = frame;
            }
            continue;
        }

        let start = IRect::from_corners(position.0, position.0 + bounds.0);
        settling.record(entity, &window, start, frame, time.elapsed());
    }
}

type SettledWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static Window,
        &'static mut Position,
        &'static mut Bounds,
        &'static mut DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
        Has<Floating>,
    ),
    (
        With<Window>,
        Without<LayoutStrip>,
        Without<WindowUnavailable>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type GeometryStrips<'w, 's> = Query<
    'w,
    's,
    (&'static mut LayoutStrip, &'static mut Position),
    (With<LayoutStrip>, Without<Window>),
>;

/// Commits the final frame of each quiet gesture and invalidates its strip
/// once. Left/top edge corrections are calculated from the pre-gesture frame,
/// never from intermediate notifications.
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn settle_external_window_geometry(
    time: Res<Time>,
    mut settling: ResMut<WindowGeometrySettling>,
    mut windows: SettledWindows,
    mut strips: GeometryStrips,
    mouse_held: Query<&MouseHeldMarker>,
    mut commands: Commands,
) {
    let mouse_held = mouse_held.iter().map(|marker| marker.0).collect();
    for (entity, pending) in settling.take_ready(time.elapsed(), &mouse_held) {
        if strips
            .iter()
            .any(|(strip, _)| strip.is_fullscreen() && strip.contains(entity))
        {
            continue;
        }
        let floating = {
            let Ok((window, mut position, mut bounds, mut desired, mut presented, floating)) =
                windows.get_mut(entity)
            else {
                continue;
            };
            if window.id() != pending.window_id || window.incarnation() != pending.incarnation {
                continue;
            }

            if position.0 != pending.latest.min {
                position.0 = pending.latest.min;
            }
            if bounds.0 != pending.latest.size() {
                bounds.0 = pending.latest.size();
            }
            if desired.0 != pending.latest {
                desired.0 = pending.latest;
            }
            if presented.0 != pending.latest {
                presented.0 = pending.latest;
            }
            floating
        };
        if floating {
            continue;
        }

        let above = {
            let Some((mut strip, mut strip_position)) =
                strips.iter_mut().find(|(strip, _)| strip.contains(entity))
            else {
                continue;
            };
            let tabbed = strip.tabbed(entity);
            let resized = pending.start.size() != pending.latest.size();
            if resized && !tabbed && pending.start.min.x != pending.latest.min.x {
                // Preserve the confirmed origin directly. Inferring the left
                // edge from the width delta only works when the right edge is
                // perfectly fixed; applications may report a complete frame
                // where origin and both edges changed together.
                strip_position.0.x += pending.latest.min.x - pending.start.min.x;
            }
            let vertical_shift = pending.start.min.y - pending.latest.min.y;
            let above = (!tabbed && vertical_shift != 0)
                .then(|| strip.above(entity))
                .flatten();
            strip.set_changed();
            above.map(|above| (above, vertical_shift))
        };

        if let Some((above, vertical_shift)) = above
            && let Ok((_, _, mut above_bounds, _, _, _)) = windows.get_mut(above)
            && above_bounds.0.y - vertical_shift > 200
        {
            above_bounds.0.y -= vertical_shift;
        }

        debug!(
            window_id = pending.window_id,
            start = ?pending.start,
            final_frame = ?pending.latest,
            "settled external window geometry"
        );
        if pending.reshuffle_on_settle {
            commands.reshuffle_around(entity);
        }
    }
}

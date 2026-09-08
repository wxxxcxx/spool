//! Declarative window-frame pipeline.
//!
//! Layout and commands write [`DesiredWindowFrame`]. Animation derives
//! [`PresentedWindowFrame`], the only frame committed to macOS. AX/WindowServer
//! reads update `ObservedWindowFrame` separately, so an optimistic write can
//! never masquerade as physical truth.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::{With, Without};
use bevy::ecs::system::{Commands, Query, Res};
use bevy::math::{IRect, IVec2};
use bevy::time::Time;
use tracing::{Level, debug, instrument, trace};

use super::workspace::WindowSpaceReassignmentPending;
use super::{Bounds, Position, RepositionMarker, ResizeMarker, Window};
use crate::config::Config;

const ANIMATE_SNAP_THRESHOLD: i32 = 5;

/// The final frame derived from Spool's current layout state.
///
/// This is analogous to a rendered value in a React-style architecture: it is
/// replaced whenever state changes and is never overwritten by animation or a
/// speculative macOS write.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesiredWindowFrame(pub IRect);

/// The frame currently being presented by the animation/effects layer.
///
/// This is the sole geometry projection allowed to drive macOS writes during
/// normal operation.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentedWindowFrame(pub IRect);

/// Marks a window whose presentation has not yet converged to its desired
/// frame.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug, Default)]
pub struct WindowFrameMotion;

/// An audit request, revalidated against current intent at commit time.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug)]
pub(crate) struct WindowFrameCorrection(pub IRect);

/// Prepared initialization geometry; success, not enqueueing, completes defaults.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug)]
pub(crate) struct DefaultWindowFrame {
    pub(crate) target: IRect,
    pub(crate) incarnation: crate::platform::WindowIncarnation,
}

/// Coalesced pointer intent. Only confirmed commit readback enters settling.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug)]
pub(crate) struct InteractiveWindowFrame {
    pub(crate) target: IRect,
    pub(crate) start: IRect,
    pub(crate) incarnation: crate::platform::WindowIncarnation,
}

/// One physical attempt owned by a pending display transfer. Membership,
/// rather than AX success, decides when the source layout can be released.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug)]
pub(crate) struct DisplayTransferFrame {
    pub(crate) target: IRect,
    pub(crate) viewport: IRect,
    pub(crate) source_space_id: crate::platform::WorkspaceId,
    pub(crate) target_space_id: crate::platform::WorkspaceId,
    pub(crate) display_id: u32,
    pub(crate) incarnation: crate::platform::WindowIncarnation,
    pub(crate) tiled: bool,
}

#[derive(bevy::ecs::component::Component, Clone, Copy, Debug)]
pub(crate) struct DisplayTransferReadback {
    pub(crate) frame: IRect,
    pub(crate) viewport_width: i32,
    pub(crate) incarnation: crate::platform::WindowIncarnation,
    pub(crate) tiled: bool,
}

/// Suspends frame-by-frame commits after an AX failure or constrained correction.
///
/// The declarative desired frame remains intact; the central reconciler owns
/// bounded retry admission; the presentation committer executes the retries.
/// A genuinely new desired frame clears this suspension and
/// starts a fresh presentation attempt.
/// New pointer input also gets one attempt; failed input is never replayed.
#[derive(bevy::ecs::component::Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFrameCommitSuspended {
    desired: IRect,
}

impl WindowFrameCommitSuspended {
    pub(crate) fn new(desired: IRect) -> Self {
        Self { desired }
    }
}

type WindowFrameRequests<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Position,
        &'static mut Bounds,
        Option<&'static DesiredWindowFrame>,
        Option<&'static PresentedWindowFrame>,
        Option<&'static RepositionMarker>,
        Option<&'static ResizeMarker>,
    ),
    (With<Window>, Without<WindowSpaceReassignmentPending>),
>;

type AnimatedWindowFrames<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
    ),
    (
        With<WindowFrameMotion>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type SuspendedWindowFrames<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static DesiredWindowFrame,
        &'static PresentedWindowFrame,
        &'static WindowFrameCommitSuspended,
    ),
>;

pub(crate) fn checked_window_frame(origin: IVec2, size: IVec2) -> Option<IRect> {
    if size.x <= 0 || size.y <= 0 {
        return None;
    }
    let max = IVec2::new(origin.x.checked_add(size.x)?, origin.y.checked_add(size.y)?);
    Some(IRect::from_corners(origin, max))
}

pub(crate) fn checked_frame_size(frame: IRect) -> Option<IVec2> {
    let size = IVec2::new(
        frame.max.x.checked_sub(frame.min.x)?,
        frame.max.y.checked_sub(frame.min.y)?,
    );
    (size.x > 0 && size.y > 0).then_some(size)
}

/// Re-enables projection only when layout state produces a new desired frame.
pub(crate) fn resume_suspended_window_frame_commits(
    windows: SuspendedWindowFrames,
    mut commands: Commands,
) {
    for (entity, desired, presented, suspended) in &windows {
        if desired.0 == suspended.desired {
            continue;
        }
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<WindowFrameCommitSuspended>();
            if presented.0 != desired.0 {
                entity_commands.try_insert(WindowFrameMotion);
            }
        }
    }
}

/// Adapts legacy movement/resize commands into declarative layout state.
///
/// `LayoutStrip` still uses `Position`/`Bounds` internally while the migration
/// is in progress. The request becomes an immediate desired frame; when it
/// also changes tiled layout inputs, the following layout pass may replace
/// that target with the fully derived frame.
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn apply_window_frame_requests(
    mut windows: WindowFrameRequests,
    mut commands: Commands,
) {
    for (entity, mut position, mut bounds, desired, presented, reposition, resize) in &mut windows {
        if reposition.is_none() && resize.is_none() {
            continue;
        }

        let origin = reposition.map_or(position.0, |request| request.0);
        let size = resize.map_or(bounds.0, |request| request.0);
        let Some(target) = checked_window_frame(origin, size) else {
            debug!(
                ?entity,
                ?origin,
                ?size,
                "ignoring an unrepresentable window frame"
            );
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.remove::<(RepositionMarker, ResizeMarker)>();
            }
            continue;
        };
        if position.0 != origin {
            position.0 = origin;
        }
        if bounds.0 != size {
            bounds.0 = size;
        }

        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.remove::<(RepositionMarker, ResizeMarker)>();
            if desired.is_none_or(|desired| desired.0 != target) {
                entity_commands.try_insert(DesiredWindowFrame(target));
            }
            if presented.is_none() {
                entity_commands.try_insert(PresentedWindowFrame(target));
            } else if presented.is_some_and(|presented| presented.0 != target) {
                entity_commands.try_insert(WindowFrameMotion);
            }
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to [0, 1], so only sub-pixel precision is discarded"
)]
fn ease_out_factor(rate: f64, delta: f64) -> f32 {
    (1.0 - (-rate * delta).exp()).clamp(0.0, 1.0) as f32
}

fn interpolate_vector(current: IVec2, target: IVec2, factor: f32) -> IVec2 {
    current
        .as_vec2()
        .lerp(target.as_vec2(), factor)
        .round()
        .as_ivec2()
}

pub(crate) fn interpolate_frame(current: IRect, target: IRect, factor: f32) -> IRect {
    IRect::from_corners(
        interpolate_vector(current.min, target.min, factor),
        interpolate_vector(current.max, target.max, factor),
    )
}

fn frame_is_close(left: IRect, right: IRect) -> bool {
    let min_delta = (left.min - right.min).abs().max_element();
    let max_delta = (left.max - right.max).abs().max_element();
    min_delta.max(max_delta) <= ANIMATE_SNAP_THRESHOLD
}

/// Derives the presentation projection from the desired projection.
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn animate_presented_window_frames(
    mut windows: AnimatedWindowFrames,
    time: Res<Time>,
    config: Res<Config>,
    mut commands: Commands,
) {
    let factor = ease_out_factor(config.animation_speed(), time.delta_secs_f64());
    for (entity, desired, mut presented) in &mut windows {
        let next = interpolate_frame(presented.0, desired.0, factor);
        let finished = frame_is_close(next, desired.0);
        let next = if finished { desired.0 } else { next };
        trace!(?entity, current = ?presented.0, target = ?desired.0, ?next, "presenting window frame");
        if presented.0 != next {
            presented.0 = next;
        }
        if finished && let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<WindowFrameMotion>();
        }
    }
}

/// Updates the desired frame and schedules presentation convergence.
pub(crate) fn set_desired_frame(
    entity: Entity,
    frame: IRect,
    snap: bool,
    desired: Option<&mut DesiredWindowFrame>,
    presented: Option<&mut PresentedWindowFrame>,
    commands: &mut Commands,
) {
    let presented_matches = presented.as_ref().is_some_and(|value| value.0 == frame);
    if let Some(desired) = desired {
        if desired.0 != frame {
            desired.0 = frame;
        }
    } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(DesiredWindowFrame(frame));
    }

    if snap {
        if let Some(presented) = presented {
            if presented.0 != frame {
                presented.0 = frame;
            }
        } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert(PresentedWindowFrame(frame));
        }
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<WindowFrameMotion>();
        }
    } else if !presented_matches && let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(WindowFrameMotion);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_moves_origin_and_size_as_one_frame() {
        let current = IRect::new(0, 20, 400, 300);
        let target = IRect::new(100, 40, 800, 600);

        assert_eq!(
            interpolate_frame(current, target, 0.5),
            IRect::new(50, 30, 600, 450)
        );
    }
}

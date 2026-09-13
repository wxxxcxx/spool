//! Declarative window-frame pipeline.
//!
//! Layout and commands write [`DesiredWindowFrame`]. Animation derives
//! [`PresentedWindowFrame`], the only frame committed to macOS. AX/WindowServer
//! reads update `ObservedWindowFrame` separately, so an optimistic write can
//! never masquerade as physical truth.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::{With, Without};
use bevy::ecs::system::{Commands, Local, Query, Res};
use bevy::math::{IRect, IVec2};
use bevy::time::Time;
use std::collections::{HashMap, HashSet};
use tracing::{Level, debug, instrument, trace};

use super::workspace::WindowSpaceReassignmentPending;
use super::{Bounds, Position, RepositionMarker, ResizeMarker, Window};
use crate::config::Config;

const ANIMATE_SNAP_THRESHOLD: i64 = 5;
const MAX_FRAME_ANIMATION_SECONDS: f64 = 1.0;

/// Pure presentation transition. Native observations and attempt budgets have
/// separate owners; reaching this target never claims that macOS accepted it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameTransition {
    target: IRect,
    presented: IRect,
    origin: IRect,
    elapsed: f64,
}

impl FrameTransition {
    fn new(presented: IRect, target: IRect) -> Self {
        Self {
            target,
            presented,
            origin: presented,
            elapsed: 0.0,
        }
    }

    fn retarget(&mut self, target: IRect) {
        if self.target != target {
            self.origin = self.presented;
            self.target = target;
            self.elapsed = 0.0;
        }
    }

    fn advance(&mut self, delta: f64, rate: f64) -> IRect {
        if delta.is_finite() && delta > 0.0 {
            self.elapsed += delta;
        }
        if !rate.is_finite() || rate <= 0.0 || self.elapsed >= MAX_FRAME_ANIMATION_SECONDS {
            return self.snap_to_target();
        }
        // Sample from the origin using accumulated time, rather than rounding
        // each increment back into the next frame. Small steps cannot stall.
        let next = interpolate_frame(
            self.origin,
            self.target,
            ease_out_factor(rate, self.elapsed),
        );
        self.presented = if frame_is_close(next, self.target) {
            self.target
        } else {
            next
        };
        self.presented
    }

    fn snap_to_target(&mut self) -> IRect {
        self.presented = self.target;
        self.presented
    }

    fn is_finished(&self) -> bool {
        self.presented == self.target
    }
}

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
        Without<WindowFrameCommitSuspended>,
        Without<WindowSpaceReassignmentPending>,
    ),
>;

type SuspendedWindowFrames<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static DesiredWindowFrame,
        &'static mut PresentedWindowFrame,
        &'static WindowFrameCommitSuspended,
        Option<&'static super::ObservedWindowFrame>,
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
    mut windows: SuspendedWindowFrames,
    mut commands: Commands,
) {
    for (entity, desired, mut presented, suspended, observed) in &mut windows {
        if desired.0 == suspended.desired {
            continue;
        }
        // A previous write may have partially succeeded. Rebase a new
        // presentation on readback; without it, submit the endpoint directly.
        let origin = observed.map_or(desired.0, |observed| observed.0);
        if presented.0 != origin {
            presented.0 = origin;
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
    [left.min.x, left.min.y, left.max.x, left.max.y]
        .into_iter()
        .zip([right.min.x, right.min.y, right.max.x, right.max.y])
        .all(|(left, right)| (i64::from(left) - i64::from(right)).abs() <= ANIMATE_SNAP_THRESHOLD)
}

/// Derives presentation while keeping target change detection independent.
#[instrument(level = Level::TRACE, skip_all)]
#[allow(
    clippy::too_many_arguments,
    reason = "independent ECS inputs gate presentation before advancing it"
)]
pub(crate) fn animate_presented_window_frames(
    mut windows: AnimatedWindowFrames,
    time: Res<Time>,
    config: Res<Config>,
    strips: Query<&super::layout::LayoutStrip>,
    participants: super::params::LayoutParticipants,
    mut sync: bevy::ecs::system::ResMut<super::reconcile::WindowStateSync>,
    mut transitions: Local<HashMap<Entity, FrameTransition>>,
    mut commands: Commands,
) {
    let mut live = HashSet::new();
    for (entity, desired, mut presented) in &mut windows {
        if strips.iter().any(|strip| {
            strip.contains(entity)
                && (strip.projection_is_blocked() || participants.height_blocked(strip))
        }) {
            sync.finish_frame_attempt(entity, desired.0, false);
            commands
                .entity(entity)
                .remove::<WindowFrameMotion>()
                .insert(WindowFrameCommitSuspended::new(desired.0));
            continue;
        }
        live.insert(entity);
        let transition = transitions
            .entry(entity)
            .or_insert_with(|| FrameTransition::new(presented.0, desired.0));
        if transition.presented != presented.0 {
            *transition = FrameTransition::new(presented.0, desired.0);
        }
        transition.retarget(desired.0);
        let next = transition.advance(time.delta_secs_f64(), config.animation_speed());
        trace!(?entity, current = ?presented.0, target = ?desired.0, ?next, "presenting window frame");
        if presented.0 != next {
            presented.0 = next;
        }
        if transition.is_finished()
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            entity_commands.try_remove::<WindowFrameMotion>();
        }
    }
    transitions.retain(|entity, _| live.contains(entity));
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
    #[test]
    fn transition_is_bounded_even_when_integer_steps_would_stall() {
        let start = IRect::new(0, 0, 600, 400);
        let end = IRect::new(0, 0, 900, 400);
        for rate in [0.001, 0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut transition = FrameTransition::new(start, end);
            for _ in 0..101 {
                transition.advance(0.01, rate);
            }
            assert_eq!(transition.presented, end);
            assert!(transition.is_finished());
        }
    }

    #[test]
    fn retarget_preserves_presentation_and_equal_targets_do_not_restart() {
        let mut transition =
            FrameTransition::new(IRect::new(0, 0, 600, 400), IRect::new(0, 0, 900, 400));
        let midway = transition.advance(0.1, 5.0);
        transition.retarget(IRect::new(0, 0, 1200, 400));
        assert_eq!(transition.presented, midway);
        assert_eq!(transition.advance(0.0, 5.0), midway);
        for _ in 0..101 {
            transition.retarget(IRect::new(0, 0, 1200, 400));
            transition.advance(0.01, 0.001);
        }
        assert!(transition.is_finished());
    }
    #[test]
    fn native_animation_frames_share_one_attempt_and_finish_at_exact_target() {
        use crate::ecs::reconcile::WindowStateSync;
        use bevy::ecs::schedule::{IntoScheduleConfigs, Schedule};
        let config: Config = (
            crate::config::MainOptions {
                animation_speed: Some(8.0),
                ..Default::default()
            },
            vec![],
        )
            .into();
        let mut harness = crate::tests::TestHarness::new()
            .with_config(config)
            .with_windows(1);
        harness.pump_frames(15);
        let entity = crate::tests::find_window_entity(0, harness.world());
        let start = harness
            .world()
            .get::<PresentedWindowFrame>(entity)
            .unwrap()
            .0;
        let target = IRect::from_corners(start.min, start.max + IVec2::new(300, 0));
        harness
            .world()
            .entity_mut(entity)
            .insert((DesiredWindowFrame(target), WindowFrameMotion));
        let writes = harness.mock_state.frame_write_attempts(0);
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                animate_presented_window_frames,
                crate::ecs::systems::commit_window_frame,
            )
                .chain(),
        );
        for _ in 0..70 {
            harness
                .world()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(16));
            schedule.run(harness.world());
        }
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(target));
        assert!(harness.mock_state.frame_write_attempts(0) > writes + 3);
        let progress = harness
            .world()
            .resource::<WindowStateSync>()
            .frame_progress(entity)
            .unwrap();
        assert_eq!(progress.attempts, 1);
        assert!(progress.confirmed);
        assert!(harness.world().get::<WindowFrameMotion>(entity).is_none());
    }

    #[test]
    fn failed_native_animation_stops_and_correction_goes_directly_to_endpoint() {
        use crate::ecs::reconcile::WindowStateSync;
        use bevy::ecs::schedule::{IntoScheduleConfigs, Schedule};
        let config: Config = (
            crate::config::MainOptions {
                animation_speed: Some(8.0),
                ..Default::default()
            },
            vec![],
        )
            .into();
        let mut harness = crate::tests::TestHarness::new()
            .with_config(config)
            .with_windows(1);
        harness.pump_frames(15);
        let entity = crate::tests::find_window_entity(0, harness.world());
        let start = harness
            .world()
            .get::<PresentedWindowFrame>(entity)
            .unwrap()
            .0;
        let target = IRect::from_corners(start.min, start.max + IVec2::new(300, 0));
        harness
            .world()
            .entity_mut(entity)
            .insert((DesiredWindowFrame(target), WindowFrameMotion));
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                animate_presented_window_frames,
                crate::ecs::systems::commit_window_frame,
            )
                .chain(),
        );
        harness.mock_state.reject_frame_writes(0, true);
        let writes = harness.mock_state.frame_write_attempts(0);
        for _ in 0..10 {
            harness
                .world()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(16));
            schedule.run(harness.world());
        }
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes + 1);
        assert_eq!(
            harness
                .world()
                .resource::<WindowStateSync>()
                .frame_progress(entity)
                .unwrap()
                .attempts,
            1
        );
        assert!(harness.world().get::<WindowFrameMotion>(entity).is_none());
        harness.mock_state.reject_frame_writes(0, false);
        harness
            .world()
            .entity_mut(entity)
            .insert(WindowFrameCorrection(target));
        schedule.run(harness.world());
        assert_eq!(harness.mock_state.actual_window_frame(0), Some(target));
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes + 2);
        assert_eq!(
            harness
                .world()
                .resource::<WindowStateSync>()
                .frame_progress(entity)
                .unwrap()
                .attempts,
            2
        );
    }
    #[test]
    fn a_new_blocked_intent_cancels_old_animation_and_cannot_confirm_old_target() {
        use crate::ecs::layout::{LayoutStrip, WidthConstraint, WidthIntent};
        use crate::ecs::reconcile::WindowStateSync;
        let config: Config = (
            crate::config::MainOptions {
                animation_speed: Some(8.0),
                ..Default::default()
            },
            vec![],
        )
            .into();
        let mut harness = crate::tests::TestHarness::new()
            .with_config(config)
            .with_windows(1);
        harness.pump_frames(15);
        let entity = crate::tests::find_window_entity(0, harness.world());
        {
            let world = harness.world();
            let mut strips = world.query::<&mut LayoutStrip>();
            let mut strip = strips
                .iter_mut(world)
                .find(|strip| strip.contains(entity))
                .unwrap();
            let id = strip.column_id(entity).unwrap();
            strip
                .set_width_intent(id, WidthIntent::Absolute(600.0))
                .unwrap();
        }
        harness.pump_frames(1);
        let old_target = harness.world().get::<DesiredWindowFrame>(entity).unwrap().0;
        assert!(harness.world().get::<WindowFrameMotion>(entity).is_some());
        let writes = harness.mock_state.frame_write_attempts(0);
        {
            let world = harness.world();
            let mut strips = world.query::<&mut LayoutStrip>();
            let mut strip = strips
                .iter_mut(world)
                .find(|strip| strip.contains(entity))
                .unwrap();
            let id = strip.column_id(entity).unwrap();
            strip
                .set_width_intent(id, WidthIntent::Absolute(900.0))
                .unwrap();
            strip
                .set_column_constraints(id, vec![WidthConstraint::Unsupported])
                .unwrap();
            assert!(strip.projection_is_blocked());
        }
        harness.pump_frames(20);
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
        assert!(harness.world().get::<WindowFrameMotion>(entity).is_none());
        harness
            .mock_state
            .os_set_window_frame_silently(0, old_target);
        harness.world().entity_mut(entity).insert((
            super::super::ObservedWindowFrame(old_target),
            WindowFrameCorrection(old_target),
        ));
        harness.pump_frames(1);
        let progress = harness
            .world()
            .resource::<WindowStateSync>()
            .frame_progress(entity)
            .unwrap();
        assert!(
            !progress.confirmed,
            "A readback cannot complete the accepted but blocked B intent"
        );
        assert!(!progress.active);
        assert_eq!(harness.mock_state.frame_write_attempts(0), writes);
    }
}

//! Coalesces user- or application-driven tiled-window geometry changes.
//!
//! macOS may emit many interleaved move and resize notifications for one
//! pointer gesture. During that burst the confirmed OS frame is kept current
//! for overlays, while the layout intent stays stable. Once the notifications
//! go quiet, the final frame is committed to the strip exactly once.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::config::Config;
use bevy::ecs::change_detection::DetectChanges as _;
use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, With, Without};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::math::IRect;
use bevy::time::Time;
use tracing::{Level, debug, instrument};

use super::layout::{ColumnId, LayoutStrip, WidthIntent};
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

/// Binds an external sample to both raw intent and its derivation context.
/// Configuration change ticks also reject a candidate before the layout pass
/// has refreshed inherited width or viewport projections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GeometryContext {
    column: ColumnId,
    intent_revision: u64,
    structure_revision: u64,
    height_revision: u64,
    height_viewport: Option<i32>,
    height_participants: Vec<super::layout::StackItemId>,
    effective_height: i32,
    target: IRect,
    effective_width: i32,
    configuration_tick: u32,
}

impl GeometryContext {
    pub(crate) fn capture(
        entity: Entity,
        target: IRect,
        configuration_tick: u32,
        strip: &LayoutStrip,
        eligible: &impl Fn(Entity) -> bool,
    ) -> Option<Self> {
        let index = strip.index_of(entity).ok()?;
        if strip.projection_is_blocked() {
            return None;
        }
        let state = strip.column_state(index)?;
        let item = strip.height_state(entity)?;
        let heights = strip
            .effective_stack_heights_for(index, strip.height_viewport(), eligible)
            .ok()?;
        let effective_height = heights.iter().find(|(id, _)| *id == item.id)?.1.slot;
        Some(Self {
            column: state.id,
            intent_revision: state.intent_revision,
            structure_revision: strip.structure_revision(),
            height_revision: state.height_revision,
            height_viewport: strip.height_viewport(),
            height_participants: heights.iter().map(|(id, _)| *id).collect(),
            effective_height,
            target,
            effective_width: strip.effective_column_width(index).ok()?.slot,
            configuration_tick,
        })
    }
}

#[derive(Clone, Debug)]
struct PendingGeometry {
    window_id: WinID,
    incarnation: WindowIncarnation,
    start: IRect,
    latest: IRect,
    last_signal: Duration,
    reshuffle_on_settle: bool,
    context: GeometryContext,
    explicit: bool,
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
    #[allow(
        clippy::too_many_arguments,
        reason = "captures identity, frame evidence, time and intent provenance atomically"
    )]
    pub(crate) fn record(
        &mut self,
        entity: Entity,
        window: &Window,
        start: IRect,
        latest: IRect,
        now: Duration,
        context: GeometryContext,
        explicit: bool,
    ) {
        let window_id = window.id();
        let incarnation = window.incarnation();
        self.pending
            .entry(entity)
            .and_modify(|pending| {
                if pending.window_id == window_id && pending.incarnation == incarnation {
                    pending.explicit |= explicit;
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
                        context: context.clone(),
                        explicit,
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
                context,
                explicit,
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
        Without<super::tiled_visibility::ParkedTile>,
    ),
>;

/// Reads move and resize notifications through one seam so their ordering
/// cannot produce two different partial layout updates.
#[instrument(level = Level::TRACE, skip_all)]
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "observation and admissible external intent have distinct ordered gates"
)]
pub(crate) fn observe_external_window_geometry(
    mut messages: MessageReader<Event>,
    mut windows: GeometryWindows,
    layout_strips: Query<&LayoutStrip>,
    participants: super::params::LayoutParticipants,
    config: Res<Config>,
    time: Res<Time>,
    mut settling: ResMut<WindowGeometrySettling>,
    mouse_held: Query<&MouseHeldMarker>,
    mut commands: Commands,
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
        if visibility.is_some()
            || layout_strips
                .iter()
                .any(|strip| strip.is_inactive_tab(entity))
        {
            continue;
        }
        let Ok(frame) = window.update_frame() else {
            continue;
        };
        let prior_fulfilled = observed
            .as_ref()
            .is_some_and(|value| super::reconcile::frames_equivalent(value.0, desired.0));
        let explicit_gesture = mouse_held.iter().any(|marker| marker.0 == entity);
        let echoed_write = observed
            .as_ref()
            .is_some_and(|observed| super::reconcile::frames_equivalent(observed.0, frame));
        if let Some(mut observed) = observed {
            if observed.0 != frame {
                observed.0 = frame;
            }
        } else {
            commands.entity(entity).insert(ObservedWindowFrame(frame));
        }
        if echoed_write || moving || resizing || presenting {
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
            // The observation becomes the intent: a floating frame is state, and
            // this is the fact that replaces it. The snap above keeps a drag
            // responsive; the derivation then agrees with what is already here.
            super::floating_geometry::set_floating_frame(&mut commands, entity, frame);
            continue;
        }

        // Stable geometry alone does not prove user intent. Never adopt a
        // conflicting observation over an outstanding target without a gesture.
        if !explicit_gesture && !prior_fulfilled && !settling.contains(entity) {
            continue;
        }
        let Some(context) = layout_strips
            .iter()
            .filter(|strip| !participants.height_blocked(strip))
            .find_map(|strip| {
                GeometryContext::capture(
                    entity,
                    desired.0,
                    config.last_changed().get(),
                    strip,
                    &|member| participants.contains(member),
                )
            })
        else {
            continue;
        };
        if !explicit_gesture
            && layout_strips
                .iter()
                .find(|strip| strip.contains(entity))
                .is_some_and(|strip| {
                    height_adoption_blocked(strip, entity, &participants)
                        || strip.index_of(entity).ok().is_none_or(|index| {
                            strip.effective_column_width(index).map_or(true, |width| {
                                width.constrained || width.slot != desired.0.width()
                            })
                        })
                })
        {
            continue;
        }
        let start = IRect::from_corners(position.0, position.0 + bounds.0);
        settling.record(
            entity,
            &window,
            start,
            frame,
            time.elapsed(),
            context,
            explicit_gesture,
        );
    }
}

type SettledWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Window,
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
        Without<super::tiled_visibility::ParkedTile>,
    ),
>;

type GeometryStrips<'w, 's> = Query<
    'w,
    's,
    (&'static mut LayoutStrip, &'static mut Position),
    (With<LayoutStrip>, Without<Window>),
>;

fn height_adoption_blocked(
    strip: &LayoutStrip,
    entity: Entity,
    participants: &super::params::LayoutParticipants,
) -> bool {
    let Some(item) = strip.height_state(entity) else {
        return true;
    };
    let Ok(index) = strip.index_of(entity) else {
        return true;
    };
    strip
        .effective_stack_heights_for(index, strip.height_viewport(), &|member| {
            participants.contains(member)
        })
        .map_or(true, |heights| {
            heights
                .iter()
                .find(|(id, _)| *id == item.id)
                .is_none_or(|(_, height)| height.constrained)
        })
}

/// A top-edge gesture transfers space from the preceding *participating* item.
/// Other gestures update the target share and redistribute participating
/// siblings proportionally. Retained-but-ordered-out items keep their raw
/// weights and never absorb or donate space.
fn adopt_external_height(
    strip: &mut LayoutStrip,
    entity: Entity,
    start: IRect,
    latest: IRect,
    participants: &super::params::LayoutParticipants,
) -> crate::errors::Result<()> {
    let eligible = |member: Entity| participants.contains(member);
    let index = strip.index_of(entity)?;
    if strip
        .column_height_items(index)
        .map_or(0, <[super::layout::StackItemState]>::len)
        < 2
    {
        return Ok(());
    }
    let viewport = strip.height_viewport();
    let vertical_shift = f64::from(start.min.y) - f64::from(latest.min.y);
    // A vertical gesture belongs to the participant immediately above. It must
    // be the previous *participating* item, never a retained but ordered-out
    // sibling, or the drag would corrupt an excluded weight.
    let has_donor = vertical_shift != 0.0
        && strip
            .height_state(entity)
            .zip(
                strip
                    .effective_stack_heights_for(index, viewport, &eligible)
                    .ok(),
            )
            .is_some_and(|(item, projection)| {
                projection
                    .iter()
                    .position(|(id, _)| *id == item.id)
                    .is_some_and(|ordinal| ordinal > 0)
            });
    let target_points = f64::from(latest.height());
    if has_donor {
        // The previous participant is the sole donor: it alone yields the
        // remainder so untouched siblings keep their exact shares.
        strip.adopt_height_from_previous_for(entity, target_points, viewport, &eligible)?;
    } else {
        strip.adopt_height_for(entity, target_points, viewport, &eligible)?;
    }
    Ok(())
}

/// Commits the final frame of each quiet gesture and invalidates its strip
/// once. Left/top edge corrections are calculated from the pre-gesture frame,
/// never from intermediate notifications.
#[instrument(level = Level::TRACE, skip_all)]
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "fresh read and version validation precede one atomic geometry adoption"
)]
pub(crate) fn settle_external_window_geometry(
    time: Res<Time>,
    config: Res<Config>,
    mut settling: ResMut<WindowGeometrySettling>,
    mut windows: SettledWindows,
    mut strips: GeometryStrips,
    participants: super::params::LayoutParticipants,
    mouse_held: Query<&MouseHeldMarker>,
    mut commands: Commands,
) {
    let mouse_held = mouse_held.iter().map(|marker| marker.0).collect();
    for (entity, pending) in settling.take_ready(time.elapsed(), &mouse_held) {
        let Ok((_, _, _, desired, _, _)) = windows.get(entity) else {
            continue;
        };
        let current = strips
            .iter()
            .filter(|(strip, _)| !participants.height_blocked(strip))
            .find_map(|(strip, _)| {
                GeometryContext::capture(
                    entity,
                    desired.0,
                    config.last_changed().get(),
                    strip,
                    &|member| participants.contains(member),
                )
            });
        if current.as_ref() != Some(&pending.context) {
            continue;
        }
        if !pending.explicit
            && strips
                .iter()
                .find(|(strip, _)| strip.contains(entity))
                .is_some_and(|(strip, _)| {
                    height_adoption_blocked(strip, entity, &participants)
                        || strip.index_of(entity).ok().is_none_or(|index| {
                            strip
                                .effective_column_width(index)
                                .map_or(true, |width| width.constrained)
                        })
                })
        {
            continue;
        }
        if strips.iter().any(|(strip, _)| {
            (strip.is_fullscreen() && strip.contains(entity)) || strip.is_inactive_tab(entity)
        }) {
            continue;
        }
        {
            let Ok((mut window, _, _, _, _, floating)) = windows.get_mut(entity) else {
                continue;
            };
            if floating
                || window.id() != pending.window_id
                || window.incarnation() != pending.incarnation
            {
                continue;
            }
            let Ok(fresh) = window.update_frame() else {
                continue;
            };
            commands.entity(entity).insert(ObservedWindowFrame(fresh));
            if fresh != pending.latest {
                settling.record(
                    entity,
                    &window,
                    pending.start,
                    fresh,
                    time.elapsed(),
                    pending.context,
                    pending.explicit,
                );
                continue;
            }
        }
        let Some((mut strip, mut strip_position)) =
            strips.iter_mut().find(|(strip, _)| strip.contains(entity))
        else {
            continue;
        };
        let mut proposed = strip.clone();
        if pending.start.width() != pending.latest.width()
            && proposed
                .set_width_intent(
                    pending.context.column,
                    WidthIntent::Absolute(f64::from(pending.latest.width())),
                )
                .is_err()
        {
            continue;
        }
        if pending.start.height() != pending.latest.height()
            && adopt_external_height(
                &mut proposed,
                entity,
                pending.start,
                pending.latest,
                &participants,
            )
            .is_err()
        {
            continue;
        }
        if !proposed.width_budget_is_valid() {
            continue;
        }
        let resized = pending.start.size() != pending.latest.size();
        let next_x = if resized && !strip.tabbed(entity) {
            i64::from(strip_position.0.x) + i64::from(pending.latest.min.x)
                - i64::from(pending.start.min.x)
        } else {
            i64::from(strip_position.0.x)
        };
        let Ok(next_x) = i32::try_from(next_x) else {
            continue;
        };
        if resized {
            *strip = proposed;
            strip_position.0.x = next_x;
        }
        // Reality is a presentation starting point; the layout pass alone
        // derives Bounds/Desired from the newly accepted domain transaction.
        if let Ok((_, mut position, _, mut desired, mut presented, _)) = windows.get_mut(entity) {
            position.0 = pending.latest.min;
            presented.0 = pending.latest;
            if !resized {
                // Pure movement retains the existing position-adoption policy;
                // it does not edit any height share or invalidate the strip.
                desired.0 = pending.latest;
            }
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

#[cfg(test)]
mod context_tests {
    use super::*;
    use crate::config::MainOptions;
    use crate::tests::{TestHarness, find_window_entity};
    use bevy::ecs::system::RunSystemOnce as _;

    fn candidate() -> (TestHarness, Entity) {
        let config: Config = (
            MainOptions {
                preset_column_widths: vec![0.5],
                ..Default::default()
            },
            vec![{
                let mut rule = crate::config::WindowParams::new(".*", None);
                rule.width = Some(0.5);
                rule
            }],
        )
            .into();
        let mut harness = TestHarness::new().with_config(config).with_windows(1);
        harness.pump_frames(15);
        let entity = find_window_entity(0, harness.world());
        let initial = harness.world().get::<DesiredWindowFrame>(entity).unwrap().0;
        let external =
            IRect::from_corners(initial.min, initial.max + bevy::math::IVec2::new(80, 0));
        let incarnation = harness.world().get::<Window>(entity).unwrap().incarnation();
        harness.mock_state.os_set_window_frame_silently(0, external);
        harness.world().write_message(Event::WindowResized {
            window_id: 0,
            incarnation,
        });
        harness.pump_frames(1);
        assert!(
            harness
                .world()
                .resource::<WindowGeometrySettling>()
                .contains(entity)
        );
        (harness, entity)
    }

    fn assert_inherited(harness: &mut TestHarness, entity: Entity) {
        let world = harness.world();
        let mut strips = world.query::<&LayoutStrip>();
        let strip = strips
            .iter(world)
            .find(|strip| strip.contains(entity))
            .unwrap();
        assert_eq!(
            strip
                .column_state(strip.index_of(entity).unwrap())
                .unwrap()
                .width,
            WidthIntent::InheritConfig
        );
        assert!(!world.resource::<WindowGeometrySettling>().contains(entity));
    }

    #[test]
    fn configuration_change_invalidates_candidate_even_before_layout_refresh() {
        for refresh_layout in [false, true] {
            let (mut harness, entity) = candidate();
            let config: Config = (
                MainOptions {
                    preset_column_widths: vec![0.75],
                    ..Default::default()
                },
                vec![{
                    let mut rule = crate::config::WindowParams::new(".*", None);
                    rule.width = Some(0.75);
                    rule
                }],
            )
                .into();
            harness.world().insert_resource(config);
            if refresh_layout {
                harness.pump_frames(3);
                assert_eq!(
                    harness
                        .world()
                        .get::<DesiredWindowFrame>(entity)
                        .unwrap()
                        .0
                        .width(),
                    768
                );
            } else {
                // No layout/config projection system has run yet: only the
                // configuration tick can distinguish this newer accepted input.
                harness
                    .world()
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_millis(200));
                harness
                    .world()
                    .run_system_once(settle_external_window_geometry)
                    .unwrap();
                assert_eq!(
                    harness
                        .world()
                        .get::<DesiredWindowFrame>(entity)
                        .unwrap()
                        .0
                        .width(),
                    512
                );
            }
            assert_inherited(&mut harness, entity);
        }
    }

    #[test]
    fn viewport_change_invalidates_candidate_before_new_frame_projection() {
        let (mut harness, entity) = candidate();
        {
            let world = harness.world();
            let mut strips = world.query::<&mut LayoutStrip>();
            let mut strip = strips
                .iter_mut(world)
                .find(|strip| strip.contains(entity))
                .unwrap();
            strip.set_width_context(Some(2048), WidthIntent::ViewportRatio(0.5));
        }
        harness
            .world()
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(200));
        harness
            .world()
            .run_system_once(settle_external_window_geometry)
            .unwrap();
        assert_inherited(&mut harness, entity);
        assert_eq!(
            harness
                .world()
                .get::<DesiredWindowFrame>(entity)
                .unwrap()
                .0
                .width(),
            512
        );
    }
    #[test]
    fn height_edit_invalidates_an_older_external_geometry_sample() {
        let (mut harness, entity) = candidate();
        {
            let world = harness.world();
            let mut strips = world.query::<&mut LayoutStrip>();
            let mut strip = strips
                .iter_mut(world)
                .find(|strip| strip.contains(entity))
                .unwrap();
            strip.set_height_weight(entity, 2.0).unwrap();
        }
        harness
            .world()
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(200));
        harness
            .world()
            .run_system_once(settle_external_window_geometry)
            .unwrap();
        assert_inherited(&mut harness, entity);
        let world = harness.world();
        let mut strips = world.query::<&LayoutStrip>();
        let strip = strips
            .iter(world)
            .find(|strip| strip.contains(entity))
            .unwrap();
        assert_eq!(
            strip.height_state(entity).unwrap().weight.to_bits(),
            2.0_f64.to_bits()
        );
    }
}

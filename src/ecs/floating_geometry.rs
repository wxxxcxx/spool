//! Retained geometry for floating windows.
//!
//! A floating window's frame is authored state: the edit commands and the
//! adoption of an external gesture both write this intent, and the effective
//! target is derived from it against the display that holds the window. Nothing
//! here writes to the platform — presenting and committing the derived target is
//! the frame pipeline's work, and a repair only changes the intent.

use bevy::ecs::system::SystemParam;
use bevy::math::IRect;
use bevy::prelude::*;

use super::layout::clamp_origin_to_viewport;
use super::topology::NativeTopology;
use super::{
    ActiveDisplayMarker, Bounds, DesiredWindowFrame, DockPosition, Floating, Position,
    PresentedWindowFrame, WindowFrameMotion,
};
use crate::config::Config;
use crate::manager::Display;

/// How many repairs the diagnostic history keeps, newest last.
const FLOATING_REPAIR_HISTORY: usize = 4;

/// The last frame write the platform answered as invalid for this floating
/// window.
///
/// A floating window's frame follows the observation, so a refused move needs no
/// alignment — the next observation is the intent again (ADR 0011). This record
/// exists only so `window inspect` can answer "why did it not move"; it drives
/// nothing, and a later tiled convergence drops it.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct FloatingMoveRefused {
    pub(crate) code: i32,
    pub(crate) at: std::time::Duration,
}

/// The frame a floating window's state says it should have.
#[derive(Component, Clone, Debug)]
pub(crate) struct FloatingGeometry {
    /// The authored frame: position and size the user asked for. A repair
    /// rewrites it only when no current display can hold it.
    pub(crate) frame: IRect,
    /// The display that held this frame when it was last authored or observed,
    /// with that display's usable viewport at the time. It is what lets a repair
    /// keep the window's relative position when that display is gone.
    pub(crate) anchor: Option<(u32, IRect)>,
    /// No display could be established at all. The intent is kept and nothing is
    /// derived from it, because a fact that cannot be read is unknown, not
    /// absence.
    pub(crate) unresolved: bool,
    pub(crate) repairs: Vec<FloatingRepair>,
    /// Bumped by every writer of the intent, and by a repair. The projection
    /// derives only when it has not derived this revision yet, so an observation
    /// adopted earlier in the same frame is not overwritten by a stale intent.
    revision: u64,
    derived: u64,
}

/// One repair of a floating frame, with the fact that required it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FloatingRepair {
    pub(crate) from: IRect,
    pub(crate) to: IRect,
    pub(crate) reason: &'static str,
}

impl FloatingGeometry {
    pub(crate) fn new(frame: IRect) -> Self {
        Self {
            frame,
            anchor: None,
            unresolved: false,
            repairs: Vec::new(),
            revision: 1,
            derived: 0,
        }
    }

    /// A frame authored on a known display, for writers that state an anchor
    /// directly (tests, and the adoption path when the display is in hand).
    #[cfg(test)]
    pub(crate) fn authored(frame: IRect, anchor: Option<(u32, IRect)>) -> Self {
        Self {
            anchor,
            ..Self::new(frame)
        }
    }

    /// Writes a new authored frame. Any writer of the intent comes through here.
    pub(crate) fn state(&mut self, frame: IRect) {
        self.frame = frame;
        self.unresolved = false;
        self.revision = self.revision.wrapping_add(1);
    }

    fn repair(&mut self, to: IRect, anchor: (u32, IRect), reason: &'static str) {
        self.repairs.push(FloatingRepair {
            from: self.frame,
            to,
            reason,
        });
        if self.repairs.len() > FLOATING_REPAIR_HISTORY {
            self.repairs.remove(0);
        }
        self.frame = to;
        self.anchor = Some(anchor);
        self.revision = self.revision.wrapping_add(1);
    }
}

/// States the frame a floating window should have.
///
/// Called by the floating edit commands and by the adoption of an external
/// gesture; neither publishes a frame write itself. The intent is created when
/// the window has none, so the first edit or observation is what gives a
/// floating window its retained frame.
pub(crate) fn set_floating_frame(commands: &mut Commands, entity: Entity, frame: IRect) {
    commands.queue(move |world: &mut World| {
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            return;
        };
        match entity_mut.get_mut::<FloatingGeometry>() {
            Some(mut geometry) => geometry.state(frame),
            None => {
                entity_mut.insert(FloatingGeometry::new(frame));
            }
        }
    });
}

type FloatingWindows<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Option<&'static mut FloatingGeometry>,
        &'static mut Position,
        &'static mut Bounds,
        Option<&'static DesiredWindowFrame>,
        Option<&'static PresentedWindowFrame>,
    ),
    With<Floating>,
>;

#[derive(SystemParam)]
pub(crate) struct FloatingGeometryCtx<'w, 's> {
    floating: FloatingWindows<'w, 's>,
    displays: Query<
        'w,
        's,
        (
            &'static Display,
            Option<&'static DockPosition>,
            Has<ActiveDisplayMarker>,
        ),
    >,
    topology: Res<'w, NativeTopology>,
    config: Res<'w, Config>,
    commands: Commands<'w, 's>,
}

/// Derives each floating window's frame from its intent, and repairs an intent no
/// current display can hold.
///
/// A frame that a display's usable viewport intersects is used as authored, apart
/// from being clamped inside that viewport — the approved order calls that clamp a
/// repair, so the intent records it rather than keeping an unreachable frame. A
/// frame no display intersects is repaired onto the display it belongs to when
/// that display still exists, otherwise onto the active display keeping the offset
/// it had on the display it was authored against. Neither repair writes to the
/// platform.
pub(crate) fn derive_floating_frames(
    FloatingGeometryCtx {
        mut floating,
        displays,
        topology,
        config,
        mut commands,
    }: FloatingGeometryCtx,
) {
    let mut viewports: Vec<(u32, IRect, bool)> = Vec::new();
    for (display, dock, active) in &displays {
        if let Some(bounds) = display.checked_actual_display_bounds(dock, &config) {
            viewports.push((display.id(), bounds, active));
        }
    }

    for (entity, geometry, mut position, mut bounds, desired, presented) in &mut floating {
        let Some(mut geometry) = geometry else {
            // A floating window the pipeline has never seen edited or moved
            // starts from the frame it has: the observation is the first intent,
            // and the next pass derives from it.
            let frame = desired.map_or_else(|| frame_at(position.0, bounds.0), |desired| desired.0);
            commands
                .entity(entity)
                .try_insert(FloatingGeometry::new(frame));
            continue;
        };
        // A display inventory that could not be read leaves the frame unknown
        // rather than absent: the intent is kept and nothing is derived from it.
        if !topology.is_complete() || viewports.is_empty() {
            geometry.unresolved = true;
            continue;
        }
        geometry.unresolved = false;

        // The display the frame was authored against is the one that decides:
        // while it exists, the frame is clamped inside it; only when it is gone
        // does the window move to another display.
        let anchored = geometry.anchor.and_then(|(id, _)| {
            viewports
                .iter()
                .find(|(viewport_id, _, _)| *viewport_id == id)
                .copied()
        });
        let held = viewports
            .iter()
            .find(|(_, viewport, _)| intersects(*viewport, geometry.frame))
            .copied();
        let (display_id, viewport, _) = anchored
            .or(held)
            .or_else(|| viewports.iter().find(|(_, _, active)| *active).copied())
            .unwrap_or(viewports[0]);

        let effective = if anchored.is_some() {
            clamped_into(geometry.frame, viewport)
        } else if let Some((_, former)) = geometry.anchor {
            // The display that held this frame is gone: keep the window's
            // position relative to the display it was authored against.
            clamped_into(shifted_into(geometry.frame, former, viewport), viewport)
        } else {
            clamped_into(geometry.frame, viewport)
        };

        let anchor = (display_id, viewport);
        if effective != geometry.frame {
            let reason = if anchored.is_some() {
                "clamped_to_viewport"
            } else {
                "display_gone"
            };
            geometry.repair(effective, anchor, reason);
        } else if geometry.anchor.map(|(id, _)| id) != Some(display_id) {
            geometry.anchor = Some(anchor);
        }

        if geometry.derived == geometry.revision {
            // This revision is already projected; an observation adopted earlier
            // in the frame owns the frame until the intent itself changes.
            continue;
        }
        geometry.derived = geometry.revision;
        let frame = geometry.frame;
        if position.0 != frame.min {
            position.0 = frame.min;
        }
        if bounds.0 != frame.size() {
            bounds.0 = frame.size();
        }
        let Ok(mut entity_commands) = commands.get_entity(entity) else {
            continue;
        };
        if desired.is_none_or(|desired| desired.0 != frame) {
            entity_commands.try_insert(DesiredWindowFrame(frame));
        }
        match presented {
            // A window that has never presented this frame starts there; one that
            // has animates to it, so a repair is visible rather than a jump.
            None => {
                entity_commands.try_insert(PresentedWindowFrame(frame));
            }
            Some(presented) if presented.0 != frame => {
                entity_commands.try_insert(WindowFrameMotion);
            }
            Some(_) => {}
        }
    }
}

/// Two usable viewports share a positive area. Half-open on every edge, matching
/// the display-ownership rule used elsewhere.
fn intersects(left: IRect, right: IRect) -> bool {
    left.min.x < right.max.x
        && right.min.x < left.max.x
        && left.min.y < right.max.y
        && right.min.y < left.max.y
}

fn clamped_into(frame: IRect, viewport: IRect) -> IRect {
    let origin = clamp_origin_to_viewport(frame.min, frame.size(), viewport);
    frame_at(origin, frame.size())
}

/// `IRect::new` takes corners; frames are authored as origin plus size.
fn frame_at(origin: IVec2, size: IVec2) -> IRect {
    IRect::new(origin.x, origin.y, origin.x + size.x, origin.y + size.y)
}

/// Keeps the frame's offset from `former` while moving it onto `target`.
fn shifted_into(frame: IRect, former: IRect, target: IRect) -> IRect {
    let offset = frame.min - former.min;
    frame_at(target.min + offset, frame.size())
}

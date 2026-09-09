//! Action-owned parking is separate from native hiding/minimization: retained
//! strip membership preserves columns, stacks and tabs while geometry moves.
use bevy::prelude::*;
use tracing::{debug, warn};

use super::focus::{FocusCoordinator, FocusSignal};
use super::layout::LayoutStrip;
use super::params::{ActiveDisplay, Windows};
use super::topology::NativeTopology;
use super::{
    Floating, Initializing, LayoutPosition, MissionControlActive, SpawnCommandsExt,
    WindowVisibility,
};
use crate::manager::WindowManager;
use crate::platform::WorkspaceId;

#[derive(Component)]
pub(crate) struct ParkedTile {
    space: WorkspaceId,
    restore_focus: bool,
    pub(crate) side: ParkingSide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParkingSide {
    Left,
    Right,
}

impl ParkingSide {
    fn nearest(frame: IRect, viewport: IRect) -> Self {
        let frame_center = i64::from(frame.min.x) + i64::from(frame.max.x);
        let viewport_center = i64::from(viewport.min.x) + i64::from(viewport.max.x);
        if frame_center <= viewport_center {
            Self::Left
        } else {
            Self::Right
        }
    }
}

fn parking_anchor(
    windows: &Windows,
    focus: &FocusCoordinator,
    strip: &LayoutStrip,
    viewport: IRect,
) -> Option<(usize, ParkingSide)> {
    let eligible = |entity| {
        strip.contains(entity)
            && windows
                .get_tracked(entity)
                .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
    };
    let entity = focus
        .snapshot()
        .confirmed_entity()
        .filter(|entity| eligible(*entity))
        .or_else(|| focus.last_tiled_matching(strip.id(), eligible))?;
    Some((
        strip.index_of(entity).ok()?,
        ParkingSide::nearest(windows.moving_frame(entity)?, viewport),
    ))
}

fn parking_side(
    index: usize,
    anchor: Option<(usize, ParkingSide)>,
    frame: IRect,
    viewport: IRect,
) -> ParkingSide {
    match anchor {
        Some((anchor_index, _)) if index < anchor_index => ParkingSide::Left,
        Some((anchor_index, _)) if index > anchor_index => ParkingSide::Right,
        Some((_, side)) => side,
        None => ParkingSide::nearest(frame, viewport),
    }
}

#[derive(Component)]
pub(crate) struct RestoringTile(bool);

#[allow(clippy::too_many_arguments)]
pub(crate) fn toggle(
    active: ActiveDisplay,
    windows: Windows,
    parked: Query<(Entity, &ParkedTile)>,
    topology: Res<NativeTopology>,
    manager: Res<WindowManager>,
    focus: Res<FocusCoordinator>,
    overview: Option<Res<MissionControlActive>>,
    initializing: Option<Res<Initializing>>,
    exiting: Option<Res<super::exit_restore::ExitInProgress>>,
    mut commands: Commands,
) {
    let space = active.active_strip().id();
    if initializing.is_some()
        || exiting.is_some()
        || overview.is_some_and(|overview| overview.0)
        || active.fullscreen().is_some()
        || topology.visible_space(active.display().id()) != Some(space)
    {
        return;
    }
    let Ok(membership) = topology.observe_memberships(&manager).inspect_err(|error| {
        warn!(%error, "unable to resolve Space for tiled visibility toggle");
    }) else {
        return;
    };
    let owned = parked
        .iter()
        .filter(|(_, parked)| parked.space == space)
        .collect::<Vec<_>>();
    if !owned.is_empty() {
        for (entity, parked) in owned {
            let Some((window, _, state)) = windows.get_tracked(entity) else {
                continue;
            };
            if !windows.layout_is_writable(entity)
                || membership.unique_space(window.id()) != Some(space)
            {
                continue;
            }
            commands
                .entity(entity)
                .insert(RestoringTile(
                    parked.restore_focus && state.is_tiled() && state.visibility().is_none(),
                ))
                .remove::<ParkedTile>();
            debug!(
                window_id = window.id(),
                space, "restoring action-parked tile"
            );
        }
        return;
    }

    let focused = focus.snapshot().confirmed_entity();
    let anchor = parking_anchor(&windows, &focus, active.active_strip(), active.bounds());
    let mut hid_focus = false;
    for entity in active.active_strip().all_windows() {
        let Some((window, _, state)) = windows.get_tracked(entity) else {
            continue;
        };
        if !state.is_tiled()
            || !state.is_visible()
            || !windows.layout_is_writable(entity)
            || membership.unique_space(window.id()) != Some(space)
            || window.try_is_full_screen().unwrap_or(true)
            || window.is_minimized()
        {
            continue;
        }
        let restore_focus = focused == Some(entity);
        let (Ok(index), Some(frame)) = (
            active.active_strip().index_of(entity),
            windows.moving_frame(entity),
        ) else {
            continue;
        };
        let side = parking_side(index, anchor, frame, active.bounds());
        hid_focus |= restore_focus;
        commands.entity(entity).insert(ParkedTile {
            space,
            restore_focus,
            side,
        });
        debug!(
            window_id = window.id(),
            space,
            ?side,
            "parking tile at screen edge"
        );
    }
    if hid_focus {
        commands.run_system_cached(crate::commands::command_focus_floating);
    }
}

/// Focus only after the normal physical target has replaced the parking target.
/// Otherwise focus's ensure-visible pass would scroll toward the old sliver.
pub(super) fn finish_restore(restoring: Query<(Entity, &RestoringTile)>, mut commands: Commands) {
    for (entity, restoring) in &restoring {
        commands.entity(entity).remove::<RestoringTile>();
        if restoring.0 {
            commands.focus_entity(entity, true);
        }
    }
}

pub(super) fn added(
    event: On<Add, ParkedTile>,
    mut positions: Query<&mut LayoutPosition>,
    mut focus: ResMut<FocusCoordinator>,
) {
    if let Ok(mut position) = positions.get_mut(event.entity) {
        position.set_changed();
    }
    focus.observe(FocusSignal::Invalidated {
        entity: event.entity,
    });
}

pub(super) fn removed(event: On<Remove, ParkedTile>, mut positions: Query<&mut LayoutPosition>) {
    if let Ok(mut position) = positions.get_mut(event.entity) {
        position.set_changed();
    }
}

/// Native lifecycle/layout transitions take ownership away from this action.
/// Entity generations prevent an old parking record from affecting a reused ID.
pub(super) fn release_detached(
    parked: Query<(Entity, &ParkedTile, Has<Floating>, Has<WindowVisibility>)>,
    strips: Query<&LayoutStrip>,
    mut commands: Commands,
) {
    for (entity, parked, floating, invisible) in &parked {
        if floating
            || invisible
            || !strips
                .iter()
                .any(|strip| strip.id() == parked.space && strip.contains(entity))
        {
            commands.entity(entity).remove::<ParkedTile>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parking_side_preserves_column_order_and_uses_nearest_edge_without_an_anchor() {
        let viewport = IRect::new(-1024, 0, 0, 768);
        let left = IRect::new(-900, 20, -700, 700);
        let right = IRect::new(-300, 20, -100, 700);
        let centered = IRect::new(-612, 20, -412, 700);
        assert_eq!(
            parking_side(0, Some((2, ParkingSide::Right)), right, viewport),
            ParkingSide::Left
        );
        assert_eq!(
            parking_side(4, Some((2, ParkingSide::Left)), left, viewport),
            ParkingSide::Right
        );
        assert_eq!(
            parking_side(2, Some((2, ParkingSide::Right)), left, viewport),
            ParkingSide::Right
        );
        assert_eq!(parking_side(0, None, left, viewport), ParkingSide::Left);
        assert_eq!(parking_side(0, None, right, viewport), ParkingSide::Right);
        assert_eq!(parking_side(0, None, centered, viewport), ParkingSide::Left);
    }
}

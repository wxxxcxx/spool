//! One tiled z-order policy shared by confirmed focus and explicit layer raises.

use std::cmp::Reverse;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use tracing::debug;

use super::FocusCoordinator;
use crate::ecs::exit_restore::ExitInProgress;
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::params::{ActiveDisplay, Windows};
use crate::ecs::topology::NativeTopology;
use crate::ecs::{Initializing, MissionControlActive, NativeFullscreenMarker, Scrolling};

#[derive(Debug, PartialEq, Eq)]
struct StackingPlan {
    strip: Entity,
    focus: Entity,
    bottom_to_top: Vec<Entity>,
}

#[derive(Resource, Default)]
pub(super) struct TiledStackingState {
    // This records requests, not proof that WindowServer accepted the order.
    last_requested: Option<StackingPlan>,
}

#[derive(SystemParam)]
pub(super) struct TiledStacking<'w, 's> {
    windows: Windows<'w, 's>,
    active: ActiveDisplay<'w, 's>,
    topology: Res<'w, NativeTopology>,
    mission_control: Res<'w, MissionControlActive>,
    initializing: Option<Res<'w, Initializing>>,
    exiting: Option<Res<'w, ExitInProgress>>,
    fullscreen: Query<'w, 's, (), With<NativeFullscreenMarker>>,
    scrolling: Query<'w, 's, &'static Scrolling>,
}

impl TiledStacking<'_, '_> {
    fn paused(&self) -> bool {
        self.initializing.is_some()
            || self.exiting.is_some()
            || self.mission_control.0
            || self.active.fullscreen().is_some()
            || self.active.active_strip().is_fullscreen()
            || self.topology.visible_space(self.active.display().id())
                != Some(self.active.active_strip().id())
            || self
                .scrolling
                .get(self.active.active_strip_entity())
                .is_ok_and(|scroll| scroll.is_user_swiping)
    }

    fn plan(&self, focus: Entity) -> Option<StackingPlan> {
        if self.paused() {
            return None;
        }
        Some(StackingPlan {
            strip: self.active.active_strip_entity(),
            focus,
            bottom_to_top: bottom_to_top(self.active.active_strip(), focus, |entity| {
                !self.fullscreen.contains(entity)
                    && self.windows.layout_is_writable(entity)
                    && self
                        .windows
                        .get_tracked(entity)
                        .is_some_and(|(_, _, state)| state.is_tiled() && state.is_visible())
            })?,
        })
    }

    pub(super) fn raise_one(&self, entity: Entity) {
        if self.initializing.is_some() || self.exiting.is_some() || self.mission_control.0 {
            return;
        }
        if let Some((window, _, state)) = self.windows.get_tracked(entity)
            && state.is_visible()
            && self.windows.layout_is_writable(entity)
            && window
                .represented_window_id()
                .is_ok_and(|id| id == window.id())
        {
            window.raise_without_focus();
        }
    }

    pub(super) fn raise_strip(&self, focus: Entity, state: &mut TiledStackingState, force: bool) {
        let Some(plan) = self.plan(focus) else {
            state.last_requested = None;
            return;
        };
        if !force && state.last_requested.as_ref() == Some(&plan) {
            return;
        }
        let ids = plan
            .bottom_to_top
            .iter()
            .filter_map(|entity| self.windows.get(*entity).map(|window| window.id()))
            .collect::<Vec<_>>();
        debug!(target: "spool::focus_diagnostics", space_id = self.active.active_strip().id(),
            ?ids, "tiled_stacking_requested_bottom_to_top");
        let started = tracing::enabled!(target: "spool::focus_diagnostics", tracing::Level::DEBUG)
            .then(std::time::Instant::now);
        for entity in &plan.bottom_to_top {
            self.raise_one(*entity);
        }
        if let Some(started) = started {
            debug!(target: "spool::focus_diagnostics", space_id = self.active.active_strip().id(),
                count = plan.bottom_to_top.len(), raise_us = started.elapsed().as_micros(),
                "tiled_stacking_completed");
        }
        state.last_requested = Some(plan);
    }
}

pub(super) fn reconcile_tiled_stacking(
    stacking: TiledStacking,
    focus: Res<FocusCoordinator>,
    mut state: ResMut<TiledStackingState>,
) {
    let snapshot = focus.snapshot();
    // Await native confirmation; a pending command or AX invalidation is not focus.
    if snapshot.requested.is_some() || snapshot.resolving.is_some() {
        return;
    }
    if let Some(entity) = snapshot.confirmed_entity() {
        stacking.raise_strip(entity, &mut state, false);
    } else {
        state.last_requested = None;
    }
}

fn selected_tab(tabs: &[Entity], focus: Entity) -> Option<Entity> {
    tabs.contains(&focus)
        .then_some(focus)
        .or_else(|| tabs.first().copied())
}

/// Only the selected native tab participates: raising dormant tabs can select them.
fn bottom_to_top(
    strip: &LayoutStrip,
    focus: Entity,
    mut eligible: impl FnMut(Entity) -> bool,
) -> Option<Vec<Entity>> {
    let mut columns = Vec::new();
    for column in strip.columns() {
        let mut members = Vec::new();
        match column {
            Column::Single(entity) => members.push(*entity),
            Column::Tabs(tabs) => members.extend(selected_tab(tabs, focus)),
            Column::Stack(items) => {
                for item in items.iter().rev() {
                    match item {
                        StackItem::Single(entity) => members.push(*entity),
                        StackItem::Tabs(tabs) => members.extend(selected_tab(tabs, focus)),
                    }
                }
            }
            Column::Fullscren(_) => {}
        }
        members.retain(|entity| eligible(*entity));
        if !members.is_empty() {
            columns.push((columns.len(), members));
        }
    }
    let anchor = columns
        .iter()
        .position(|(_, members)| members.contains(&focus))?;
    columns.sort_by_key(|(index, _)| (Reverse(index.abs_diff(anchor)), *index));
    let mut result = columns
        .into_iter()
        .flat_map(|(_, members)| members)
        .collect::<Vec<_>>();
    result.retain(|entity| *entity != focus);
    result.push(focus);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_count_as_one_column_and_only_selected_tabs_are_raised() {
        let mut world = World::new();
        let e = (0..7).map(|_| world.spawn_empty().id()).collect::<Vec<_>>();
        let mut strip = LayoutStrip::new(1);
        strip.append(e[0]);
        strip.append_column(Column::Stack(vec![
            StackItem::Single(e[1]),
            StackItem::Tabs(vec![e[2], e[3]]),
        ]));
        strip.append_column(Column::Tabs(vec![e[4], e[5]]));
        strip.append(e[6]);
        assert_eq!(
            bottom_to_top(&strip, e[4], |_| true),
            Some(vec![e[0], e[2], e[1], e[6], e[4]])
        );
        assert_eq!(
            bottom_to_top(&strip, e[5], |_| true),
            Some(vec![e[0], e[2], e[1], e[6], e[5]])
        );
        assert_eq!(
            bottom_to_top(&strip, e[3], |_| true),
            Some(vec![e[6], e[0], e[4], e[1], e[3]])
        );
        assert!(
            matches!(strip.columns().nth(2), Some(Column::Tabs(tabs)) if tabs == &vec![e[4], e[5]])
        );
        assert_eq!(
            bottom_to_top(&strip, e[0], |entity| entity != e[4]),
            Some(vec![e[6], e[2], e[1], e[0]]),
            "an unavailable tab leader must not promote a dormant tab"
        );
    }
}

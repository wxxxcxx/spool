//! Stack height intent is owned by independent frame items, never observations.
use bevy::ecs::entity::Entity;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::errors::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StackItemId(pub u64);

impl StackItemId {
    fn allocate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("runtime stack item identity exhausted"),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackItemState {
    pub id: StackItemId,
    /// Positive finite relative weight; 1 is the equal-share default.
    pub weight: f64,
    pub intent_revision: u64,
    pub members: Vec<Entity>,
}

impl StackItemState {
    pub(crate) fn new(members: Vec<Entity>) -> Self {
        Self {
            id: StackItemId::allocate(),
            weight: 1.0,
            intent_revision: 0,
            members,
        }
    }

    pub(crate) fn split(&self) -> Self {
        Self {
            id: StackItemId::allocate(),
            ..self.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightProjectionBlocked {
    UnknownViewport,
    InvalidIntent,
    InsufficientSpace,
    Unrepresentable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveStackHeight {
    /// Unconstrained normalized allocation in logical slot points.
    pub requested: f64,
    pub slot: i32,
    pub constrained: bool,
}

pub(crate) fn validate_weight(weight: f64) -> Result<()> {
    if weight.is_finite() && weight > 0.0 {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "stack height weight must be positive and finite".into(),
        ))
    }
}

pub(crate) fn normalized_weights(weights: &[f64]) -> Result<Vec<f64>> {
    for weight in weights {
        validate_weight(*weight)?;
    }
    let maximum = weights.iter().copied().fold(0.0_f64, f64::max);
    let scaled = weights
        .iter()
        .map(|weight| weight / maximum)
        .collect::<Vec<_>>();
    let total = scaled.iter().sum::<f64>();
    if !total.is_finite() || total <= 0.0 || scaled.iter().any(|weight| *weight <= 0.0) {
        return Err(Error::InvalidInput(
            "stack height proportions are not representable".into(),
        ));
    }
    Ok(scaled.into_iter().map(|weight| weight / total).collect())
}

/// The 200-point minimum is layout policy, not a discovered native constraint.
/// Single-item columns fill the viewport without imposing a stack minimum.
#[allow(
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    reason = "validated bounded allocations are rounded to exact integer slot coordinates"
)]
pub fn project_stack_heights(
    weights: &[f64],
    viewport: Option<i32>,
) -> std::result::Result<Vec<EffectiveStackHeight>, HeightProjectionBlocked> {
    use HeightProjectionBlocked as Blocked;
    let height = viewport
        .filter(|height| *height > 0)
        .ok_or(Blocked::UnknownViewport)?;
    if weights.is_empty() {
        return Ok(Vec::new());
    }
    let proportions = normalized_weights(weights).map_err(|_| Blocked::InvalidIntent)?;
    let count = i32::try_from(weights.len()).map_err(|_| Blocked::Unrepresentable)?;
    let minimum = if count == 1 { 1 } else { 200 };
    if count.checked_mul(minimum).ok_or(Blocked::Unrepresentable)? > height {
        return Err(Blocked::InsufficientSpace);
    }
    let requested = proportions
        .iter()
        .map(|weight| weight * f64::from(height))
        .collect::<Vec<_>>();
    let mut effective = vec![0.0; weights.len()];
    let mut remaining = f64::from(height);
    let mut active = (0..weights.len()).collect::<Vec<_>>();
    loop {
        let sum = active.iter().map(|index| proportions[*index]).sum::<f64>();
        let limited = active
            .iter()
            .copied()
            .filter(|index| remaining * proportions[*index] / sum < f64::from(minimum))
            .collect::<Vec<_>>();
        if limited.is_empty() {
            for index in active {
                effective[index] = remaining * proportions[index] / sum;
            }
            break;
        }
        for index in &limited {
            effective[*index] = f64::from(minimum);
            remaining -= f64::from(minimum);
        }
        active.retain(|index| !limited.contains(index));
        if active.is_empty() {
            break;
        }
    }
    let mut slots = effective
        .iter()
        .map(|value| value.floor() as i32)
        .collect::<Vec<_>>();
    let allocated = slots
        .iter()
        .try_fold(0_i32, |sum, value| sum.checked_add(*value))
        .ok_or(Blocked::Unrepresentable)?;
    let remainder = height
        .checked_sub(allocated)
        .ok_or(Blocked::Unrepresentable)?;
    let mut order = (0..slots.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        (effective[*right] - f64::from(slots[*right]))
            .total_cmp(&(effective[*left] - f64::from(slots[*left])))
            .then_with(|| left.cmp(right))
    });
    let remainder = usize::try_from(remainder).map_err(|_| Blocked::Unrepresentable)?;
    if remainder > order.len() {
        return Err(Blocked::Unrepresentable);
    }
    for index in order.into_iter().take(remainder) {
        slots[index] += 1;
    }
    Ok(requested
        .into_iter()
        .zip(effective)
        .zip(slots)
        .map(|((requested, effective), slot)| EffectiveStackHeight {
            requested,
            slot,
            constrained: (effective - requested).abs() > 0.000_001,
        })
        .collect())
}

impl super::LayoutStrip {
    /// Re-associate metadata by cohort membership after a structural mutation.
    /// A surviving independent item retains identity even when its first tab changes.
    pub(super) fn sync_height_items(&mut self) {
        let previous = self
            .column_states
            .iter()
            .flat_map(|state| state.height_items.iter())
            .cloned()
            .collect::<Vec<_>>();
        let cohorts = self
            .columns
            .iter()
            .map(|column| match column {
                super::Column::Stack(items) => items
                    .iter()
                    .map(|item| item.window_iter().collect())
                    .collect::<Vec<Vec<Entity>>>(),
                other => vec![other.window_iter().collect()],
            })
            .collect::<Vec<_>>();
        let split_ids = previous
            .iter()
            .filter(|old| {
                cohorts
                    .iter()
                    .flatten()
                    .filter(|members| members.iter().any(|member| old.members.contains(member)))
                    .count()
                    > 1
            })
            .map(|old| old.id)
            .collect::<std::collections::HashSet<_>>();
        let mut used = std::collections::HashSet::new();
        for (cohorts, state) in cohorts.into_iter().zip(&mut self.column_states) {
            state.height_items = cohorts
                .into_iter()
                .map(|members| {
                    let mut item = previous
                        .iter()
                        .find(|old| members.iter().any(|member| old.members.contains(member)))
                        .cloned()
                        .unwrap_or_else(|| StackItemState::new(members.clone()));
                    if split_ids.contains(&item.id) || !used.insert(item.id) {
                        item = item.split();
                        used.insert(item.id);
                    }
                    item.members = members;
                    item
                })
                .collect();
        }
    }

    pub fn height_state(&self, entity: Entity) -> Option<&StackItemState> {
        self.column_states
            .get(self.index_of(entity).ok()?)?
            .height_items
            .iter()
            .find(|item| item.members.contains(&entity))
    }

    pub fn column_height_items(&self, index: usize) -> Option<&[StackItemState]> {
        Some(&self.column_states.get(index)?.height_items)
    }

    pub fn height_context(&self, entity: Entity) -> Option<(StackItemId, u64, u64)> {
        let column = self.column_states.get(self.index_of(entity).ok()?)?;
        let item = column
            .height_items
            .iter()
            .find(|item| item.members.contains(&entity))?;
        Some((item.id, item.intent_revision, column.height_revision))
    }

    pub fn set_height_context(&mut self, viewport: Option<i32>) -> bool {
        if self.viewport_height == viewport {
            return false;
        }
        self.viewport_height = viewport;
        true
    }

    pub fn height_viewport(&self) -> Option<i32> {
        self.viewport_height
    }

    /// Projection over every retained item. Runtime and diagnostics must use
    /// [`Self::effective_stack_heights_for`]; this retained view exists only to
    /// assert that an ordered-out item keeps intent without reserving space.
    #[cfg(test)]
    pub fn effective_stack_heights(
        &self,
        index: usize,
        viewport: Option<i32>,
    ) -> std::result::Result<Vec<EffectiveStackHeight>, HeightProjectionBlocked> {
        let items = self
            .column_height_items(index)
            .ok_or(HeightProjectionBlocked::InvalidIntent)?;
        project_stack_heights(
            &items.iter().map(|item| item.weight).collect::<Vec<_>>(),
            viewport,
        )
    }

    pub fn effective_stack_heights_for(
        &self,
        index: usize,
        viewport: Option<i32>,
        eligible: &impl Fn(Entity) -> bool,
    ) -> std::result::Result<Vec<(StackItemId, EffectiveStackHeight)>, HeightProjectionBlocked>
    {
        let items = self
            .column_height_items(index)
            .ok_or(HeightProjectionBlocked::InvalidIntent)?
            .iter()
            .filter(|item| item.members.iter().any(|entity| eligible(*entity)))
            .collect::<Vec<_>>();
        let heights = project_stack_heights(
            &items.iter().map(|item| item.weight).collect::<Vec<_>>(),
            viewport,
        )?;
        Ok(items
            .into_iter()
            .zip(heights)
            .map(|(item, height)| (item.id, height))
            .collect())
    }

    #[cfg(test)]
    pub fn height_projection_is_blocked(&self) -> bool {
        self.height_projection_is_blocked_for(&|_| true)
    }

    pub fn height_projection_is_blocked_for(&self, eligible: &impl Fn(Entity) -> bool) -> bool {
        self.columns.iter().enumerate().any(|(index, _)| {
            self.effective_stack_heights_for(index, self.viewport_height, eligible)
                .is_err()
        })
    }

    pub fn set_height_weight(&mut self, entity: Entity, weight: f64) -> Result<bool> {
        validate_weight(weight)?;
        let index = self.index_of(entity)?;
        let mut weights = self.column_states[index]
            .height_items
            .iter()
            .map(|item| item.weight)
            .collect::<Vec<_>>();
        let position = self.column_states[index]
            .height_items
            .iter()
            .position(|item| item.members.contains(&entity))
            .ok_or_else(|| Error::NotFound("stack item identity no longer exists".into()))?;
        weights[position] = weight;
        self.replace_height_weights(index, &weights)
    }

    pub fn equalize_heights(&mut self, entity: Entity) -> Result<bool> {
        let index = self.index_of(entity)?;
        self.replace_height_weights(
            index,
            &vec![1.0; self.column_states[index].height_items.len()],
        )
    }

    /// Position of the item owning `entity` inside its column cohort.
    fn height_position(&self, index: usize, entity: Entity) -> Result<usize> {
        self.column_states[index]
            .height_items
            .iter()
            .position(|item| item.members.contains(&entity))
            .ok_or_else(|| Error::NotFound("stack item identity no longer exists".into()))
    }

    /// Eligible positions inside one column cohort. An item is eligible when at
    /// least one member currently participates in projection, matching the
    /// filter documented by [`Self::effective_stack_heights_for`].
    fn eligible_positions(&self, index: usize, eligible: &impl Fn(Entity) -> bool) -> Vec<usize> {
        self.column_states[index]
            .height_items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.members.iter().any(|entity| eligible(*entity)))
            .map(|(position, _)| position)
            .collect()
    }

    /// Replace the cohort weights needed to honor `points` for one target
    /// position, redistributing `1 - fraction` over `yield_positions` only.
    /// Excluded items keep their exact raw weight, and the edited cohort keeps
    /// its exact total weight units so a rejoining sibling resumes its share.
    fn replace_eligible_points(
        &mut self,
        index: usize,
        target: usize,
        points: f64,
        viewport: i32,
        yield_positions: &[usize],
    ) -> Result<bool> {
        let fraction = points / f64::from(viewport);
        if !fraction.is_finite() || fraction <= 0.0 || fraction >= 1.0 {
            return Err(Error::InvalidInput(
                "stack item height must be inside the viewport".into(),
            ));
        }
        let cohort = {
            let mut cohort = yield_positions.to_vec();
            cohort.push(target);
            cohort
        };
        let items = &self.column_states[index].height_items;
        let normalized = normalized_weights(
            &cohort
                .iter()
                .map(|at| items[*at].weight)
                .collect::<Vec<_>>(),
        )?;
        let mut raw = vec![0.0_f64; items.len()];
        for (ordinal, at) in cohort.iter().enumerate() {
            raw[*at] = normalized[ordinal];
        }
        let yielded = yield_positions.iter().map(|at| raw[*at]).sum::<f64>();
        if !yielded.is_finite() || yielded <= 0.0 {
            return Err(Error::InvalidInput(
                "stack height shares are not representable".into(),
            ));
        }
        // `fraction` and the yields are shares of the viewport, so the new
        // cohort proportions sum to exactly 1. Re-expressing them in the
        // cohort's existing weight units therefore preserves that total, which
        // is what lets a default-1 item joining later resume its relative
        // share. Every excluded item keeps its exact raw weight.
        let units = cohort.iter().map(|at| items[*at].weight).sum::<f64>();
        if !units.is_finite() || units <= 0.0 {
            return Err(Error::InvalidInput(
                "stack height weight units are not representable".into(),
            ));
        }
        let shares = cohort
            .iter()
            .map(|at| {
                if *at == target {
                    fraction
                } else {
                    raw[*at] / yielded * (1.0 - fraction)
                }
            })
            .collect::<Vec<_>>();
        let mut next = items.iter().map(|item| item.weight).collect::<Vec<_>>();
        for (share, at) in shares.iter().zip(&cohort) {
            next[*at] = share * units;
        }
        self.replace_height_weights(index, &next)
    }

    /// Reject heights outside the viewport before any weight arithmetic runs.
    fn viewport_fraction(points: f64, viewport_height: Option<i32>) -> Result<(f64, i32)> {
        let viewport = viewport_height
            .filter(|height| *height > 0)
            .ok_or_else(|| Error::InvalidInput("stack height viewport is unknown".into()))?;
        let fraction = points / f64::from(viewport);
        if !fraction.is_finite() || fraction <= 0.0 || fraction >= 1.0 {
            return Err(Error::InvalidInput(
                "stack item height must be inside the viewport".into(),
            ));
        }
        Ok((fraction, viewport))
    }

    /// Adjust raw shares, never the rounded/constrained or observed frame.
    pub fn resize_height(
        &mut self,
        entity: Entity,
        delta_points: f64,
        viewport_height: Option<i32>,
    ) -> Result<bool> {
        self.resize_height_for(entity, delta_points, viewport_height, &|_| true)
    }

    /// [`Self::resize_height`] restricted to the cohort that currently
    /// participates in projection. Retained-but-ordered-out items neither move
    /// nor reserve space.
    pub fn resize_height_for(
        &mut self,
        entity: Entity,
        delta_points: f64,
        viewport_height: Option<i32>,
        eligible: &impl Fn(Entity) -> bool,
    ) -> Result<bool> {
        if !delta_points.is_finite() {
            return Err(Error::InvalidInput("height delta must be finite".into()));
        }
        let viewport = viewport_height
            .filter(|height| *height > 0)
            .ok_or_else(|| Error::InvalidInput("stack height viewport is unknown".into()))?;
        let index = self.index_of(entity)?;
        let positions = self.eligible_positions(index, eligible);
        let position = self.height_position(index, entity)?;
        let Some(ordinal) = positions
            .iter()
            .position(|candidate| *candidate == position)
        else {
            return Ok(false);
        };
        if positions.len() < 2 {
            return Ok(false);
        }
        let normalized = normalized_weights(
            &positions
                .iter()
                .map(|position| self.column_states[index].height_items[*position].weight)
                .collect::<Vec<_>>(),
        )?;
        let maximum = f64::from(viewport)
            - f64::from(
                i32::try_from(positions.len() - 1)
                    .map_err(|_| Error::InvalidInput("too many stack items".into()))?,
            );
        if maximum < 1.0 {
            return Err(Error::InvalidInput(
                "stack height viewport is too small".into(),
            ));
        }
        let points = (normalized[ordinal] * f64::from(viewport) + delta_points).clamp(1.0, maximum);
        if (points - normalized[ordinal] * f64::from(viewport)).abs()
            <= f64::EPSILON * f64::from(viewport)
        {
            return Ok(false);
        }
        let yielded = positions
            .iter()
            .copied()
            .filter(|candidate| *candidate != position)
            .collect::<Vec<_>>();
        self.replace_eligible_points(index, position, points, viewport, &yielded)
    }

    /// Accept an evidenced user height by editing this item and sibling shares
    /// restricted to the cohort that currently participates in projection.
    /// Excluded siblings keep their exact raw weight, and the participating
    /// cohort keeps its exact total weight units, so a later reappearance
    /// resumes its intended share.
    pub fn adopt_height_for(
        &mut self,
        entity: Entity,
        points: f64,
        viewport_height: Option<i32>,
        eligible: &impl Fn(Entity) -> bool,
    ) -> Result<bool> {
        let (_, viewport) = Self::viewport_fraction(points, viewport_height)?;
        let index = self.index_of(entity)?;
        let positions = self.eligible_positions(index, eligible);
        let position = self.height_position(index, entity)?;
        if !positions.contains(&position) || positions.len() < 2 {
            return Ok(false);
        }
        let yielded = positions
            .iter()
            .copied()
            .filter(|candidate| *candidate != position)
            .collect::<Vec<_>>();
        self.replace_eligible_points(index, position, points, viewport, &yielded)
    }

    /// Honor an evidenced height for `entity` by taking the whole remainder
    /// from its immediate predecessor in the participating cohort. An external
    /// top-edge resize moves that neighbour's boundary, so redistributing
    /// across every sibling would silently re-share untouched items.
    pub fn adopt_height_from_previous_for(
        &mut self,
        entity: Entity,
        points: f64,
        viewport_height: Option<i32>,
        eligible: &impl Fn(Entity) -> bool,
    ) -> Result<bool> {
        let (_, viewport) = Self::viewport_fraction(points, viewport_height)?;
        let index = self.index_of(entity)?;
        let positions = self.eligible_positions(index, eligible);
        let position = self.height_position(index, entity)?;
        let Some(ordinal) = positions
            .iter()
            .position(|candidate| *candidate == position)
        else {
            return Ok(false);
        };
        if ordinal == 0 {
            return Ok(false);
        }
        let donor = vec![positions[ordinal - 1]];
        self.replace_eligible_points(index, position, points, viewport, &donor)
    }

    #[allow(
        clippy::float_cmp,
        reason = "same-value raw intent edits must not advance revision"
    )]
    fn replace_height_weights(&mut self, index: usize, weights: &[f64]) -> Result<bool> {
        normalized_weights(weights)?;
        let state = &mut self.column_states[index];
        if state
            .height_items
            .iter()
            .zip(weights)
            .all(|(item, weight)| item.weight == *weight)
        {
            return Ok(false);
        }
        let next = state
            .height_revision
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("stack height revision exhausted".into()))?;
        if state
            .height_items
            .iter()
            .zip(weights)
            .any(|(item, weight)| item.weight != *weight && item.intent_revision == u64::MAX)
        {
            return Err(Error::InvalidInput("stack item revision exhausted".into()));
        }
        for (item, weight) in state.height_items.iter_mut().zip(weights) {
            if item.weight != *weight {
                item.weight = *weight;
                item.intent_revision += 1;
            }
        }
        state.height_revision = next;
        Ok(true)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "tests verify exact preservation of raw weight values"
)]
mod tests {
    use super::*;
    use crate::ecs::layout::{Column, LayoutStrip, StackItem, WidthIntent};
    use bevy::ecs::world::World;
    use bevy::math::IRect;

    fn stacked(count: usize) -> (LayoutStrip, Vec<Entity>) {
        let mut world = World::new();
        let entities = (0..count)
            .map(|_| world.spawn_empty().id())
            .collect::<Vec<_>>();
        let mut strip = LayoutStrip::new(7);
        for entity in &entities {
            strip.append(*entity);
        }
        for entity in entities.iter().skip(1) {
            strip.stack(*entity).unwrap();
        }
        strip.set_width_context(Some(1200), WidthIntent::Absolute(500.0));
        strip.set_height_context(Some(1000));
        (strip, entities)
    }

    #[test]
    fn weights_project_with_exact_sum_and_layout_minimum_without_changing_raw_intent() {
        for viewport in [600, 601, 1000, 2000, i32::MAX] {
            let heights = project_stack_heights(&[1.0, 2.0, 7.0], Some(viewport)).unwrap();
            assert_eq!(
                heights.iter().map(|height| height.slot).sum::<i32>(),
                viewport
            );
            assert!(heights.iter().all(|height| height.slot >= 200));
        }
        let limited = project_stack_heights(&[1.0, 9.0], Some(1000)).unwrap();
        assert_eq!(
            limited.iter().map(|height| height.slot).collect::<Vec<_>>(),
            [200, 800]
        );
        assert!(limited.iter().all(|height| height.constrained));
        assert!((limited[0].requested - 100.0).abs() < 0.000_001);
        assert_eq!(
            project_stack_heights(&[1.0, 1.0, 1.0], Some(601))
                .unwrap()
                .iter()
                .map(|height| height.slot)
                .collect::<Vec<_>>(),
            [201, 200, 200]
        );
        assert_eq!(
            project_stack_heights(&[1.0, 9.0], Some(399)),
            Err(HeightProjectionBlocked::InsufficientSpace)
        );
        assert_eq!(
            project_stack_heights(&[1.0], Some(100)).unwrap()[0].slot,
            100
        );
        assert_eq!(
            project_stack_heights(&[1.0, 1.0], None),
            Err(HeightProjectionBlocked::UnknownViewport)
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                project_stack_heights(&[bad, 1.0], Some(1000)),
                Err(HeightProjectionBlocked::InvalidIntent)
            );
        }
        assert!(project_stack_heights(&[f64::MAX, f64::MAX], Some(1000)).is_ok());
    }

    #[test]
    fn last_item_resize_is_effective_and_uses_raw_shares_when_constrained() {
        let (mut strip, entities) = stacked(2);
        strip.set_height_weight(entities[1], 9.0).unwrap();
        let before = strip.height_context(entities[1]).unwrap();
        strip
            .resize_height(entities[1], -100.0, Some(1000))
            .unwrap();
        // Raw 900 decreases to 800; using effective 800 would wrongly give 700.
        let heights = strip.effective_stack_heights(0, Some(1000)).unwrap();
        assert!((heights[1].requested - 800.0).abs() < 0.000_001);
        assert_ne!(strip.height_context(entities[1]).unwrap(), before);
        let edited = strip.height_context(entities[1]).unwrap();
        let weight = strip.height_state(entities[1]).unwrap().weight;
        assert!(!strip.set_height_weight(entities[1], weight).unwrap());
        assert_eq!(strip.height_context(entities[1]).unwrap(), edited);
        assert!(strip.resize_height(entities[1], 50.0, None).is_err());
        assert!(strip.equalize_heights(entities[1]).unwrap());
        assert!(
            strip
                .column_height_items(0)
                .unwrap()
                .iter()
                .all(|item| item.weight == 1.0)
        );
    }

    #[test]
    fn relative_resize_preserves_weight_units_for_later_default_item_join() {
        let (mut strip, entities) = stacked(2);
        strip.resize_height(entities[0], 100.0, Some(1000)).unwrap();
        let total = strip
            .column_height_items(0)
            .unwrap()
            .iter()
            .map(|item| item.weight)
            .sum::<f64>();
        assert!((total - 2.0).abs() < 0.000_001);
        let new = Entity::from_bits(123);
        strip.append(new);
        strip.stack(new).unwrap();
        let last = strip.effective_stack_heights(0, Some(1200)).unwrap()[2];
        assert!((last.requested - 400.0).abs() < 0.000_001);
    }

    #[test]
    fn participant_cohort_edits_preserve_excluded_raw_weights_and_total_units() {
        let (mut strip, entities) = stacked(4);
        // B is retained but ordered out, so only A and C participate (1000 -> 500/500).
        let participating = [entities[0], entities[2]]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let eligible = |entity: Entity| participating.contains(&entity);
        let viewport = strip.height_viewport();
        let before = strip
            .effective_stack_heights_for(0, viewport, &eligible)
            .unwrap();
        assert_eq!(
            before
                .iter()
                .map(|(_, height)| height.slot)
                .collect::<Vec<_>>(),
            [500, 500]
        );
        // Dragging C's bottom edge must not readmit B; B keeps weight 1.0 exactly.
        strip
            .adopt_height_for(entities[2], 700.0, viewport, &eligible)
            .unwrap();
        let weights = strip
            .column_height_items(0)
            .unwrap()
            .iter()
            .map(|item| item.weight)
            .collect::<Vec<_>>();
        assert_eq!(weights[1].to_bits(), 1.0_f64.to_bits());
        assert_eq!(weights[3].to_bits(), 1.0_f64.to_bits());
        assert!((weights[0] + weights[2] - 2.0).abs() < 0.000_001);
        let after_effective = strip
            .effective_stack_heights_for(0, viewport, &eligible)
            .unwrap();
        assert_eq!(
            after_effective
                .iter()
                .map(|(_, height)| height.slot)
                .collect::<Vec<_>>(),
            [300, 700]
        );
        let retained = strip.effective_stack_heights(0, viewport).unwrap();
        assert_ne!(
            retained
                .iter()
                .map(|height| height.slot)
                .collect::<Vec<_>>(),
            [300, 700]
        );
    }

    #[test]
    fn participant_resize_moves_only_eligible_siblings_and_reports_nonparticipants() {
        let (mut strip, entities) = stacked(4);
        let participating = [entities[0], entities[2]]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let eligible = |entity: Entity| participating.contains(&entity);
        let viewport = strip.height_viewport();
        strip
            .resize_height_for(entities[2], 200.0, viewport, &eligible)
            .unwrap();
        let effective = strip
            .effective_stack_heights_for(0, viewport, &eligible)
            .unwrap();
        assert_eq!(
            effective
                .iter()
                .map(|(_, height)| height.slot)
                .collect::<Vec<_>>(),
            [300, 700]
        );
        let eligible_ids = participating
            .iter()
            .map(|entity| strip.height_state(*entity).unwrap().id)
            .collect::<Vec<_>>();
        let excluded = strip
            .column_height_items(0)
            .unwrap()
            .iter()
            .filter(|item| !eligible_ids.contains(&item.id))
            .all(|item| item.weight == 1.0);
        assert!(excluded);
        // An ordered-out item cannot be edited through the participant cohort.
        assert!(
            !strip
                .resize_height_for(entities[1], 100.0, viewport, &eligible)
                .unwrap()
        );
        assert!(
            !strip
                .adopt_height_for(entities[1], 400.0, viewport, &eligible)
                .unwrap()
        );
    }

    #[test]
    fn weighted_participant_cohort_redistributes_proportionally_among_eligible_siblings() {
        let (mut strip, entities) = stacked(4);
        strip.set_height_weight(entities[0], 3.0).unwrap();
        // B stays ordered out; A and C participate with raw 3:1.
        let participating = [entities[0], entities[2]]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let eligible = |entity: Entity| participating.contains(&entity);
        let viewport = strip.height_viewport();
        strip
            .adopt_height_for(entities[2], 400.0, viewport, &eligible)
            .unwrap();
        let effective = strip
            .effective_stack_heights_for(0, viewport, &eligible)
            .unwrap();
        assert_eq!(
            effective
                .iter()
                .map(|(_, height)| height.slot)
                .collect::<Vec<_>>(),
            [600, 400]
        );
        assert_eq!(
            strip.height_state(entities[1]).unwrap().weight.to_bits(),
            1.0_f64.to_bits()
        );
    }

    #[test]
    fn geometry_observations_do_not_own_height_and_unavailable_items_reserve_no_space() {
        let (mut strip, entities) = stacked(3);
        strip.set_height_weight(entities[0], 2.0).unwrap();
        let first = strip
            .relative_positions(1000, &|_| Some(IRect::new(0, 0, 500, 800)))
            .collect::<Vec<_>>();
        let second = strip
            .relative_positions(1000, &|_| Some(IRect::new(0, 0, 500, 222)))
            .collect::<Vec<_>>();
        assert_eq!(first, second);
        let only = strip
            .relative_positions(100, &|entity| {
                (entity == entities[0]).then_some(IRect::new(0, 0, 500, 800))
            })
            .collect::<Vec<_>>();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].1.height(), 100);
        assert_eq!(strip.height_state(entities[0]).unwrap().weight, 2.0);
        assert_eq!(
            strip
                .relative_positions(599, &|_| Some(IRect::new(0, 0, 500, 500)))
                .count(),
            0
        );
    }

    #[test]
    fn item_identity_and_weight_follow_stack_unstack_swap_and_tabs() {
        let (mut strip, entities) = stacked(3);
        strip.set_height_weight(entities[1], 3.0).unwrap();
        let item = strip.height_state(entities[1]).unwrap().clone();
        strip.unstack(entities[1]).unwrap();
        assert_eq!(strip.height_state(entities[1]).unwrap(), &item);
        strip.stack(entities[1]).unwrap();
        assert_eq!(strip.height_state(entities[1]).unwrap(), &item);
        strip.edit_column(0, |column| {
            if let Column::Stack(items) = column {
                items.swap(0, 1);
            }
        });
        assert_eq!(strip.height_state(entities[1]).unwrap(), &item);
        let follower = Entity::from_bits(123);
        strip.convert_to_tabs(entities[1], follower).unwrap();
        let tab_id = strip.height_state(entities[1]).unwrap().id;
        assert_eq!(strip.height_state(follower).unwrap().id, tab_id);
        assert_eq!(tab_id, item.id);
        strip.remove(entities[1]);
        assert_eq!(strip.height_state(follower).unwrap().id, tab_id);
        assert_eq!(strip.height_state(follower).unwrap().weight, 3.0);
    }

    #[test]
    fn partial_native_cohort_split_gets_independent_item_identities_with_raw_weight() {
        let (mut strip, entities) = stacked(2);
        strip.edit_column(0, |column| *column = Column::Tabs(entities.clone()));
        strip.set_height_weight(entities[0], 3.0).unwrap();
        let old = strip.height_state(entities[0]).unwrap().id;
        let extracted =
            strip.take_windows_preserving_layout(&std::collections::HashSet::from([entities[0]]));
        let a = extracted.height_state(entities[0]).unwrap();
        let b = strip.height_state(entities[1]).unwrap();
        assert_ne!(a.id, old);
        assert_ne!(b.id, old);
        assert_ne!(a.id, b.id);
        assert_eq!(a.weight, 3.0);
        assert_eq!(b.weight, 3.0);
    }

    #[test]
    fn tab_regrouping_preserves_shared_state_and_exact_frames() {
        let (mut strip, entities) = stacked(3);
        strip.set_height_weight(entities[0], 2.0).unwrap();
        let id = strip.height_state(entities[0]).unwrap().id;
        strip.edit_column(0, |column| {
            *column = Column::Stack(vec![
                StackItem::Tabs(vec![entities[0], entities[1]]),
                StackItem::Single(entities[2]),
            ]);
        });
        assert_eq!(strip.height_state(entities[1]).unwrap().id, id);
        let out = strip
            .relative_positions(1000, &|_| Some(IRect::new(0, 0, 500, 100)))
            .collect::<Vec<_>>();
        assert_eq!(out[0].1, out[1].1);
        assert_eq!(out[0].1.height() + out[2].1.height(), 1000);
    }
}

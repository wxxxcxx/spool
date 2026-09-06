//! Retry admission for complete window-default transactions, including reads.

use std::collections::HashMap;
use std::time::Duration;

use bevy::ecs::change_detection::DetectChanges as _;
use bevy::ecs::entity::Entity;
use bevy::ecs::query::{Has, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Query, Res, ResMut};
use bevy::math::IRect;

use super::reconcile::WindowUnavailable;
use super::topology::NativeTopology;
use super::workspace::WindowSpaceReassignmentPending;
use super::{
    DockPosition, FullscreenDefaultsDeferred, WindowDefaultsApplied, WindowDefaultsPending,
};
use crate::config::Config;
use crate::manager::{Display, Window};
use crate::platform::{WindowIncarnation, WorkspaceId};

const FAST_ATTEMPTS: u8 = 3;
const COOLDOWN: Duration = Duration::from_secs(5);

struct AttemptBudget {
    incarnation: WindowIncarnation,
    attempts: u8,
    retry_at: Duration,
}

type DisplayContext = (u32, IRect, Vec<WorkspaceId>);

#[derive(Resource, Default)]
pub(crate) struct DefaultRetries {
    attempts: HashMap<Entity, AttemptBudget>,
    topology: Option<Vec<DisplayContext>>,
}

impl DefaultRetries {
    pub(crate) fn admit(
        &mut self,
        entity: Entity,
        incarnation: WindowIncarnation,
        now: Duration,
    ) -> bool {
        let budget = self.attempts.entry(entity).or_insert(AttemptBudget {
            incarnation,
            attempts: 0,
            retry_at: Duration::ZERO,
        });
        if budget.incarnation != incarnation || now >= budget.retry_at {
            budget.incarnation = incarnation;
            budget.attempts = 0;
        }
        if budget.attempts >= FAST_ATTEMPTS {
            return false;
        }
        budget.attempts += 1;
        budget.retry_at = now.saturating_add(COOLDOWN);
        true
    }
}

type PendingDefaults<'w, 's> = Query<
    'w,
    's,
    (
        &'static Window,
        Has<WindowDefaultsApplied>,
        Has<WindowUnavailable>,
        Has<WindowSpaceReassignmentPending>,
        Has<FullscreenDefaultsDeferred>,
    ),
    With<WindowDefaultsPending>,
>;

/// Sampling epochs are not retry epochs: only a changed successful projection
/// or configuration invalidates the shared context. Blocked/dead entries retire
/// independently so resuming one window cannot reset another window's budget.
pub(crate) fn refresh_default_retries(
    mut retries: ResMut<DefaultRetries>,
    config: Res<Config>,
    topology: Res<NativeTopology>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    windows: PendingDefaults,
) {
    if windows.is_empty() {
        retries.attempts.clear();
        return;
    }
    if config.is_changed() {
        retries.attempts.clear();
    }
    if topology.is_complete() {
        let mut projection = topology
            .known_displays()
            .map(|(display, spaces)| {
                let (projected, dock) = displays
                    .iter()
                    .find(|(candidate, _)| candidate.id() == display.id())?;
                Some((
                    display.id(),
                    projected.actual_display_bounds(dock, &config),
                    spaces.to_vec(),
                ))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(projection) = projection.as_mut() {
            projection.sort_by_key(|entry| entry.0);
        }
        if projection.is_some() && projection != retries.topology {
            retries.attempts.clear();
            retries.topology = projection;
        }
    }
    retries.attempts.retain(|entity, budget| {
        windows
            .get(*entity)
            .is_ok_and(|(window, applied, unavailable, migrating, fullscreen)| {
                !applied
                    && !unavailable
                    && !migrating
                    && !fullscreen
                    && window.incarnation() == budget.incarnation
            })
    });
}

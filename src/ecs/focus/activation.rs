//! Bounded, read-only activation confirmation. Notifications alone never yield.
use super::{
    ActivationEffects, ActivationOutcome, Commands, Config, Duration, Entity, Event,
    FocusCoordinator, FocusObservation, FocusRequestKind, ObservedFocus, Pid, Res, ResMut,
    SendMessageTrigger, Time, WinID, WindowIncarnation, WorkspaceId,
};

// Engineering defaults for this coordinator, not native causality guarantees.
const PROBES: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(5),
];

impl FocusCoordinator {
    fn accept_activation_readback(
        &mut self,
        version: u64,
        identity: Option<(Pid, WinID, WindowIncarnation)>,
        now: Duration,
    ) {
        let Some(request) = &mut self.requested else {
            return;
        };
        if request.version != version || !request.pending() {
            return;
        }
        let Some((pid, window_id, incarnation)) = identity else {
            request.conflict = None;
            return;
        };
        if (pid, window_id, incarnation) == (request.pid, request.window_id, request.incarnation) {
            request.outcome = ActivationOutcome::Confirmed;
            request.conflict = None;
            return;
        }
        if self
            .uncertain_effects
            .contains(&(pid, window_id, incarnation))
        {
            request.conflict = None;
            return;
        }
        if request
            .conflict
            .is_some_and(|(old_pid, old_id, old_incarnation, since)| {
                (old_pid, old_id, old_incarnation) == (pid, window_id, incarnation)
                    && now.saturating_sub(since) >= Duration::from_millis(250)
            })
        {
            request.outcome = ActivationOutcome::Yielded;
            request.conflict = None;
            // A yielded preference remains retained, but it no longer owns
            // navigation over freshly confirmed Space-local history.
            if let Some(memory) = self.by_workspace.get_mut(&request.workspace) {
                memory.selection = memory.any.last();
            }
        } else {
            request.conflict = Some((pid, window_id, incarnation, now));
        }
    }

    pub(crate) fn activation_diagnostics(&self) -> serde_json::Value {
        use serde_json::json;
        json!({
            "observation_generation": self.generation,
            "actual": match self.observed {
                ObservedFocus::Tracked { window_id, .. } => json!({"kind":"tracked","window_id":window_id}),
                ObservedFocus::Untracked { pid, window_id } => json!({"kind":"untracked","pid":pid,"window_id":window_id}),
                ObservedFocus::Unresolved => json!({"kind":"unknown"}),
            },
            "activation": self.requested.map(|request| json!({
                "version":request.version,"window_id":request.window_id,"pid":request.pid,
                "incarnation":request.incarnation,"space_id":request.workspace,
                "source":match request.kind {FocusRequestKind::Explicit=>"explicit",FocusRequestKind::Automatic=>"automatic"},
                "attempted":request.submitted.is_some(),"submission_failed":request.submission_failed,"blocker":request.blocker,
                "outcome":match request.outcome {
                    ActivationOutcome::Accepted=>"accepted", ActivationOutcome::Blocked=>"blocked",
                    ActivationOutcome::Unconfirmed=>"unconfirmed", ActivationOutcome::Confirmed=>"confirmed",
                    ActivationOutcome::Yielded=>"yielded", ActivationOutcome::Invalidated=>"invalidated",
                },
                "readbacks":request.probes,"readback_budget_exhausted":request.probes>=PROBES.len(),
            })),
        })
    }

    pub(crate) fn preference_entity(&self, workspace: WorkspaceId) -> Option<Entity> {
        self.by_workspace.get(&workspace)?.preference
    }
}

pub(crate) fn verify_activation(
    mut focus: ResMut<FocusCoordinator>,
    mut effects: ActivationEffects,
    config: Res<Config>,
    time: Res<Time>,
    mut commands: Commands,
) {
    let Some(request) = focus
        .requested
        .filter(|request| request.pending() && request.submitted.is_some())
    else {
        return;
    };
    let Some(submitted) = request.submitted else {
        return;
    };
    let Some(delay) = PROBES.get(request.probes) else {
        return;
    };
    if time.elapsed().saturating_sub(submitted) < *delay {
        return;
    }
    if let Some(current) = &mut focus.requested {
        current.probes += 1;
    }
    // A visible topology snapshot alone does not establish the absence of a
    // transition. Pending target transfers and Mission Control veto evidence.
    let evidence = if effects.mission_control.0
        || effects.transactions.focus_transition_pending()
        || effects.windows.layout_transition_pending(request.entity)
    {
        None
    } else {
        read_focus_identity(&mut effects, &config)
    };
    focus.accept_activation_readback(request.version, evidence, time.elapsed());
    if let Some((pid, window_id, incarnation)) = evidence {
        // Route actual focus through the existing observation/history/marker
        // writer, including independent native revalidation at consumption.
        commands.trigger(SendMessageTrigger(Event::WindowFocused(FocusObservation {
            window_id,
            pid: Some(pid),
            incarnation: Some(incarnation),
            source: crate::events::FocusSource::StateSync,
            generation: None,
        })));
    }
}

fn read_focus_identity(
    effects: &mut ActivationEffects,
    config: &Config,
) -> Option<(Pid, WinID, WindowIncarnation)> {
    let mut front = effects.apps.iter().filter(|app| app.is_frontmost());
    let app = front.next()?;
    if front.next().is_some() {
        return None;
    }
    let window_id = app.focused_window_id().ok()?;
    let inventory = app.window_inventory(config).ok()?;
    if !inventory.complete {
        return None;
    }
    let mut identities = inventory
        .identities
        .iter()
        .filter(|(id, _)| *id == window_id);
    let (_, incarnation) = *identities.next()?;
    if identities.any(|(_, other)| *other != incarnation) {
        return None;
    }
    // Recheck both ends of the application/window evidence after inventory.
    if !app.is_frontmost() || app.focused_window_id().ok()? != window_id {
        return None;
    }
    effects
        .topology
        .observe_visible_window_space(&effects.manager, window_id)?;
    if !app.is_frontmost() || app.focused_window_id().ok()? != window_id {
        return None;
    }
    Some((app.pid(), window_id, incarnation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::focus::FocusSignal;
    use bevy::ecs::world::World;

    fn requested() -> (FocusCoordinator, Entity, Entity) {
        let mut world = World::new();
        let a = world.spawn(()).id();
        let b = world.spawn(()).id();
        let mut focus = FocusCoordinator::default();
        focus.request(a);
        (focus, a, b)
    }

    #[test]
    fn activation_unknown_and_competing_notification_are_not_terminal() {
        let (mut focus, a, _) = requested();
        focus.observe(FocusSignal::Untracked {
            generation: None,
            pid: Some(20),
            window_id: Some(30),
        });
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
        focus.accept_activation_readback(1, None, Duration::from_secs(1));
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
    }

    #[test]
    fn activation_stable_competition_yields_without_forging_success() {
        let (mut focus, a, _) = requested();
        let b = Some((20, 30, 40));
        focus.accept_activation_readback(1, b, Duration::from_millis(250));
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
        focus.accept_activation_readback(1, b, Duration::from_secs(1));
        assert_eq!(focus.snapshot().requested_entity(), None);
        assert_eq!(
            focus.activation_diagnostics()["activation"]["outcome"],
            "yielded"
        );
        focus.accept_activation_readback(1, Some((0, 0, 0)), Duration::from_secs(5));
        assert_eq!(
            focus.activation_diagnostics()["activation"]["outcome"],
            "yielded"
        );
    }

    #[test]
    fn activation_unknown_between_samples_breaks_competition_stability() {
        let (mut focus, a, _) = requested();
        let competitor = Some((20, 30, 40));
        focus.accept_activation_readback(1, competitor, Duration::from_millis(250));
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: None,
                candidate: None,
            })
            .generation()
            .unwrap();
        focus.observe(FocusSignal::Unresolved { generation });
        focus.accept_activation_readback(1, competitor, Duration::from_secs(1));
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
    }

    #[test]
    fn activation_old_candidate_cannot_terminate_a_new_request() {
        let (mut focus, _, b) = requested();
        focus.accept_activation_readback(1, Some((20, 30, 40)), Duration::from_millis(250));
        focus.request(b);
        focus.accept_activation_readback(1, Some((20, 30, 40)), Duration::from_secs(1));
        assert_eq!(focus.snapshot().requested_entity(), Some(b));
        assert_eq!(focus.activation_diagnostics()["activation"]["version"], 2);
    }

    #[test]
    fn activation_old_resolution_updates_fact_without_completing_new_intent() {
        let (mut focus, a, _) = requested();
        let generation = focus
            .observe(FocusSignal::Resolve {
                pid: None,
                candidate: Some(0),
            })
            .generation()
            .unwrap();
        focus.request(a);
        focus.observe(FocusSignal::Tracked {
            generation,
            entity: a,
            window_id: 0,
        });
        assert_eq!(focus.snapshot().confirmed_entity(), Some(a));
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
    }

    #[test]
    fn activation_superseded_attempt_is_not_external_competition() {
        let (mut focus, _, b) = requested();
        focus.requested.as_mut().unwrap().submitted = Some(Duration::ZERO);
        focus.request(b);
        let mut next = focus.requested.unwrap();
        next.window_id = 2;
        focus.requested = Some(next);
        for time in [250, 1000, 5000] {
            focus.accept_activation_readback(2, Some((0, 0, 0)), Duration::from_millis(time));
        }
        assert_eq!(focus.snapshot().requested_entity(), Some(b));
    }

    #[test]
    fn activation_navigation_selection_is_space_local_and_history_is_observed() {
        let (mut focus, a, b) = requested();
        focus.record_navigation(2, b, false, true);
        assert_eq!(focus.navigation_entity(1), Some(a));
        assert_eq!(focus.navigation_entity(2), Some(b));
        assert_eq!(focus.last_tiled(1), None);
        focus.suspend(a);
        assert_eq!(focus.snapshot().requested_entity(), Some(a));
    }
}

//! Resumable discovery of AX windows omitted from `AXWindows` on inactive Spaces.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use bevy::ecs::entity::Entity;
use objc2::MainThreadMarker;
use objc2_core_foundation::{CFMutableData, CFRetained};
use tracing::{debug, warn};

use super::skylight::_AXUIElementCreateWithRemoteToken;
use super::windows::{WindowEvidence, try_ax_window_id};
use super::{Window, WindowApi, WindowOS};
use crate::config::Config;
use crate::errors::Result;
use crate::platform::{Pid, ProcessSerialNumber, WinID};
use crate::util::{AXUIAttributes, AXUIWrapper};
use crate::window_policy::Admission;

const ELEMENT_LIMIT: u16 = 0x7fff;
const METADATA_ATTEMPTS: u8 = 3;
const PUBLICATION_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveryOwner {
    pub application: Entity,
    pub pid: Pid,
    pub psn: ProcessSerialNumber,
}

#[derive(Clone, Copy)]
struct DiscoveryBudget {
    tick_steps: usize,
    tick_time: Duration,
    app_steps: usize,
    app_time: Duration,
}

impl Default for DiscoveryBudget {
    fn default() -> Self {
        Self {
            tick_steps: 256,
            tick_time: Duration::from_millis(2),
            app_steps: usize::from(ELEMENT_LIMIT) * 8,
            app_time: Duration::from_millis(250),
        }
    }
}

struct DiscoveryTick {
    steps: usize,
    deadline: Duration,
}

impl DiscoveryTick {
    fn new(started: Duration, budget: DiscoveryBudget) -> Self {
        Self {
            steps: budget.tick_steps,
            deadline: started.saturating_add(budget.tick_time),
        }
    }

    fn take_step(&mut self, now: Duration) -> bool {
        if self.steps == 0 || now >= self.deadline {
            return false;
        }
        self.steps -= 1;
        true
    }
}

struct DiscoveryJob<S> {
    owner: DiscoveryOwner,
    remaining: HashSet<WinID>,
    element: u64,
    retry: Option<DeferredElement>,
    deferred: VecDeque<DeferredElement>,
    scan_next: bool,
    steps: usize,
    elapsed: Duration,
    state: S,
}

struct DeferredElement {
    element: u64,
    id: WinID,
    attempt: u8,
}

impl<S> DiscoveryJob<S> {
    fn finish_element(&mut self, deferred: Option<WinID>) {
        let (element, attempt) = self.retry.take().map_or_else(
            || {
                let element = self.element;
                self.element += 1;
                (element, 0)
            },
            |retry| (retry.element, retry.attempt),
        );
        if let Some(id) = deferred {
            if attempt + 1 < METADATA_ATTEMPTS {
                if !self.deferred.iter().any(|retry| retry.id == id) {
                    self.deferred.push_back(DeferredElement {
                        element,
                        id,
                        attempt: attempt + 1,
                    });
                }
            } else {
                self.remaining.remove(&id);
                warn!(?self.owner, id, "AX discovery exhausted metadata retries");
            }
        }
        self.deferred
            .retain(|retry| self.remaining.contains(&retry.id));
        let needs_scan = self.element < u64::from(ELEMENT_LIMIT)
            && self
                .remaining
                .iter()
                .any(|id| !self.deferred.iter().any(|retry| retry.id == *id));
        // Alternate retries and forward search; when all remaining identities
        // have known tokens, skip the unrelated tail of the token range.
        if !needs_scan || !self.scan_next {
            self.retry = self.deferred.pop_front();
        }
        self.scan_next = self.retry.is_some();
    }
}

enum Probe<T> {
    Pending,
    NextElement,
    Resolved(WinID, Option<T>),
    Deferred(WinID),
    Cancelled,
}

impl<T> Probe<T> {
    fn admitted(id: WinID, window: T, decision: Admission) -> Self {
        match decision {
            Admission::Track => Self::Resolved(id, Some(window)),
            Admission::Ignore => Self::Resolved(id, None),
            Admission::Defer => Self::Deferred(id),
        }
    }
}

struct DiscoveryQueue<S> {
    jobs: VecDeque<DiscoveryJob<S>>,
    budget: DiscoveryBudget,
    published_this_tick: bool,
}

struct DiscoveryPublications<T> {
    pending: VecDeque<DiscoveryPublication<T>>,
    published_this_tick: bool,
}

struct DiscoveryPublication<T> {
    owner: DiscoveryOwner,
    window: T,
    attempts: u8,
}

impl<T> DiscoveryPublications<T> {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            published_this_tick: false,
        }
    }

    fn is_pending(&self) -> bool {
        !self.pending.is_empty() || self.published_this_tick
    }

    fn cancel(&mut self) {
        self.pending.clear();
        self.published_this_tick = false;
    }

    fn advance(
        &mut self,
        now: impl Fn() -> Duration,
        tick: &mut DiscoveryTick,
        mut is_running: impl FnMut(DiscoveryOwner) -> Result<bool>,
    ) -> Vec<(DiscoveryOwner, T)> {
        let mut found = Vec::new();
        // Visit each candidate at most once per tick; an unknown sibling must
        // not consume every validation turn before other results can publish.
        for _ in 0..self.pending.len() {
            if !tick.take_step(now()) {
                break;
            }
            let Some(mut publication) = self.pending.pop_front() else {
                break;
            };
            match is_running(publication.owner) {
                Ok(true) => found.push((publication.owner, publication.window)),
                Ok(false) => {}
                Err(error) => {
                    publication.attempts += 1;
                    if publication.attempts < PUBLICATION_ATTEMPTS {
                        self.pending.push_back(publication);
                    } else {
                        warn!(owner = ?publication.owner, %error,
                            "AX discovery exhausted publication liveness retries");
                    }
                }
            }
        }
        self.published_this_tick = !found.is_empty();
        found
    }
}

impl<S> DiscoveryQueue<S> {
    fn new(budget: DiscoveryBudget) -> Self {
        Self {
            jobs: VecDeque::new(),
            budget,
            published_this_tick: false,
        }
    }

    fn enqueue(&mut self, owner: DiscoveryOwner, remaining: Vec<WinID>, state: S) {
        if remaining.is_empty() || self.jobs.iter().any(|job| job.owner == owner) {
            return;
        }
        self.jobs.push_back(DiscoveryJob {
            owner,
            remaining: remaining.into_iter().collect(),
            element: 0,
            retry: None,
            deferred: VecDeque::new(),
            scan_next: true,
            steps: 0,
            elapsed: Duration::ZERO,
            state,
        });
    }

    fn is_settled(&self) -> bool {
        self.jobs.is_empty() && !self.published_this_tick
    }

    fn cancel(&mut self) {
        self.jobs.clear();
        self.published_this_tick = false;
    }

    fn advance_validated<T>(
        &mut self,
        publications: &mut DiscoveryPublications<T>,
        now: impl Fn() -> Duration,
        mut probe: impl FnMut(DiscoveryOwner, u64, &HashSet<WinID>, &mut S) -> Probe<T>,
        mut is_running: impl FnMut(DiscoveryOwner) -> Result<bool>,
    ) -> Vec<(DiscoveryOwner, T)> {
        let mut tick = DiscoveryTick::new(now(), self.budget);
        // Drain a bounded publication batch before starting more probes. This
        // reserves validation turns even when each scan uses the whole tick.
        if publications.pending.is_empty() {
            let found =
                self.advance_in_tick(&now, &mut tick, |owner, element, remaining, state| {
                    match is_running(owner) {
                        Ok(true) => probe(owner, element, remaining, state),
                        Ok(false) => Probe::Cancelled,
                        // Unknown liveness consumes the existing application work
                        // allowance without discarding a partially inspected token.
                        Err(_) => Probe::Pending,
                    }
                });
            publications
                .pending
                .extend(
                    found
                        .into_iter()
                        .map(|(owner, window)| DiscoveryPublication {
                            owner,
                            window,
                            attempts: 0,
                        }),
                );
        }
        publications.advance(now, &mut tick, is_running)
    }

    #[cfg(test)]
    fn advance<T>(
        &mut self,
        now: impl Fn() -> Duration,
        probe: impl FnMut(DiscoveryOwner, u64, &HashSet<WinID>, &mut S) -> Probe<T>,
    ) -> Vec<(DiscoveryOwner, T)> {
        let mut tick = DiscoveryTick::new(now(), self.budget);
        self.advance_in_tick(now, &mut tick, probe)
    }

    fn advance_in_tick<T>(
        &mut self,
        now: impl Fn() -> Duration,
        tick: &mut DiscoveryTick,
        mut probe: impl FnMut(DiscoveryOwner, u64, &HashSet<WinID>, &mut S) -> Probe<T>,
    ) -> Vec<(DiscoveryOwner, T)> {
        let mut found = Vec::new();
        while !self.jobs.is_empty() && tick.take_step(now()) {
            let Some(mut job) = self.jobs.pop_front() else {
                break;
            };
            let before = now();
            let element = job
                .retry
                .as_ref()
                .map_or(job.element, |retry| retry.element);
            let outcome = probe(job.owner, element, &job.remaining, &mut job.state);
            job.elapsed += now().saturating_sub(before);
            job.steps += 1;
            match outcome {
                Probe::Pending => {}
                Probe::NextElement => {
                    // A failed re-read of a deferred token is still unknown,
                    // but spends one of the same finite metadata attempts.
                    let deferred = job.retry.as_ref().map(|retry| retry.id);
                    job.finish_element(deferred);
                }
                Probe::Resolved(id, window) => {
                    if job.remaining.remove(&id)
                        && let Some(window) = window
                    {
                        found.push((job.owner, window));
                    }
                    job.finish_element(None);
                }
                Probe::Deferred(id) => job.finish_element(Some(id)),
                Probe::Cancelled => continue,
            }
            if job.remaining.is_empty()
                || (job.element >= u64::from(ELEMENT_LIMIT) && job.retry.is_none())
            {
                continue;
            }
            if job.elapsed >= self.budget.app_time || job.steps >= self.budget.app_steps {
                warn!(?job.owner, element = job.element, unresolved = ?job.remaining,
                    "AX discovery exhausted its active-work budget");
                continue;
            }
            self.jobs.push_back(job);
        }
        self.published_this_tick = !found.is_empty();
        found
    }
}

/// Owns every native probe and result on the main thread, including destruction.
/// Deliberately `NonSend`: neither AX elements nor Window values enter a task pool.
pub(crate) struct WindowDiscovery {
    queue: DiscoveryQueue<NativeProbe>,
    publications: DiscoveryPublications<Window>,
    _main_thread: MainThreadMarker,
}

impl WindowDiscovery {
    pub fn new(main_thread: MainThreadMarker) -> Self {
        Self {
            queue: DiscoveryQueue::new(DiscoveryBudget::default()),
            publications: DiscoveryPublications::new(),
            _main_thread: main_thread,
        }
    }

    pub fn enqueue(
        &mut self,
        owner: DiscoveryOwner,
        bundle_id: Option<String>,
        remaining: Vec<WinID>,
        config: Config,
    ) {
        self.queue.enqueue(
            owner,
            remaining,
            NativeProbe {
                bundle_id,
                config,
                token: None,
                stage: ProbeStage::Create,
            },
        );
    }

    pub fn is_pending(&self) -> bool {
        !self.queue.is_settled() || self.publications.is_pending()
    }

    pub fn cancel(&mut self) {
        self.queue.cancel();
        self.publications.cancel();
    }

    /// The 2ms tick and 250ms active-app budgets are soft wall-clock ceilings.
    /// One synchronous call may overrun them; no further probe starts that tick.
    /// Waiting between ticks does not reduce an application's search allowance.
    pub fn advance(
        &mut self,
        is_running: impl FnMut(DiscoveryOwner) -> Result<bool>,
    ) -> Vec<(DiscoveryOwner, Window)> {
        let started = Instant::now();
        self.queue.advance_validated(
            &mut self.publications,
            || started.elapsed(),
            |owner, element, remaining, state| state.step(owner.pid, element, remaining),
            is_running,
        )
    }
}

struct NativeProbe {
    bundle_id: Option<String>,
    config: Config,
    token: Option<CFRetained<CFMutableData>>,
    stage: ProbeStage,
}

#[derive(Default)]
enum ProbeStage {
    #[default]
    Create,
    Identify(CFRetained<AXUIWrapper>),
    Inspect(Box<Candidate>, Attribute),
    Admit(Box<Candidate>),
}

struct Candidate {
    window: WindowOS,
    evidence: WindowEvidence,
    parent: Option<CFRetained<AXUIWrapper>>,
}

enum Attribute {
    Role,
    Subrole,
    Title,
    Parent,
    ParentRole,
}

impl NativeProbe {
    // Each step performs at most one synchronous AX operation. Even matching
    // candidates yield between identity, attributes, parent, and admission.
    fn step(&mut self, pid: Pid, element_id: u64, remaining: &HashSet<WinID>) -> Probe<Window> {
        match std::mem::take(&mut self.stage) {
            ProbeStage::Create => {
                let Some(token) = self.token(pid, element_id) else {
                    warn!(pid, "unable to allocate the AX discovery token");
                    return Probe::Cancelled;
                };
                let Ok(element) = AXUIWrapper::from_retained(unsafe {
                    _AXUIElementCreateWithRemoteToken(token.as_ref())
                }) else {
                    return Probe::NextElement;
                };
                self.stage = ProbeStage::Identify(element);
            }
            ProbeStage::Identify(element) => {
                let Some(id) =
                    try_ax_window_id(element.as_ptr()).filter(|id| remaining.contains(id))
                else {
                    return Probe::NextElement;
                };
                self.stage = ProbeStage::Inspect(
                    Box::new(Candidate {
                        window: WindowOS::from_element(id, &element),
                        evidence: WindowEvidence::default(),
                        parent: None,
                    }),
                    Attribute::Role,
                );
            }
            ProbeStage::Inspect(mut candidate, attribute) => {
                let next = match attribute {
                    Attribute::Role => {
                        candidate.evidence.role = candidate.window.role().ok();
                        Attribute::Subrole
                    }
                    Attribute::Subrole => {
                        candidate.evidence.subrole = candidate.window.subrole().ok();
                        Attribute::Title
                    }
                    Attribute::Title => {
                        candidate.evidence.title = candidate.window.title().ok();
                        Attribute::Parent
                    }
                    Attribute::Parent => {
                        candidate.parent = candidate.window.parent_element().ok();
                        Attribute::ParentRole
                    }
                    Attribute::ParentRole => {
                        candidate.evidence.parent_role = candidate
                            .parent
                            .as_ref()
                            .and_then(|parent| parent.role().ok());
                        self.stage = ProbeStage::Admit(candidate);
                        return Probe::Pending;
                    }
                };
                self.stage = ProbeStage::Inspect(candidate, next);
            }
            ProbeStage::Admit(candidate) => {
                let Candidate {
                    window, evidence, ..
                } = *candidate;
                let id = window.id();
                let decision = evidence.admission(&self.config, self.bundle_id.as_deref());
                debug!(id, ?decision, "discovered window admission");
                return Probe::admitted(id, Window::new(Box::new(window)), decision);
            }
        }
        Probe::Pending
    }

    fn token(&mut self, pid: Pid, element_id: u64) -> Option<&CFRetained<CFMutableData>> {
        const BUFFER_SIZE: u8 = 20;
        if self.token.is_none() {
            let token = CFMutableData::new(None, isize::from(BUFFER_SIZE))?;
            CFMutableData::increase_length(Some(token.as_ref()), isize::from(BUFFER_SIZE));
            self.token = Some(token);
        }
        let token = self.token.as_ref()?;
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                CFMutableData::mutable_byte_ptr(Some(token.as_ref())),
                usize::from(BUFFER_SIZE),
            )
        };
        bytes[0..4].copy_from_slice(&pid.to_ne_bytes());
        bytes[4..8].fill(0);
        bytes[8..12].copy_from_slice(&0x636f_636fu32.to_ne_bytes());
        bytes[12..20].copy_from_slice(&element_id.to_ne_bytes());
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn owner(pid: i32) -> DiscoveryOwner {
        DiscoveryOwner {
            application: Entity::from_raw_u32(pid.cast_unsigned()).unwrap(),
            pid,
            psn: ProcessSerialNumber {
                high: 0,
                low: pid.cast_unsigned(),
            },
        }
    }

    #[test]
    fn discovery_retries_deferred_title_until_metadata_recovers() {
        let config =
            Config::try_from(r#"{"windows":{"settings":{"title":"Settings","floating":true}}}"#)
                .unwrap();
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        let mut reads = 0;
        let mut found = Vec::new();
        for _ in 0..8 {
            found.extend(queue.advance(
                || Duration::ZERO,
                |_, _, _, ()| {
                    reads += 1;
                    let title = if reads == 1 {
                        Err(crate::errors::Error::macos(
                            "AXTitle",
                            accessibility_sys::kAXErrorCannotComplete,
                        ))
                    } else {
                        Ok("Settings".to_owned())
                    };
                    let evidence = WindowEvidence {
                        role: Some("AXWindow".into()),
                        subrole: Some("AXStandardWindow".into()),
                        title: title.ok(),
                        parent_role: Some("AXApplication".into()),
                    };
                    Probe::admitted(42, 42, evidence.admission(&config, Some("test")))
                },
            ));
        }
        assert_eq!(found, vec![(owner(1), 42)]);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_retries_publication_liveness_without_rescanning() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        let mut publications = DiscoveryPublications::new();
        queue.enqueue(owner(1), vec![42], ());
        let mut probes = 0;
        let mut checks = 0;
        let mut found = Vec::new();
        for _ in 0..8 {
            found.extend(queue.advance_validated(
                &mut publications,
                || Duration::ZERO,
                |_, _, _, ()| {
                    probes += 1;
                    Probe::Resolved(42, Some(42))
                },
                |_| {
                    checks += 1;
                    if checks == 2 {
                        Err(crate::errors::Error::macos("GetProcessPID", -1))
                    } else {
                        Ok(true)
                    }
                },
            ));
        }
        assert_eq!(found, vec![(owner(1), 42)]);
        assert_eq!(probes, 1);
        assert!(!publications.is_pending());
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_exhausts_publication_errors_without_starving_a_sibling() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        let mut publications = DiscoveryPublications::new();
        queue.enqueue(owner(1), vec![42, 43], ());
        let mut checks = 0;
        let mut found = Vec::new();
        for tick in 0..8 {
            found.extend(queue.advance_validated(
                &mut publications,
                || Duration::ZERO,
                |_, element, _, ()| match element {
                    0 => Probe::Resolved(42, Some(42)),
                    1 => Probe::Resolved(43, Some(43)),
                    _ => panic!("both windows resolved"),
                },
                |_| {
                    checks += 1;
                    if checks <= 2 || checks == 4 {
                        Ok(true)
                    } else {
                        Err(crate::errors::Error::macos("GetProcessPID", -1))
                    }
                },
            ));
            if tick == 0 {
                assert_eq!(found, vec![(owner(1), 43)]);
                assert!(publications.is_pending());
            }
        }
        assert_eq!(
            checks, 6,
            "two probes, three failed checks, one live sibling"
        );
        assert_eq!(found, vec![(owner(1), 43)]);
        assert!(queue.is_settled());
        assert!(!publications.is_pending());
    }

    #[test]
    fn discovery_all_owner_errors_are_finite_and_do_not_starve_a_live_owner() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            app_steps: 3,
            ..DiscoveryBudget::default()
        });
        let mut publications = DiscoveryPublications::new();
        queue.enqueue(owner(1), vec![42], ());
        queue.enqueue(owner(2), vec![43], ());
        let mut errors = 0;
        let mut found = Vec::new();
        for _ in 0..12 {
            found.extend(queue.advance_validated(
                &mut publications,
                || Duration::ZERO,
                |owner, _, _, ()| {
                    assert_eq!(owner.pid, 2, "unknown owner cannot start native work");
                    Probe::Resolved(43, Some(43))
                },
                |owner| {
                    if owner.pid == 1 {
                        errors += 1;
                        Err(crate::errors::Error::macos("GetProcessPID", -1))
                    } else {
                        Ok(true)
                    }
                },
            ));
        }
        assert_eq!(errors, 3);
        assert_eq!(found, vec![(owner(2), 43)]);
        assert!(queue.is_settled());
        assert!(!publications.is_pending());
    }

    #[test]
    fn discovery_publication_shares_tick_steps_and_keeps_the_startup_guard() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        let mut publications = DiscoveryPublications::new();
        queue.enqueue(owner(1), vec![42], ());
        let mut checks = 0;
        for tick in 0..3 {
            let found = queue.advance_validated(
                &mut publications,
                || Duration::ZERO,
                |_, _, _, ()| Probe::Resolved(42, Some(42)),
                |_| {
                    checks += 1;
                    Ok(true)
                },
            );
            match tick {
                0 => {
                    assert!(found.is_empty());
                    assert_eq!(checks, 1, "the probe used the only tick step");
                    assert!(publications.is_pending());
                }
                1 => {
                    assert_eq!(found, vec![(owner(1), 42)]);
                    assert_eq!(checks, 2);
                    assert!(
                        publications.is_pending(),
                        "spawn commands still need to settle"
                    );
                }
                _ => {
                    assert!(found.is_empty());
                    assert!(queue.is_settled());
                    assert!(!publications.is_pending());
                }
            }
        }
    }

    #[test]
    fn discovery_publication_yields_after_a_slow_check_and_rotates_the_error() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        let mut publications = DiscoveryPublications::new();
        queue.enqueue(owner(1), vec![42, 43], ());
        let clock = Cell::new(Duration::ZERO);
        let mut checks = 0;
        for tick in 0..2 {
            let found = queue.advance_validated(
                &mut publications,
                || clock.get(),
                |_, element, _, ()| match element {
                    0 => Probe::Resolved(42, Some(42)),
                    1 => Probe::Resolved(43, Some(43)),
                    _ => panic!("both windows resolved"),
                },
                |_| {
                    checks += 1;
                    if checks == 3 {
                        clock.set(clock.get() + Duration::from_millis(17));
                        Err(crate::errors::Error::macos("GetProcessPID", -1))
                    } else {
                        Ok(true)
                    }
                },
            );
            if tick == 0 {
                assert!(found.is_empty());
                assert_eq!(checks, 3, "no sibling check after the soft deadline");
            } else {
                assert_eq!(found, vec![(owner(1), 43), (owner(1), 42)]);
            }
        }
    }

    #[test]
    fn discovery_pending_publications_drop_on_invalidation_or_cancellation() {
        for cancel in [false, true] {
            let candidate = Rc::new(());
            let weak = Rc::downgrade(&candidate);
            let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
            let mut publications = DiscoveryPublications::new();
            queue.enqueue(owner(1), vec![42], Some(candidate));
            let mut checks = 0;
            assert!(
                queue
                    .advance_validated(
                        &mut publications,
                        || Duration::ZERO,
                        |_, _, _, candidate| Probe::Resolved(42, candidate.take()),
                        |_| {
                            checks += 1;
                            if checks == 1 {
                                Ok(true)
                            } else {
                                Err(crate::errors::Error::macos("GetProcessPID", -1))
                            }
                        },
                    )
                    .is_empty()
            );
            assert!(weak.upgrade().is_some());
            if cancel {
                queue.cancel();
                publications.cancel();
            } else {
                assert!(
                    queue
                        .advance_validated(
                            &mut publications,
                            || Duration::ZERO,
                            |_, _, _, _| panic!("no rescan of a retained publication"),
                            |_| Ok(false),
                        )
                        .is_empty()
                );
            }
            assert!(weak.upgrade().is_none());
            assert!(
                queue
                    .advance_validated(
                        &mut publications,
                        || Duration::ZERO,
                        |_, _, _, _| panic!("no remaining probe"),
                        |_| panic!("no remaining publication"),
                    )
                    .is_empty()
            );
            assert!(queue.is_settled());
            assert!(!publications.is_pending());
        }
    }

    #[test]
    fn discovery_exhausts_deferred_metadata_without_starving_a_sibling() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42, 43], ());
        let mut deferred_reads = 0;
        let mut found = Vec::new();
        for tick in 0..8 {
            found.extend(queue.advance(
                || Duration::ZERO,
                |_, element, _, ()| match element {
                    0 => {
                        deferred_reads += 1;
                        Probe::admitted(42, 42, Admission::Defer)
                    }
                    1 => Probe::admitted(43, 43, Admission::Track),
                    _ => panic!("all remaining identities already have tokens"),
                },
            ));
            if tick == 1 {
                assert_eq!(found, vec![(owner(1), 43)]);
            }
        }
        assert_eq!(deferred_reads, 3);
        assert_eq!(found, vec![(owner(1), 43)]);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_metadata_retry_does_not_reset_the_application_budget() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            app_steps: 2,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        let mut reads = 0;
        for _ in 0..8 {
            assert!(
                queue
                    .advance(
                        || Duration::ZERO,
                        |_, _, _, ()| {
                            reads += 1;
                            Probe::admitted(42, 42, Admission::Defer)
                        },
                    )
                    .is_empty()
            );
        }
        assert_eq!(reads, 2);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_retries_the_last_token_and_does_not_retry_rejection() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        queue.enqueue(owner(1), vec![42, 43], ());
        let mut last_reads = 0;
        let mut rejected_reads = 0;
        let mut found = Vec::new();
        for _ in 0..131 {
            found.extend(queue.advance(
                || Duration::ZERO,
                |_, element, _, ()| {
                    if element == 0 {
                        rejected_reads += 1;
                        Probe::admitted(43, 43, Admission::Ignore)
                    } else if element == 0x7ffe {
                        last_reads += 1;
                        Probe::admitted(
                            42,
                            42,
                            if last_reads == 1 {
                                Admission::Defer
                            } else {
                                Admission::Track
                            },
                        )
                    } else {
                        Probe::NextElement
                    }
                },
            ));
        }
        assert_eq!(last_reads, 2);
        assert_eq!(rejected_reads, 1);
        assert_eq!(found, vec![(owner(1), 42)]);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_yields_at_the_tick_step_budget_and_resumes_the_cursor() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 2,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        let mut visited = Vec::new();
        queue.advance::<()>(
            || Duration::ZERO,
            |_, element, _, ()| {
                visited.push(element);
                Probe::NextElement
            },
        );
        assert_eq!(visited.len(), 2);
        assert_eq!(visited, vec![0, 1]);
        assert!(!queue.is_settled());
        visited.clear();
        queue.advance::<()>(
            || Duration::ZERO,
            |_, element, _, ()| {
                visited.push(element);
                Probe::NextElement
            },
        );
        assert_eq!(visited, vec![2, 3]);
    }

    #[test]
    fn discovery_stops_after_one_unpreemptible_probe_crosses_the_soft_deadline() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        queue.enqueue(owner(1), vec![42], ());
        let clock = Cell::new(Duration::ZERO);
        let mut calls = 0;
        queue.advance::<()>(
            || clock.get(),
            |_, _, _, ()| {
                calls += 1;
                clock.set(clock.get() + Duration::from_millis(17));
                Probe::NextElement
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(clock.get(), Duration::from_millis(17));
        assert!(!queue.is_settled());
    }

    #[test]
    fn discovery_round_robins_across_ticks_and_only_charges_active_work() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            app_time: Duration::from_millis(3),
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        queue.enqueue(owner(2), vec![43], ());
        let clock = Cell::new(Duration::ZERO);
        let mut visited = Vec::new();
        for _ in 0..6 {
            queue.advance::<()>(
                || clock.get(),
                |owner, element, _, ()| {
                    visited.push((owner.pid, element));
                    clock.set(clock.get() + Duration::from_millis(1));
                    Probe::NextElement
                },
            );
            // Time spent queued behind other applications/frames is not work.
            clock.set(clock.get() + Duration::from_secs(100));
        }
        assert_eq!(
            visited,
            vec![(1, 0), (2, 0), (1, 1), (2, 1), (1, 2), (2, 2)]
        );
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_bounds_total_work_when_clock_and_cursor_never_advance() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 2,
            app_steps: 3,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        let mut calls = 0;
        for _ in 0..3 {
            queue.advance::<()>(
                || Duration::ZERO,
                |_, _, _, ()| {
                    calls += 1;
                    Probe::Pending
                },
            );
        }
        assert_eq!(calls, 3);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_cancellation_drops_suspended_state_without_more_probes() {
        struct State(Rc<Cell<usize>>);
        impl Drop for State {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let dropped = Rc::new(Cell::new(0));
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], State(dropped.clone()));
        queue.enqueue(owner(2), vec![43], State(dropped.clone()));
        queue.advance::<()>(|| Duration::ZERO, |_, _, _, _| Probe::Pending);
        assert_eq!(dropped.get(), 0);
        queue.cancel();
        assert_eq!(dropped.get(), 2);
        queue.advance::<()>(|| Duration::ZERO, |_, _, _, _| panic!("cancelled probe"));
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_exited_owner_drops_a_candidate_instead_of_publishing_it() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        let candidate = Rc::new(());
        let weak = Rc::downgrade(&candidate);
        queue.enqueue(owner(1), vec![42], candidate);
        let first = queue.advance::<()>(|| Duration::ZERO, |_, _, _, _| Probe::Pending);
        assert!(first.is_empty());
        assert!(weak.upgrade().is_some());
        let after_exit = queue.advance::<()>(|| Duration::ZERO, |_, _, _, _| Probe::Cancelled);
        assert!(after_exit.is_empty());
        assert!(weak.upgrade().is_none());
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_keeps_the_full_inactive_space_token_range() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        queue.enqueue(owner(1), vec![42], ());
        let mut visited = 0;
        let mut found = Vec::new();
        for _ in 0..130 {
            found.extend(queue.advance(
                || Duration::ZERO,
                |_, element, _, ()| {
                    visited += 1;
                    if element == 0x7ffe {
                        Probe::Resolved(42, Some("background Space window"))
                    } else {
                        Probe::NextElement
                    }
                },
            ));
        }
        assert_eq!(visited, 32767);
        assert_eq!(found, vec![(owner(1), "background Space window")]);
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_settles_only_after_the_last_publication_tick() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget {
            tick_steps: 1,
            ..DiscoveryBudget::default()
        });
        queue.enqueue(owner(1), vec![42], ());
        assert!(!queue.is_settled());
        queue.advance::<()>(|| Duration::ZERO, |_, _, _, ()| Probe::Pending);
        assert!(!queue.is_settled());
        let found = queue.advance(
            || Duration::ZERO,
            |_, _, _, ()| Probe::Resolved(42, Some(42)),
        );
        assert_eq!(found, vec![(owner(1), 42)]);
        assert!(!queue.is_settled());
        queue.advance::<()>(|| Duration::ZERO, |_, _, _, ()| panic!("finished probe"));
        assert!(queue.is_settled());
    }

    #[test]
    fn discovery_state_probes_and_results_stay_on_the_calling_thread() {
        let caller = std::thread::current().id();
        let state = Rc::new(Cell::new(0));
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        queue.enqueue(owner(1), vec![42], state.clone());
        let found = queue.advance(
            || Duration::ZERO,
            |_, _, _, state| {
                assert_eq!(std::thread::current().id(), caller);
                state.set(state.get() + 1);
                Probe::Resolved(42, Some(state.clone()))
            },
        );
        assert_eq!(state.get(), 1);
        assert!(Rc::ptr_eq(&found[0].1, &state));
    }

    #[test]
    fn discovery_deduplicates_jobs_and_consumes_rejected_window_ids() {
        let mut queue = DiscoveryQueue::new(DiscoveryBudget::default());
        queue.enqueue(owner(1), vec![42, 42, 43], "original");
        queue.enqueue(owner(1), vec![99], "duplicate");
        let mut calls = 0;
        let found = queue.advance(
            || Duration::ZERO,
            |_, element, _, state| {
                calls += 1;
                assert_eq!(*state, "original");
                match element {
                    0 => Probe::Resolved(42, None),
                    1 => Probe::Resolved(43, Some(43)),
                    _ => panic!("already resolved"),
                }
            },
        );
        assert_eq!(calls, 2);
        assert_eq!(found, vec![(owner(1), 43)]);
    }
}

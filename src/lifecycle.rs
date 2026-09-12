//! Readiness shared by the IPC adapter and the ordered command executor.

use bevy::prelude::Resource;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Phase {
    Starting,
    WaitingForPermission,
    Running,
    Stopping,
}

#[derive(Clone, Resource)]
pub(crate) struct Lifecycle(Arc<AtomicU8>);

impl Default for Lifecycle {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(Phase::Running as u8)))
    }
}

impl Lifecycle {
    pub(crate) fn starting() -> Self {
        Self(Arc::new(AtomicU8::new(Phase::Starting as u8)))
    }
    pub(crate) fn set(&self, phase: Phase) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current != Phase::Stopping as u8).then_some(phase as u8)
            });
    }
    pub(crate) fn phase(&self) -> Phase {
        match self.0.load(Ordering::Acquire) {
            0 => Phase::Starting,
            1 => Phase::WaitingForPermission,
            2 => Phase::Running,
            _ => Phase::Stopping,
        }
    }
    pub(crate) fn rejection(&self) -> Option<&'static str> {
        match self.phase() {
            Phase::Starting | Phase::WaitingForPermission => Some("not_ready"),
            Phase::Stopping => Some("shutting_down"),
            Phase::Running => None,
        }
    }
}

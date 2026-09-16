//! Aligning retained intent with what the display actually shows.
//!
//! An accepted edit's realization ends in one of three outcomes (ADR 0011):
//! realized (including a constrained realization), definitively refused, or not
//! yet decidable. When a round is over — the code has stopped trying and the
//! window keeps showing a value the derived target never reached — the authored
//! field follows the display, so state and display agree again.
//!
//! Alignment never invents a value: it adopts what a stable observation shows,
//! it corrects only authored intent (never a derived target), and it applies only
//! while the failed round still owns the field. The records here are bounded,
//! in-memory diagnostics, like the repair histories the membership and floating
//! slices keep.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::resource::Resource;

use crate::ecs::layout::{ColumnId, WidthIntent};

/// How many alignments one window keeps for diagnostics. Repairs keep a similar
/// bound; the file this used to feed no longer exists (ADR 0010).
const MAX_RECORDS: usize = 4;

/// Why an authored field was aligned to the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlignmentReason {
    /// The round was over and the window kept showing a value the target never
    /// reached: the grace period expired without a realization.
    NotRealizedWithinGrace,
    /// The platform answered that this exact request is invalid for this window.
    WriteRefused { code: i32 },
}

/// The authored field alignment corrected, with both values for diagnosis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AlignedField {
    ColumnWidth {
        column: ColumnId,
        prior: WidthIntent,
        adopted: WidthIntent,
    },
}

/// One alignment, kept in memory only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AlignmentRecord {
    pub field: AlignedField,
    pub reason: AlignmentReason,
    pub at: Duration,
}

/// Marks a window whose frame write the platform refused as invalid for that
/// exact request. The commit site knows the error code; the alignment pass owns
/// the strips a correction needs, so the refusal travels between them.
#[derive(Clone, Copy, Debug, Component)]
pub(crate) struct FrameWriteRefused {
    pub code: i32,
}

/// Bounded, in-memory alignment history per window.
#[derive(Debug, Default, Resource)]
pub(crate) struct RealizationAlignments {
    by_window: HashMap<Entity, VecDeque<AlignmentRecord>>,
}

impl RealizationAlignments {
    pub(crate) fn record(&mut self, entity: Entity, record: AlignmentRecord) {
        let records = self.by_window.entry(entity).or_default();
        if records.len() == MAX_RECORDS {
            records.pop_front();
        }
        records.push_back(record);
    }

    pub(crate) fn records(
        &self,
        entity: Entity,
    ) -> impl DoubleEndedIterator<Item = &AlignmentRecord> {
        self.by_window.get(&entity).into_iter().flatten()
    }

    pub(crate) fn forget(&mut self, entity: Entity) {
        self.by_window.remove(&entity);
    }
}

/// Whether a macOS error answers "this request is invalid for this window"
/// rather than "not right now".
///
/// Only the first is a definitive refusal. `kAXErrorCannotComplete`, an
/// unavailable or disabled API, and `kAXErrorNotImplemented` describe a moment,
/// and `kAXErrorFailure` is too vague to read as an answer at all, so they stay
/// "not yet decidable" (ADR 0011).
pub(crate) fn refusal_is_definitive(code: i32) -> bool {
    matches!(
        code,
        accessibility_sys::kAXErrorIllegalArgument
            | accessibility_sys::kAXErrorAttributeUnsupported
            | accessibility_sys::kAXErrorActionUnsupported
    )
}

/// The width intent that matches what the display shows, keeping the intent's
/// variant: a viewport ratio stays a ratio recomputed against the current
/// viewport, while an inherited width becomes absolute, because the displayed
/// value is always concrete (ADR 0011).
///
/// `None` means the display cannot be expressed as intent — a ratio without a
/// known viewport — and the caller must keep waiting instead of inventing one.
pub(crate) fn aligned_width_intent(
    intent: WidthIntent,
    observed_width: i32,
    viewport_width: Option<i32>,
) -> Option<WidthIntent> {
    let observed = f64::from(observed_width);
    if observed <= 0.0 {
        return None;
    }
    match intent {
        WidthIntent::Absolute(_) | WidthIntent::InheritConfig => {
            Some(WidthIntent::Absolute(observed))
        }
        WidthIntent::ViewportRatio(_) => {
            let viewport = f64::from(viewport_width.filter(|width| *width > 0)?);
            Some(WidthIntent::ViewportRatio(observed / viewport))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_invalid_request_errors_are_definitive_refusals() {
        use accessibility_sys::{
            kAXErrorActionUnsupported, kAXErrorAttributeUnsupported, kAXErrorCannotComplete,
            kAXErrorFailure, kAXErrorIllegalArgument, kAXErrorNotImplemented,
        };
        for code in [
            kAXErrorIllegalArgument,
            kAXErrorAttributeUnsupported,
            kAXErrorActionUnsupported,
        ] {
            let _ = kAXErrorCannotComplete;
            assert!(refusal_is_definitive(code), "code {code}");
        }
        for code in [
            kAXErrorCannotComplete,
            kAXErrorFailure,
            kAXErrorNotImplemented,
        ] {
            assert!(!refusal_is_definitive(code), "code {code}");
        }
    }

    #[test]
    fn an_absolute_width_adopts_the_displayed_width() {
        assert_eq!(
            aligned_width_intent(WidthIntent::Absolute(900.0), 800, Some(1600)),
            Some(WidthIntent::Absolute(800.0))
        );
    }

    #[test]
    fn an_inherited_width_becomes_absolute() {
        assert_eq!(
            aligned_width_intent(WidthIntent::InheritConfig, 640, Some(1600)),
            Some(WidthIntent::Absolute(640.0))
        );
    }

    #[test]
    fn a_ratio_stays_a_ratio_against_the_current_viewport() {
        assert_eq!(
            aligned_width_intent(WidthIntent::ViewportRatio(0.75), 800, Some(1600)),
            Some(WidthIntent::ViewportRatio(0.5))
        );
    }

    #[test]
    fn a_ratio_without_a_viewport_is_not_invented() {
        assert_eq!(
            aligned_width_intent(WidthIntent::ViewportRatio(0.75), 800, None),
            None
        );
        assert_eq!(
            aligned_width_intent(WidthIntent::ViewportRatio(0.75), 800, Some(0)),
            None
        );
    }

    #[test]
    fn an_unrepresentable_display_width_is_not_adopted() {
        assert_eq!(
            aligned_width_intent(WidthIntent::Absolute(900.0), 0, Some(1600)),
            None
        );
    }

    #[test]
    fn records_are_bounded_per_window() {
        let mut world = bevy::prelude::World::new();
        let first = world.spawn_empty().id();
        let second = world.spawn_empty().id();
        let mut alignments = RealizationAlignments::default();
        let recorded = u32::try_from(MAX_RECORDS + 2).expect("a small bound");
        for value in 1..=recorded {
            alignments.record(
                first,
                AlignmentRecord {
                    field: AlignedField::ColumnWidth {
                        column: ColumnId(1),
                        prior: WidthIntent::Absolute(900.0),
                        adopted: WidthIntent::Absolute(f64::from(value)),
                    },
                    reason: AlignmentReason::NotRealizedWithinGrace,
                    at: Duration::ZERO,
                },
            );
        }
        assert_eq!(alignments.records(first).count(), MAX_RECORDS);
        assert_eq!(
            alignments
                .records(first)
                .next_back()
                .map(|record| record.field),
            Some(AlignedField::ColumnWidth {
                column: ColumnId(1),
                prior: WidthIntent::Absolute(900.0),
                adopted: WidthIntent::Absolute(f64::from(recorded)),
            })
        );
        assert_eq!(alignments.records(second).count(), 0);
        alignments.forget(first);
        assert_eq!(alignments.records(first).count(), 0);
    }
}

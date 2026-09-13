//! Pure column intent and width projection. Native observations never own intent.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::errors::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColumnId(pub u64);

impl ColumnId {
    fn allocate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("runtime column identity exhausted"),
        )
    }
}

/// Width of a layout slot, including both horizontal padding edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum WidthIntent {
    #[default]
    InheritConfig,
    Absolute(f64),
    ViewportRatio(f64),
}

impl WidthIntent {
    pub fn validate(self) -> Result<()> {
        match self {
            Self::InheritConfig => Ok(()),
            Self::Absolute(value) | Self::ViewportRatio(value)
                if value.is_finite() && value > 0.0 =>
            {
                Ok(())
            }
            _ => Err(Error::InvalidInput(
                "column width must be positive and finite".into(),
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ColumnState {
    pub height_items: Vec<super::StackItemState>,
    pub height_revision: u64,
    pub id: ColumnId,
    pub width: WidthIntent,
    pub intent_revision: u64,
    pub restore_width: Option<WidthIntent>,
    pub(crate) configured_width: Option<WidthIntent>,
    pub(crate) config_source: Option<bevy::ecs::entity::Entity>,
    pub(crate) constraints: Vec<WidthConstraint>,
}

impl Default for ColumnState {
    fn default() -> Self {
        Self {
            id: ColumnId::allocate(),
            height_items: Vec::new(),
            height_revision: 0,
            width: WidthIntent::InheritConfig,
            intent_revision: 0,
            restore_width: None,
            configured_width: None,
            config_source: None,
            constraints: Vec::new(),
        }
    }
}

impl ColumnState {
    pub(crate) fn split(&self) -> Self {
        Self {
            id: ColumnId::allocate(),
            height_items: self.height_items.clone(),
            height_revision: self.height_revision,
            width: self.width,
            intent_revision: self.intent_revision,
            restore_width: None,
            configured_width: self.configured_width,
            config_source: None,
            constraints: Vec::new(),
        }
    }
}

/// Only fresh, trusted independent width intervals belong here. Unsupported
/// discrete or coupled constraints are explicit instead of invented intervals.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(
    dead_code,
    reason = "trusted constraint adapter seam; unknown native capabilities contribute no interval"
)]
pub enum WidthConstraint {
    Interval { min: f64, max: f64 },
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthProjectionBlocked {
    UnknownViewport,
    UnknownConfiguration,
    InvalidIntent,
    InvalidConstraint,
    UnsupportedConstraint,
    ConflictingConstraints,
    Unrepresentable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveColumnWidth {
    pub requested: f64,
    pub slot: i32,
    pub constrained: bool,
}

/// Convert a native outer-frame interval into slot coordinates exactly once.
#[allow(
    dead_code,
    reason = "native-to-slot conversion seam for trusted capability adapters"
)]
pub fn native_width_constraint(min: f64, max: f64, padding: f64) -> WidthConstraint {
    if !padding.is_finite() || padding < 0.0 {
        return WidthConstraint::Unsupported;
    }
    WidthConstraint::Interval {
        min: min + 2.0 * padding,
        max: max + 2.0 * padding,
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    reason = "validated rounded integer coordinates are exactly representable as f64"
)]
pub fn project_column_width(
    intent: WidthIntent,
    inherited: WidthIntent,
    viewport: Option<i32>,
    constraints: impl IntoIterator<Item = WidthConstraint>,
) -> std::result::Result<EffectiveColumnWidth, WidthProjectionBlocked> {
    use WidthProjectionBlocked as Blocked;
    let intent = if intent == WidthIntent::InheritConfig {
        inherited
    } else {
        intent
    };
    intent.validate().map_err(|_| Blocked::InvalidIntent)?;
    let requested = match intent {
        WidthIntent::InheritConfig => return Err(Blocked::UnknownConfiguration),
        WidthIntent::Absolute(width) => width,
        WidthIntent::ViewportRatio(ratio) => {
            ratio
                * f64::from(
                    viewport
                        .filter(|width| *width > 0)
                        .ok_or(Blocked::UnknownViewport)?,
                )
        }
    };
    if !requested.is_finite() || requested.round() < 1.0 || requested.round() > f64::from(i32::MAX)
    {
        return Err(Blocked::Unrepresentable);
    }
    let mut lower = 1.0_f64;
    let mut upper = f64::from(i32::MAX);
    for constraint in constraints {
        match constraint {
            WidthConstraint::Unsupported => return Err(Blocked::UnsupportedConstraint),
            WidthConstraint::Interval { min, max } => {
                if !min.is_finite() || !max.is_finite() || min <= 0.0 || max < min {
                    return Err(Blocked::InvalidConstraint);
                }
                lower = lower.max(min.ceil());
                upper = upper.min(max.floor());
            }
        }
    }
    if lower > upper {
        return Err(Blocked::ConflictingConstraints);
    }
    let effective = requested.round().clamp(lower, upper);
    Ok(EffectiveColumnWidth {
        requested,
        slot: effective as i32,
        constrained: effective != requested.round(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constraints_preserve_raw_intent_and_padding_is_applied_once() {
        let intent = WidthIntent::Absolute(800.0);
        let result = project_column_width(
            intent,
            WidthIntent::Absolute(900.0),
            None,
            [native_width_constraint(984.0, 1200.0, 8.0)],
        )
        .unwrap();
        assert_eq!(result.slot, 1000);
        assert!(result.constrained);
        assert_eq!(intent, WidthIntent::Absolute(800.0));
        assert_eq!(
            project_column_width(intent, WidthIntent::Absolute(900.0), None, [])
                .unwrap()
                .slot,
            800
        );
    }
    #[test]
    fn ratio_is_valid_without_a_viewport_but_projection_is_blocked() {
        let intent = WidthIntent::ViewportRatio(0.5);
        assert!(intent.validate().is_ok());
        assert_eq!(
            project_column_width(intent, intent, None, []),
            Err(WidthProjectionBlocked::UnknownViewport)
        );
        assert_eq!(
            project_column_width(intent, intent, Some(1800), [])
                .unwrap()
                .slot,
            900
        );
    }
    #[test]
    fn incompatible_or_unsupported_constraints_block() {
        let intent = WidthIntent::Absolute(800.0);
        assert_eq!(
            project_column_width(
                intent,
                intent,
                None,
                [
                    WidthConstraint::Interval {
                        min: 1000.0,
                        max: 1200.0
                    },
                    WidthConstraint::Interval {
                        min: 500.0,
                        max: 900.0
                    }
                ]
            ),
            Err(WidthProjectionBlocked::ConflictingConstraints)
        );
        assert_eq!(
            project_column_width(intent, intent, None, [WidthConstraint::Unsupported]),
            Err(WidthProjectionBlocked::UnsupportedConstraint)
        );
    }
    #[test]
    fn inheritance_follows_configuration_but_explicit_equal_width_does_not() {
        for default in [800.0, 900.0] {
            assert_eq!(
                project_column_width(
                    WidthIntent::InheritConfig,
                    WidthIntent::Absolute(default),
                    None,
                    []
                )
                .unwrap()
                .slot,
                crate::util::round_px(default)
            );
            assert_eq!(
                project_column_width(
                    WidthIntent::Absolute(800.0),
                    WidthIntent::Absolute(default),
                    None,
                    []
                )
                .unwrap()
                .slot,
                800
            );
        }
    }
}

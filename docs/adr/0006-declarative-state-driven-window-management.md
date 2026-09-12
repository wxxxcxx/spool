# Declarative, state-driven window management

Status: Accepted architectural direction, 2026-09-12. This extends [ADR 0001](0001-declarative-window-frame-pipeline.md) from geometry to Spool's overall window-management model; it does not claim the current implementation has completed that evolution.

Spool's long-term goal is declarative window management driven entirely by explicit state. Commands, configuration, and scripts express intent through state transitions; platform effects are derived from that state and reconciled against native observations, rather than becoming a second, independently authoritative control path. Future designs, implementations, and reviews must use this decision as their baseline and identify existing deviations as migration work.

## State admission and platform execution are separate

A valid edit to a known retained layout must ultimately be possible even when its Space is not visible or a native write cannot run immediately. Validate target identity and the proposed state on their own terms; evaluate visibility, accessibility, native transitions, and platform capability when deciding whether and how to execute effects. Temporary inability to apply a state must not, by itself, erase or invalidate the desired arrangement.

For example, changing the column widths in an invisible Space should eventually update its retained Layout State immediately, without switching Spaces or focusing a window. Reconciliation should apply the latest desired arrangement when the necessary native conditions are established. This retains desired state, not a queue of stale imperative actions to replay; later state changes supersede earlier intent.

Keep desired state, presented effects, and observed facts distinct. Accepted intent is not proof of native completion. Pending, blocked, failed, or unsupported realization must remain diagnosable, and reconciliation must stay bounded rather than repeatedly forcing an impossible outcome.

## Native ownership remains explicit

macOS remains authoritative for observed window lifecycle, Space topology and membership, visibility, and accepted physical geometry. Declarative requests do not fabricate those facts. External changes enter as observations and may produce deliberate state transitions under the relevant policy; they must not silently overwrite desired layout or cause unrelated imperative effects.

Existing identity, native-transition, and write-safety protections remain in force until state admission and effect execution have been correctly separated. Pure state driving does not mean every desired result is supported by macOS or guaranteed to converge.

## Temporary first-version inspection/CLI boundary

The resource-CLI first version may restrict explicit geometry and arrangement mutations to confirmed visible user Spaces, including visible secondary displays, while inspection covers all available Spaces. This is an accepted temporary implementation limitation, not a permanent rule of the domain or public architecture. It must not be encoded as an inherent requirement that a valid desired layout can only change while visible.

Future work should remove that coupling by separating state edits from deferred platform effects. Until then, report rejected mutations honestly; do not claim the invisible-Space state-edit capability is already implemented.

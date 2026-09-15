# Declarative, state-driven window management

Status: Accepted architectural direction, 2026-09-12. This extends [ADR 0001](0001-declarative-window-frame-pipeline.md) from geometry to Spool's overall window-management model; it does not claim the current implementation has completed that evolution.

Spool's long-term goal is declarative window management driven entirely by explicit state. Commands, configuration, and scripts express intent through state transitions; platform effects are derived from that state and reconciled against native observations, rather than becoming a second, independently authoritative control path. Future designs, implementations, and reviews must use this decision as their baseline and identify existing deviations as migration work.

## State admission and platform execution are separate

A valid edit to a known retained layout must ultimately be possible even when its Space is not visible or a native write cannot run immediately. Validate target identity and the proposed state on their own terms; evaluate visibility, accessibility, native transitions, and platform capability when deciding whether and how to execute effects. Temporary inability to apply a state must not, by itself, erase or invalidate the desired arrangement.

For example, changing the column widths in an invisible Space should eventually update its retained Layout State immediately, without switching Spaces or focusing a window. Reconciliation should apply the latest desired arrangement when the necessary native conditions are established. This retains desired state, not a queue of stale imperative actions to replay; later state changes supersede earlier intent.

Keep desired state, presented effects, and observed facts distinct. Accepted intent is not proof of native completion. Pending, blocked, failed, or unsupported realization must remain diagnosable, and reconciliation must stay bounded rather than repeatedly forcing an impossible outcome.

Which state is retained under this rule, and which is repaired instead, is decided in the next section.

## Retained state is a choice, not a default

Retaining desired state is only justified when the user cannot cheaply retry at the moment the condition becomes true, and when the blocked state is common and long-lived. Column widths, stack heights and arrangement meet that test: a Space's layout cannot be written while it is invisible, the user cannot visit and return merely to resize a column, and the desired value stays meaningful for as long as the window and column exist. Their retained intent is realized when the Space is shown.

**Target Space membership does not meet it.** Its failures are "not permitted right now" — the capability is off, a move is already in flight, the display is unreachable — the user can see the target and retry cheaply, and a target whose Space does not exist cannot be validated at all. Membership therefore has no waiting target:

- an edit is accepted only when its target is a valid, existing user Space **and** its effect can be attempted now; otherwise it is refused with a specific reason rather than held;
- an attempt runs once, and an unconfirmed attempt repairs the declared state to the observed reality instead of retrying;
- when an external event invalidates membership state that was valid — a Space destroyed or merged, a display detached, a window moved or replaced — the state is repaired to what observation shows, never rewritten to a target the user never chose.

Repair changes state only; performing a native move is the effect layer's work, derived from state. A failed read is unknown rather than absence, and a Space re-created under a new identity is never silently adopted.

## Native ownership remains explicit

macOS remains authoritative for observed window lifecycle, Space topology and membership, visibility, and accepted physical geometry. Declarative requests do not fabricate those facts. External changes enter as observations and may produce deliberate state transitions under the relevant policy; they must not silently overwrite desired layout or cause unrelated imperative effects.

Existing identity, native-transition, and write-safety protections remain in force until state admission and effect execution have been correctly separated. Pure state driving does not mean every desired result is supported by macOS or guaranteed to converge.

## Temporary first-version inspection/CLI boundary

The resource-CLI first version may restrict explicit geometry and arrangement mutations to confirmed visible user Spaces, including visible secondary displays, while inspection covers all available Spaces. This is an accepted temporary implementation limitation, not a permanent rule of the domain or public architecture. It must not be encoded as an inherent requirement that a valid desired layout can only change while visible.

Future work should remove that coupling by separating state edits from deferred platform effects. Until then, report rejected mutations honestly; do not claim the invisible-Space state-edit capability is already implemented.

Removing that coupling does not create a waiting target for membership. An edit whose effect cannot be attempted is still refused and reported, per the section above; the temporary boundary governs whether a *retained* layout edit may be admitted while its Space is invisible, not whether an unrealizable target should be held.

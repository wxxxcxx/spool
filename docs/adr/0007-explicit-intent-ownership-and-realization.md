# Explicit intent ownership and bounded realization

Status: Accepted design, 2026-09-13; all map decisions resolved through authorized expert consensus or explicit user decisions. Column-width, arrangement and stack-height slices implemented. Focus activation slice implemented with conservative bounded evidence; see the [focus slice](../wayfinding/declarative-state/issues/21-focus-activation.md). Space membership implemented as an invariant with repair ([issue 23](../wayfinding/declarative-state/issues/23-space-membership.md)), and floating frames implemented as retained intent with a derived target ([issue 24](../wayfinding/declarative-state/issues/24-floating-windows.md)).

## Context

[ADR 0006](0006-declarative-state-driven-window-management.md) establishes the declarative direction. At the time of the decision, layout width was derived from window frames and state admission was coupled with native execution conditions. Separating intent without assigning writers would leave competing authorities and permit observed constraints or delayed effects to rewrite user choices.

## Decision record

The canonical detailed decisions are the local tracker tickets linked by [Spool 声明式状态模型与迁移边界](../wayfinding/declarative-state/map.md). They distinguish direct user confirmation from unanimous expert decisions accepted under the user's explicit authorization. This ADR is an architectural entry point, not a duplicate decision store.

- [Intent and observation ownership](../wayfinding/declarative-state/issues/05-state-ownership.md) assigns domain writers and atomic placement/layout transitions.
- [Constraint projection](../wayfinding/declarative-state/issues/03-constraints.md) retains original intent while deriving effective targets.
- [Bounded coordination](../wayfinding/declarative-state/issues/07-reconciliation.md) separates acceptance, realization and durability, with versioned attempts rather than stale command replay.
- [Geometry representation](../wayfinding/declarative-state/issues/17-layout-intent.md) makes column width intent explicit.
- [Migration and verification](../wayfinding/declarative-state/issues/10-migration.md) defines the first complete slice, finalized policies and required validation.

## Consequences

The first slice is a deliberate replacement of tiled-column width authority, including old feedback paths; adding parallel state fields is insufficient. Development does not require old-format or old-interface compatibility. Actual cross-daemon restoration remains outside this slice, which provides intent persistence and isolated candidates only.

This design increases explicit state and provenance, in exchange for keeping deferred intent meaningful and making incomplete effects diagnosable. It does not guarantee every macOS outcome is realizable. All 19 map decisions are resolved and the planning map is closed; this ADR must not be used to claim implementation or native validation is complete.

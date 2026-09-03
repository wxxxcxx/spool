# ADR 0001: Declarative Window Frame Pipeline

- Status: Accepted
- Date: 2026-09-03

## Context

Window geometry previously shared `Position` and `Bounds` across layout, animation, macOS writes, external notifications, overlays, and reconciliation. Multiple systems could therefore reinterpret an intermediate or stale value as authoritative state. Missed close notifications left visual surfaces alive, constrained writes could desynchronize borders, external resize bursts repeatedly disturbed neighbours, and fullscreen or Space transitions could race ordinary layout commits.

Spool needs React-like directionality for geometry without claiming ownership of facts that macOS controls. Spool owns tiled arrangement and desired geometry. macOS owns window lifecycle, Native Space topology and membership, and the physical frame it accepted.

## Decision

Use three explicit frame projections:

1. `DesiredWindowFrame` is rendered from Layout State and is the final target.
2. `PresentedWindowFrame` is derived from Desired by animation or an explicit snap and is the only normal-operation commit input.
3. `ObservedWindowFrame` is updated only from successful macOS readback and is used by borders, public queries, tab detection, and drift comparison.

External tiled-window move and resize observations do not directly rewrite layout. They update Observed immediately and are coalesced into one Layout State change after a 150 ms quiet period and mouse release. Floating windows may adopt confirmed geometry immediately because they have no tiling neighbours.

Lifecycle and Native Space reconciliation remain OS-authoritative. Fullscreen transitions, Space reassignment, unavailable windows, and graceful exit remove or suspend ordinary frame motion before another owner acts. Reconciliation retries desired/observed drift with a bounded budget and cooldown.

`Position` and `Bounds` remain temporary compatibility inputs for `LayoutStrip`; they no longer represent the physical frame or directly drive macOS commits.

## Consequences

- Layout changes are deterministic: state changes first, then projections converge in one direction.
- Animation cannot corrupt the final layout target, and AX readback cannot silently become tiled state.
- Borders follow confirmed surfaces and disappear when confirmation is unavailable.
- Mouse and application resize bursts cause one neighbour reflow instead of one per notification.
- Native fullscreen and Space transitions have explicit geometry ownership boundaries.
- There are temporarily more frame components and adapters while legacy layout math still uses `Position` and `Bounds`.
- Tests must assert the appropriate projection rather than treating every frame-like value as interchangeable.

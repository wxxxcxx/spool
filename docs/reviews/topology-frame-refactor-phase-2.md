# Topology and frame refactor: phase 2

This phase follows `topology-frame-refactor-phase-1.md` and centralizes the
observation used by topology projections. It does not yet merge every lifecycle
mutation into one reconciler.

## Shared observation

`src/ecs/topology.rs` owns `NativeTopology`. Each observation epoch contains:

- A generation number, including for failed observations.
- The physical display inventory result and each display's Space-list result.
- Per-display visibility results, the active display, and fullscreen Space IDs.
- Whether an explicitly empty physical inventory may be accepted.

Startup samples before gathering displays. During ordinary frames, native Space
commands run after event pumping, followed by one topology sample when invalidated
or when the one-second heartbeat expires. Missing-Space detection then consumes
that sample. Display reconciliation, Space projection, destroyed-Space recovery,
orphan recovery, initial placement, and session restore share the same resource.

Consumers do not independently retry a failed topology query during that epoch.
They may retain their ECS projection, but the resource does not substitute a
previous successful observation as current evidence. Session-restore retries
request a new sample through the shared sampler instead of making a private read.

Unchanged `NativeSpace` values are no longer marked changed on every heartbeat.
Debug logging no longer issues extra Space-membership queries.

## Consistency boundary

This is one published observation for ECS consumers, not an atomic macOS
transaction: platform calls within sampling can still observe an OS transition.
Window membership is deliberately queried separately before accepting migration;
the shared display/Space inventory alone never proves a window's destination.
Destruction recovery waits if any display's Space inventory is unknown.

The native command admission check and platform command adapter still perform
fresh topology validation before requesting OS mutations. Those reads do not
publish competing layout projections.

The existing startup, display, Space, orphan, and restore projection mutation
paths remain separate. Centralizing their observation is a prerequisite to
consolidating their ownership, not a claim that consolidation is finished.

## Regression coverage

- One invalidation now produces one lifecycle topology sample instead of three.
- A failed inventory advances the observation generation, preserves the display
  entity, and exposes no successful topology as membership evidence. A heartbeat
  recovers without a new OS notification.
- A partial Space-list failure is shared by every consumer in the same frame;
  another consumer cannot silently consume a second, successful mock response.
- The existing transient-source-omission test explicitly invalidates topology
  and waits for the shared heartbeat, instead of depending on private per-consumer
  retry counts. Its tombstone-preservation assertion remains intact.
- The repeated-destruction test injects visibility failure into the shared
  observation, rather than expecting an independent second read to fail. It
  still verifies retention on failure and cleanup only after recovery.

## Remaining phases

- Consolidate Space creation, reparenting, and retirement into one lifecycle owner.
- Unify bounded corrective geometry writes with the presentation committer.
- Add transition replay for partial column moves, destruction, unavailable
  members, and native ID reuse.
- Address Lua generations/caches and the macOS FFI threading findings.
- Perform real multi-monitor acceptance after explicit deployment approval.

No installation, daemon restart, live window manipulation, or git commit was
performed in this phase. Existing research files were left untouched.

## Verification

- `cargo test --workspace --locked --quiet -- --test-threads=1`: 571 passed,
  2 ignored across all workspace crates. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

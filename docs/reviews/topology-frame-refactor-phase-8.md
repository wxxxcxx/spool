# Topology and frame refactor: phase 8

This phase follows `topology-frame-refactor-phase-7.md` and adds deterministic
transition replays for native fullscreen recovery and window-ID reuse in gestures.

## Fullscreen reconciliation

- `reconcile_fullscreen_spaces` replaces the event-only handler. It responds to
  SpaceChanged hints and new shared topology observations, including the existing
  heartbeat. A lost notification or failed membership read no longer requires
  another user Space switch to recover.
- Ordinary active-Space migration yields for projected native fullscreen Spaces,
  even before a `NativeFullscreenMarker` exists. Only the fullscreen reconciler
  transfers the window and captures its source strip and column index.
- Membership is deduplicated and must identify one tracked source candidate.
  Multiple candidates retain their original layout until a later unique read;
  the reconciler does not choose one based on iteration order.
- Layout calculation remains ordered after fullscreen reconciliation.

## Gesture identity

The modifier-resize gesture now retains both its ECS entity and AX incarnation,
in addition to the native numeric ID. A replacement window cannot inherit an
unfinished gesture. Releasing the modifier clears that identity so a subsequent
gesture can select the replacement normally. Commit-time incarnation validation
from phase 5 remains in place as a separate last check.

## Regression coverage

Four new mock-ECS replays cover:

1. An entire fullscreen-transition frame with unknown membership retains the
   source layout; subsequent heartbeat recovery records the original index, and
   exiting fullscreen restores that index.
2. A lost SpaceChanged event still converges through the topology heartbeat.
3. Ambiguous fullscreen membership preserves both source windows until a unique
   read permits migration of the actual fullscreen window.
4. A numeric window ID reused during a held modifier gesture does not receive a
   resize. Releasing the modifier and starting a new gesture works normally.

The lost-event and reused-ID tests failed against the previous implementation.
The unavailable/ambiguous membership scripts span the whole transition frame:
focus restoration also reads membership, so injecting only one failed call
would make the replay depend on internal reader ordering.

## Boundaries

These are mock transition replays, not real hotplug or fullscreen acceptance.
Fullscreen migration still requires a tracked tiled source; startup fullscreen
windows without such a source retain the existing deferred-default lifecycle.
Unresolved fullscreen reads retry at topology observation cadence rather than
running a new per-frame query loop.

Broader multi-window native move replay, Lua generation/cache findings, and macOS
FFI threading findings remain separate work. No installation, daemon restart,
live window manipulation, or git commit was performed. Existing research files
were left untouched; physical multi-display acceptance still requires approval.

## Verification

- `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`:
  593 passed, 2 ignored. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

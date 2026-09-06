# Topology and frame refactor: phase 5

This phase follows `topology-frame-refactor-phase-4.md` and moves modifier-driven
mouse resizing into the presentation committer. Default-parameter application
and shutdown restoration are deliberately not changed in this phase.

## Changes

- Mouse input now computes `InteractiveWindowFrame` requests without writing AX
  geometry or changing observed/layout projections. Multiple events for the same
  window in one frame accumulate into one target and one physical commit.
- Each request carries the pre-gesture layout frame and window incarnation. The
  committer validates incarnation and native ownership at execution time.
- Pointer intent takes priority over animation and audit correction for that
  commit, but never bypasses unavailable-window, Space-migration, or fullscreen
  guards. Blocked requests are consumed, not retained for later replay.
- Tiled windows still track pointer input immediately. Only confirmed readback
  enters the geometry-settling ledger; neighbouring columns wait for the existing
  quiet period before reflowing. Floating windows adopt confirmed position, size,
  and desired geometry immediately.
- Interactive writes use the same complete-frame adapter and failed-write
  suspension/readback handling as corrective writes. A partially successful AX
  setter does not silently become accepted tiled layout intent.
- Readback updates presentation without scheduling a second write. A new pointer
  event may make a fresh attempt; the failed request itself is never replayed.

## Regression coverage

Four new mock-ECS regressions cover:

1. A three-event input burst accumulates its full width delta but performs one
   frame write. The previous direct writer failed with two writes.
2. Space migration beginning after input but before commit prevents the physical
   write and discards the queued gesture.
3. A setter that changes physical geometry but fails its final readback preserves
   fresh physical truth, suspends commits, and does not settle speculative intent.
4. A floating-window resize publishes confirmed intent and does not replay it in
   subsequent frames.

The existing live-resize/deferred-neighbour regression and all prior topology,
correction-budget, animation, fullscreen, and session-restore tests still pass.

## Remaining boundaries

Production geometry setters under `src/ecs` now remain in the normal committer,
default-parameter application (`triggers.rs`), and final shutdown restoration
(`exit_restore.rs`). Defaults need their own initialization/retry review; final
shutdown restoration must remain protected from later layout writes.

Transition replay, native ID reuse across longer-lived gestures, fullscreen
retry behavior, Lua generation/cache findings, and macOS FFI threading findings
remain separate work. Real multi-display hotplug and gesture acceptance still
requires explicit deployment approval.

No installation, daemon restart, live window manipulation, or git commit was
performed. Existing research files were left untouched.

## Verification

- `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`:
  581 passed, 2 ignored. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

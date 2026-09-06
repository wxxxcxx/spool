# Topology and frame refactor: phase 7

This phase follows `topology-frame-refactor-phase-6.md` and bounds repeated
initialization attempts, including preparation reads rather than only AX writes.

## Retry policy

`ecs::defaults::DefaultRetries` admits three fast attempts per window instance.
After the third attempt, a five-second cooldown precedes the next burst. The
admission point is before fullscreen, resizability, frame, and membership reads
in default preparation; denied attempts do not enter that preparation path.
Transient failures can still recover on the following Update within the fast
budget, preserving the existing initialization transaction timing.

The budget is not keyed by a recomputed target rectangle. An application moving
its origin, or a setter partially succeeding before reporting an error, cannot
continually create new budgets. Ordinary events and topology sampling generations
also do not reset attempts.

## Reset and lifetime

- Bevy configuration change detection clears the current initialization budgets.
- A changed successful topology projection clears them: the comparison includes
  physical display IDs, usable bounds, and ordered native Space IDs. Identical
  heartbeat observations do not count; incomplete observations do not replace
  the last successful context. Pending work may wait through its remaining
  cooldown when observation merely recovers without a semantic context change.
- Applied, removed, unavailable, migrating, fullscreen-deferred, or replaced
  window instances lose their own entries. On resume, that instance gets a fresh
  fast budget without resetting unrelated windows.
- With no pending default transactions, the ledger is cleared and the system
  skips building a topology comparison.

Successful default writes still use the shared committer and only then publish
`WindowDefaultsApplied`. The tiled correction budget remains independent.

## Regression coverage

Five new mock-ECS tests cover:

1. Repeated audit events and topology heartbeats produce three failed default
   writes over forty frames, then allow recovery after cooldown. The previous
   implementation failed this test with forty writes.
2. Configured grid changes and physical display geometry changes each restart a
   cooling transaction and apply the new target immediately.
3. Repeated frame-read failures are also throttled, then recover.
4. Changing physical origins after partial default writes does not restart the
   initialization budget.
5. Resuming one unavailable window leaves another pending window's cooldown intact.

## Boundaries

This is per-window retry throttling, not a global AX work budget. Configuration
change signals and genuinely changed complete topology can start new bursts.
The topology comparison is shared across pending windows; it does not attempt
to prove that a display change affects only one particular transaction.

Native transition replay, fullscreen retry behavior, native ID reuse, Lua cache
and generation findings, and macOS FFI threading findings remain separate work.
No installation, daemon restart, live window manipulation, or git commit was
performed. Existing research files were left untouched. Real multi-display
acceptance still requires deployment approval.

## Verification

- `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`:
  589 passed, 2 ignored. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

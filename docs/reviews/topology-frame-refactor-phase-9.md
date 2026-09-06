# Topology and frame refactor: phase 9

This phase follows `topology-frame-refactor-phase-8.md` and hardens native
multi-window move completion and deferred focus follow.

## Transaction identity and completion

- Tracked move members retain their native ID, ECS entity, and AX incarnation.
  Submission rejects columns with unavailable members. Reconciliation does not
  append a captured column after a member has disappeared or been replaced.
- Retired identity releases explicit transaction ownership, but retains the
  reassignment geometry barrier for surviving members. Temporarily unavailable
  members wait for confirmation or the existing two-second timeout.
- Completion requires every submitted native ID to belong uniquely to the
  target in a complete membership observation across known native Spaces.
  Source/target overlap and any failed membership query defer completion.
  Retiring targets and fullscreen targets cannot receive the captured column.
- One membership map is shared by all moves and follows in each reconciliation
  pass. No membership scan runs when both transaction queues are empty.
- Partial moves still time out into the existing membership audit. Recovery uses
  actual observed destinations; it does not issue speculative OS rollback moves.

## Focus follow

Deferred follow retains the original tracked identity rather than resolving a
numeric ID anew. It waits for target visibility, member availability, and unique
target membership. Identity retirement cancels it. The five-second follow limit
now uses the ECS clock, as the move deadline already did.

## Regression coverage

Four mock-ECS tests in `src/tests/native_move.rs` cover:

1. Overlapping source/target membership retains the source and barriers; a later
   unique observation commits the original stacked column structure.
2. Replacing one member cannot reinsert the retired entity or trigger Space
   follow. Surviving members retain their geometry barrier for recovery.
3. A partially successful column move times out, then each member converges to
   its actual Space without rollback or focus-follow commands.
4. A numeric ID reused while focus follow is pending does not receive focus.

The first two tests failed against the preceding implementation. The partial
move replay also preserves the previous owner-only timeout behavior.

## Boundaries

The membership map is assembled from separate OS queries, not an atomic native
snapshot. Identity validation uses current ECS tracking. Untracked associated
native surfaces still have only numeric IDs, although their membership must also
be uniquely confirmed. Conservative all-Space reads can defer a valid move when
an unrelated Space query fails; the existing timeout/audit path remains the
fallback. No new polling scheduler or platform rollback mechanism was added.

Real monitor hotplug, fullscreen, and AX timing acceptance remains outstanding.
Lua generation/cache and macOS FFI threading findings remain separate work.
No installation, daemon restart, live window manipulation, or git commit was
performed. Existing research files were left untouched.

## Verification

- `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`:
  597 passed, 2 ignored. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

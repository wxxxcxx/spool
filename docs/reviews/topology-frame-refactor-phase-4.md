# Topology and frame refactor: phase 4

This phase follows `topology-frame-refactor-phase-3.md` and moves bounded tiled
frame corrections into the normal presentation committer.

## Changes

- The window-state audit no longer calls AX geometry setters. It reads physical
  truth and submits `WindowFrameCorrection` with the audited desired target.
- The committer consumes requests after layout and animation. It rejects stale
  targets and yields to motion, native fullscreen, geometry settling, unavailable
  windows, and Space reassignment. A new presentation can supersede a correction
  without first writing the obsolete audit target.
- `WindowStateSync` retains retry policy but no longer executes writes. Admission
  counts actual correction attempts at commit time, keeps the three-attempt
  budget and five-second cooldown, and resets on confirmed convergence or a new
  target. Repeated signals and changing partial readbacks do not reset the budget.
- Corrective and ordinary writes share failure suspension and fresh readback.
  A constrained corrective readback also suspends ordinary per-frame writes, so
  verification cannot silently bypass the correction cooldown.
- Corrective readback updates presentation without marking it as a new write
  request. If the application reaches the target independently, the audit
  synchronizes presentation and resumes commits without issuing another write.
- The frame committer uses a SystemParam context. Write-strategy selection and
  failed-write readback are private helpers; admission and success publication
  remain in one commit pass.

## Regression coverage

Four new mock-ECS regressions cover:

1. Audit leaves the physical frame untouched until the presentation commit.
   This assertion failed against the former direct audit writer.
2. New desired/presented state between audit and commit produces only one write,
   to the replacement target.
3. Space reassignment beginning between audit and commit prevents the write.
4. Independent application recovery during correction suspension synchronizes
   presentation and clears suspension without another write. This caught stale
   presentation in the initial version of the new audit path.

Existing constrained-write, partial-readback, rejected-write, retry-budget,
animation, fullscreen, topology, and session-restore regressions remain passing.

## Scope and remaining work

This consolidates ordinary presentation and bounded tiled-frame correction, not
every geometry mutation in the process. Default-parameter application in
`triggers.rs`, interactive resizing in `mouse.rs`, and final shutdown restoration
in `exit_restore.rs` still own explicit writes. Their ownership, failure behavior,
and interaction with the presentation pipeline need a separate pass; shutdown
restoration must remain protected from later layout writes.

Transition replay for partial native moves, unavailable members, destruction,
native ID reuse, and event-only fullscreen retry remains pending. Lua generation
and cache findings and macOS FFI threading findings are also outside this phase.
Real multi-monitor hotplug acceptance still requires deployment approval.

No installation, daemon restart, live window manipulation, or git commit was
performed. Existing research files were left untouched.

## Verification

- Full workspace tests: 577 passed, 2 ignored. Command:
  `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`.
  IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

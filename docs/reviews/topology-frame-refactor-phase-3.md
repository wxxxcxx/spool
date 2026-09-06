# Topology and frame refactor: phase 3

This phase follows `topology-frame-refactor-phase-2.md`. It consolidates Space
creation and attachment while preserving layout identity through disconnection,
native fullscreen transitions, and session restore.

## Lifecycle ownership

- `native_space::reconcile_native_spaces` is now the only production caller of
  `spawn_layout_strip`. Startup runs it after gathering physical displays; Update
  runs it after display reconciliation and before fullscreen handling and layout.
- Display gathering, addition, and geometry updates no longer create or reparent
  Space strips. Display removal detaches child strips before deleting the parent.
- A detached strip retains `DetachedSpace { source_display_id }`, not a generic
  30-second `Timeout`. The removed orphan scanner no longer competes with the
  native projection or the global entity-expiration system.
- Floating-layer state is a component on the canonical strip entity. Removing a
  physical display no longer cascades into deletion of that state. The redundant
  copy of the Space ID has been removed.
- Session restore validates live membership and waits for canonical targets, then
  changes their columns in place. It preserves entity identity, native metadata,
  display attachment, viewport state, visibility, and activation markers. It
  refuses a fullscreen restore that would discard unmatched live members.
- Native fullscreen handling migrates a tracked member into an already projected
  target and records its source column index. It no longer depends on being the
  first system to create the target Space.

## Disconnect versus destruction

The existing missing-Space audit can now inspect detached strips without a
`ChildOf`. A complete observation can freeze missing-source windows and live
membership can establish their destination. A disconnected physical display is
not itself proof that its native Space was destroyed: an empty source tombstone
is retained until its source display is observed with the Space absent and the
existing membership reconciliation checks permit retirement.

Creation and attachment are centralized; physical detachment and confirmed
retirement remain explicit responsibilities in display reconciliation and the
membership audit. This is not a claim that every lifecycle operation is one
function, nor that multiple macOS reads form an atomic transaction.

## Regression coverage

- A simulated disconnect longer than 30 seconds preserves the same strip entity,
  window order, and floating-layer state, then reattaches it on reconnection.
  The original timeout implementation failed this regression; the separate
  display-owned floating layer failed the strengthened lifetime assertion.
- Session restore changes column order without replacing the native Space entity
  or its metadata and display parent. The previous recreate path failed this test.
- Existing fullscreen source-index, Space projection, orphan migration, tiling,
  focus-tier, and session-restore tests remain in the full regression suite.

## Remaining work

- Unify bounded corrective geometry writes with the presentation committer.
- Extend transition replay for partial column moves, destruction, unavailable
  members, and native ID reuse; audit event-only fullscreen retry behavior.
- Address Lua generation/cache and macOS FFI threading review findings.
- Perform real multi-monitor hotplug acceptance after deployment approval.

No installation, daemon restart, live window manipulation, or git commit was
performed. Existing research files were left untouched.

## Verification

- `cargo test --workspace --locked --quiet -- --test-threads=1`: 573 passed,
  2 ignored across all workspace crates. IPC tests ran outside the sandbox;
  `RUST_LOG=off` suppressed fixture logging.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

# Topology and frame refactor: phase 1

This implementation follows the full review at `2026-09-05-full-code-review.md`.
It does not claim to complete every architectural recommendation in that review.

## Implemented boundaries

- `DisplayObservation` distinguishes a failed physical inventory from an empty
  inventory, and a failed per-display Space query from a disconnected monitor.
  Display reconciliation consumes one observation per pass and audits every
  second, including when no new OS notification arrives.
- Each strip remembers its last usable viewport. Viewport translation rebases
  both its position and any in-flight scroll target, preserving the local
  scrolling offset across same-ID monitor movement and reparenting.
- An explicitly destroyed Space remains a tombstone. An implicitly omitted
  Space can resume its retained strip when it reappears in OS topology.
- Explicit native moves share the existing geometry suspension barrier.
  `NativeMoveOwner` prevents the destruction reconciler from consuming an
  active move. Confirmation releases the barrier; timeout transfers ownership
  to live-membership reconciliation while keeping geometry suspended. Move
  deadlines use Bevy time, and overlapping requests for the same window are
  rejected while a transaction remains active.
- Session restore uses complete observed topology and unique native membership
  to filter identity matches before planning columns. Saved state may restore
  order within a Space but cannot override native Space membership. Transient
  read failures retry within the existing startup grace period.
- Position verification observes the settled presented frame and requests a
  normal frame commit. It cannot write the final desired origin over an
  in-flight animation, nor bypass AX-failure suspension.
- Wake-time floating-window refresh clamps only out-of-bounds origins. It
  preserves valid positions and skips unavailable or transitioning windows.

## Regression coverage

Ten tests were added through the existing mock WindowManager / TestHarness:

1. Same-ID monitor origin changes in multiple directions and back.
2. A partial Space query cannot remove its connected monitor.
3. A temporarily omitted Space resumes its retained strip.
4. The position verifier cannot overwrite an animation frame.
5. Wake preserves a valid floating-window position.
6. Restore cannot override membership on an inactive Space.
7. Native move submission suspends source geometry before confirmation.
8. Native move timeout retains suspension until membership queries recover.
9. Heartbeat discovers a monitor without a connection notification.
10. Restore retries a membership read failure without another notification.

The seven original failure probes were reproduced before their corresponding
fixes. Existing exit tests now keep mock OS topology consistent with their ECS
fixtures. The pending-source restore test now also asserts that membership must
be observed before restoration consumes its source layout.

## Remaining work

- Replace the multiple topology readers and lifecycle reconcilers with one
  versioned observation snapshot and one Space lifecycle owner. The legacy
  `present_displays` compatibility view still exists; this phase does not make
  every topology consumer atomic.
- Route the central reconciler's bounded corrective writes through the common
  commit path without losing partial-AX-write readback or retry budgets. Removing
  the verifier's competing writer is not yet a repository-wide sole-writer proof.
- Expand transaction replay coverage for partial column moves, concurrent
  destruction, unavailable members, and reused native IDs. Generalize the
  remaining wall-clock timers only with these behavior tests in place.
- Address the review's Lua runtime-generation / dispatch-cache findings and
  AppKit / AX worker-thread boundaries separately.
- Validate real monitor unplug/replug, main-display changes, sleep/wake, and
  floating-window interactions on the installed daemon. No live desktop
  operations, installation, daemon restart, or git commit were performed here.

## Verification

- `cargo test --workspace --locked --quiet -- --test-threads=1`: 568 passed,
  2 ignored across all workspace crates. Local IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check --workspace --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Workspace-wide `--no-default-features` is not a supported no-Lua check: the
  independent Lua crate requires an explicit Lua backend. The daemon-only
  command above verifies the intended no-Lua build.

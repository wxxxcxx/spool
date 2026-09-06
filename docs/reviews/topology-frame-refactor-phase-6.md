# Topology and frame refactor: phase 6

This phase follows `topology-frame-refactor-phase-5.md` and routes configured
default geometry through the shared frame committer.

## Changes

- Default application prepares `DefaultWindowFrame` requests rather than calling
  AX geometry setters. Preparation runs after native Space projection. An Update
  commit phase executes these requests before window placement, including during
  initialization; the PostUpdate phase still owns presentation and corrections.
  Both phases use the same executor, write adapter, and failed-write readback.
- A request includes the window incarnation. Execution respects unavailable,
  Space-reassignment, and native-fullscreen guards. Only a successful returned
  frame publishes `WindowDefaultsApplied`; failures retain pending defaults for
  retry. Pending defaults cannot leak into ordinary presentation commits.
- Configured grid and width geometry resolve their owning display through a
  complete topology observation and uniquely confirmed native Space membership.
  Failed or ambiguous membership waits rather than falling back to the active
  display. The current projected display supplies usable bounds and Dock padding.
- Grid geometry uses origin plus size, not width/height as absolute maximum
  coordinates. Startup configured widths now use the owning display.
- The Floating-add observer still removes strip membership, but does not apply
  another active-display grid while the default transaction owns geometry. This
  competing observer was found when the new multi-display regression initially
  continued failing after the default target calculation had been corrected.
- An already-confirmed normal presentation frame is not written again merely
  because a component was marked changed during default placement.

## Regression coverage

Three new mock-ECS tests cover:

1. A floating grid on an inactive display with a nonzero origin fills that
   display's usable rectangle and needs one frame write. The original path moved
   it to the primary display instead.
2. A configured startup width uses the inactive owning display's width rather
   than the active display's dimensions.
3. Failed and ambiguous Space membership both preserve pending defaults without
   a physical write, then apply them once unique membership becomes available.

Existing transient-default-write failure, unavailable-window pause/resume,
fullscreen deferral, initialization, geometry, topology, and restore tests remain
part of the full suite.

## Boundaries

Direct geometry setters under `src/ecs` now exist only in the shared committer
and final shutdown restoration. Shutdown remains deliberately separate so later
layout or animation cannot overwrite the restored launch frame.

Default retry cadence is preserved: a pending available window can retry in the
next Update. This phase does not give defaults the tiled-correction three-attempt
budget; sustained initialization failures still need a separate retry-policy pass.
The two commit phases preserve initialization ordering, not a promise of a single
physical write for every possible layout change across an entire application frame.

Longer transition replay, native ID reuse, fullscreen retries, Lua generation/cache
findings, and macOS FFI threading findings remain separate work. No installation,
daemon restart, live window manipulation, or git commit was performed. Existing
research files were left untouched; real multi-monitor acceptance remains pending.

## Verification

- `RUST_LOG=off cargo test --workspace --locked --quiet -- --test-threads=1`:
  584 passed, 2 ignored. IPC tests ran outside the sandbox.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo check -p spool --locked`: passed.
- `cargo check -p spool --locked --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

# Release Readiness

Date: 2026-09-07. Starting checkpoint: `e2c4406f0c50509d3beae2cb066b4b8b0c25f209`
plus the existing uncommitted working tree. Package version: `0.4.4`.

## Scope

Review the complete candidate, preserve existing work, and iterate through
reproduction, focused fixes, regression tests, and independent review.
Architecture standards and documented behavior are separate review axes.
No release number bump, commit, installation, daemon restart, live window
manipulation, signing credential use, or publication is implied by this work.

## Gates

- [x] Standards review: platform threading, lifecycle, and architecture.
- [x] Spec review: window policy, native Spaces, restore, and Bar projection.
- [x] Resolve Lua hot-reload ownership and overlapping snapshot defects.
- [x] Formatting and strict Clippy pass for the current native feature matrix.
- [x] Current workspace tests and Lua-free tests pass, with ignored tests explained.
- [x] Current locked native release builds and non-mutating CLI smoke tests pass.
- [ ] Installation, supported platforms, upgrade behavior, and CI agree.
- [ ] Final regression review finds no known unaddressed release blockers.
- [ ] Authorized live macOS acceptance covers monitor hotplug, fullscreen,
      sleep/wake, drag/resize, Space transitions, and Bar interaction.

## Evidence

- Initial `cargo fmt --all -- --check`: passed.
- Initial sandboxed workspace tests: 569 passed, 6 IPC permission failures,
  2 ignored. The six failures were `Operation not permitted` while binding
  temporary Unix sockets. Approved unsandboxed rerun: 639 workspace tests
  passed, 2 ignored (575 daemon + 11 IPC + 6 Lua client + 47 shared types).
- `cargo test -p spool --locked lua:: -- --test-threads=1`: 61 passed after
  the Lua changes. Both defects were reproduced by failing regressions before
  their fixes. Coverage includes pending reads in later input batches,
  old callbacks retaining snapshots, and no top-level access while suspended.
- `cargo test -p spool --locked ax_ownership_tests`: 2 passed, checking owned
  Create/Copy transfer and borrowed-value retain behavior with real AX values.
- `cargo test -p spool --locked platform:: -- --test-threads=1`: 41 passed,
  including notification rollback/teardown, short payload guards, synchronous
  KVO registration, and installer plist regressions.
- The standalone LuaJIT module built in locked native release mode and loaded
  into the existing Nix LuaJIT host and a debug CLI interpreter without daemon
  access. Loading through the final release CLI is part of the release gate.
- Focused policy/Space/restore validation: 102 tests passed (12 policy, 7 native
  move, 16 restore, 67 interaction). A further regression confirmed that a
  deferred fullscreen window recovers when AX settles after the final native
  event; the existing topology heartbeat supplies the retry.
- Prior review reports are historical evidence, not verification of this
  candidate. Mock ECS tests do not establish live WindowServer behavior.

### Iteration 1 Native Run

Host: Apple Silicon, macOS 26.6.2 (25G83), Rust 1.95.0. The approved native
release gate passed formatting, locked workspace/Lua-free compilation, strict
Clippy for workspace/all targets, Lua-free/all targets, and the standalone Lua
module. Workspace tests: 685 passed, 2 ignored (621 daemon, 11 IPC, 6 Lua client,
47 shared types). Lua-free daemon: 539 passed, 2 ignored.

The ignored tests query the private bridged Space-move class and construct its
operation object; they require macOS 26.4+ and remain outside the automated
gate. No actual native move, installation, or daemon startup was tested.
The final complete gate exited successfully after the last production edit.
Both release variants passed version/help smoke tests, and the default release
CLI loaded the standalone module without daemon access. All three SHA-256
checks passed, including an independent second verification. `file` confirmed
ARM64 Mach-O artifacts; external load dependencies are system frameworks and
libraries. The executable deployment metadata is 11.0, matching Rust's default,
not evidence that macOS 11 is a supported runtime.

The first release build caught a Lua-free link failure: Carbon process-event
symbols were declared without a framework dependency and had depended on the
Lua-only keyboard-layout module to provide it. The process FFI now declares
Carbon directly. The gate also aligns native dependency deployment targets
with Rust instead of mixing the current SDK's default with Rust's default.
The full rerun passed after this production fix. This run validates the
Iteration 1 snapshot; the subsequent Iteration 2 changes require a fresh gate
and are not covered by the artifact hashes below.

Iteration 1 artifacts were written under
`target/release-check/aarch64-apple-darwin/`. These historical hashes are
superseded at those paths by the Iteration 2 artifacts recorded below:

```text
bbf62d73b0a07a0aacaa4f160268d39db0978d56e78e77a3eaad201c8ae81d0a  default/spool
8782e17a500d0d74890e5116faefb416a30d258abcf81b4329a870de0a7d2e56  without-lua/spool
d319689027f81b30fa2041957523e2e5bc44104a7db408b87721370d92ee899e  lua/spool.so
```

Local logs: `/tmp/spool-release-gate-20260907.log` preserves the initial linker
failure; `/tmp/spool-release-gate-20260907-final.log` records the successful
complete rerun.

## Iteration 1

Lua fixes are implemented. Retired runtimes no longer publish handler
availability. Explicit `DispatchBatch` snapshots replace caches whose lifetime
was tied to all overlapping callbacks; poll-scoped access prevents suspended
callbacks from exposing their world access to reload top-level code.

### Standards

Independent static review identified KVO synchronous reentry, notification
context lifetime and eager short-packet reads, non-inherited AX timeouts,
off-thread discovery/startup scheduling, and extra retains of owned AX values.
Notification and KVO lifecycle regressions now pass. Create/Copy ownership
callers no longer add redundant retains. Discovery is now a resumable NonSend
queue: AX reads are staged, applications share bounded active-work allowances,
and application exit/identity changes cancel stale results. Startup and frame
schedules execute inline; the event pump stays responsive while initialization
waits for the last discovery publication. Timeout initialization uses the
system-wide AX object and propagates failure.

Integrated tests cover finite/fair probe work, cancellation and ownership,
calling-thread execution, startup scheduling, pump pacing, and timeout errors.
Soft per-tick deadlines cannot preempt a native call; actual AX latency remains
a desktop acceptance requirement.

### Spec

Independent static review identified capability recovery selecting the active
Space, fullscreen/unknown capability failing to recover, floating native moves
entering the strip, and fallback session restore losing to initial preferences.
All four now have passing mock regressions after their reproduced failures.
Ambiguous memberships remain deferred, floating moves make no geometry writes,
and applied restore ownership is scoped to the exact window incarnation.
These mock results are not evidence of a reproduced or fixed live desktop
failure; final integration review and desktop acceptance remain separate.

### Independent Regression Review

A second read-only pass over the Lua changes, installer/bundle ownership,
release script, module smoke test, and CI matrix found no confirmed introduced
regressions. The delayed-fullscreen suspicion was checked against the topology
heartbeat and a passing regression, not counted as an additional defect.

## Iteration 2

The follow-up concentrates on reproducible bugs and boundary handling.
Production fixes are complete. The full integration gate passed formatting,
strict Clippy, and locked compilation for the feature matrix. Workspace tests:
706 passed, 2 ignored (642 daemon, 11 IPC, 6 Lua client, 47 shared types).
Lua-free daemon: 556 passed, 2 ignored. The same two private Space-operation
tests remain outside the automated acceptance scope. Both native release builds,
CLI version/help smoke checks, and standalone LuaJIT module loading passed.
The gate exited successfully after the final production edit.

### Lua and Timers

- A keypress captured before reload dispatched a different callback after
  reload because callback IDs restarted at one. The worker regression failed
  with `Window(Balance)` where only the current binding should execute.
  Process-unique IDs now prevent replay into replacement callbacks; failed
  reloads keep the published bindings intact, even after partial registration.
- A negative `spool.flash` duration reached the ECS command path and panicked
  in `Duration::from_secs_f32`. Lua now rejects negative, NaN, infinite, and
  unrepresentable durations with a catchable error. Worker messages and timers
  carry `Duration` directly. Tests also cover zero, fractional/default seconds,
  nanosecond precision, and the maximum typed duration.
- Focused Lua validation: 64 passed. Main-thread schedule/timer validation:
  2 passed. Both newly confirmed defects had failing regressions before fixes.
  A further runtime-level test independently checks stale, zero, and maximum
  callback IDs; the complete gate covers it alongside the worker regression.

### Standards

Two independent findings were reproduced and fixed: deferred admission discarded
a discovered offscreen window after a temporary metadata failure, and final
publication discarded a resolved window after one unknown process-liveness
result. Both regressions first returned no window instead of window 42.
Metadata retries retain the known token within the original application budget.
Publication retries retain the resolved native object for bounded revalidation.
Both paths distinguish rejection/invalidation from uncertainty and share finite
tick limits. The 20 discovery tests cover retry exhaustion, sibling progress,
cancellation, the last token, slow calls, and the final-publication startup guard.
Three discovery lifecycle integration tests also passed.

### Spec

Two independent findings were reproduced and fixed: delayed manual retile chose
the active Space instead of the window's actual native owner, and a fallback
restore lost its candidate to initial floating defaults after a failed membership
query. Retile now resolves complete unique membership and the owning display's
bounds. Pending restore eligibility holds initial defaults for the exact window
incarnation until recovery or grace expiry, without overriding a later explicit
floating choice.

Focused validation passed 104 tests: 19 policy-related, 18 restore, and 67
interaction. Additional red/green checks caught target-display geometry using
600px instead of 1000px, dependence on an active display, and a later explicit
floating choice being undone by the held defaults transaction.

### Independent Regression Review

A separate read-only review of this round's Lua callback IDs, flash validation,
typed durations, reload failures, overflow, and Lua-free conditional compilation
found no confirmed introduced regression. Native desktop behavior remains outside
these mock and static results.

The first integration run stopped at strict Clippy after the expanded floating
observer exceeded parameter/length limits. A cohesive `FloatingTransactions`
system parameter now owns its restore/default transaction decision. The complete
test matrix above ran after this final production edit.

### Iteration 2 Artifacts

`file` confirmed all three current artifacts are ARM64 Mach-O. The gate's
SHA-256 verification and an independent second verification both passed.
The default CLI loaded the standalone module without accessing a daemon.
The current files under `target/release-check/aarch64-apple-darwin/` are:

```text
8a5d3294df90fd3423d4f2f62e6e215bb4ed6ab3b4dafcb4242d694626501e3f  default/spool
95a61b385a593dd62b2424c5db47e97153a4a6bc7ba9da2bae8f2348228d9028  without-lua/spool
d319689027f81b30fa2041957523e2e5bc44104a7db408b87721370d92ee899e  lua/spool.so
```

Local logs: `/tmp/spool-release-gate-20260907-iteration2.log` records the initial
Clippy failure; `/tmp/spool-release-gate-20260907-iteration2-final.log` records
the successful complete rerun. Only documentation was edited after that run.

## Packaging

- Nix default `meta.mainProgram` evaluated to `spool`, as required. No executable
  metadata change was necessary.
- The Intel overlay evaluated to an `aarch64-darwin` package before the fix.
  It now selects the requested host architecture. Package checks validate
  executable names, default/Lua-free variants, and overlay architecture.
- Installer regressions reproduced malformed XML for executable paths containing
  `&`/`<` and false ownership matches outside `CFBundleIdentifier`. Structured
  plist conversion/extraction fixes both, with no service installation or
  startup during validation.
- A reusable native release gate and ARM/Intel CI matrix now cover strict
  feature checks, serial tests, both CLI variants, and Lua module loading.
  Shell syntax and workflow YAML passed local validation. The native ARM gate
  completed locally; no remote CI or Intel execution is implied.
- **Open:** the locked nixpkgs input identifies itself as 26.11 and rejects
  `x86_64-darwin`. Intel Nix checks therefore cannot evaluate with the current
  lock. No input/lock update or platform-support reduction has been made.
- Git-backed Nix source evaluation omitted the current untracked Rust modules,
  including `src/manager/discovery.rs` and `src/window_policy.rs`. Package
  metadata evaluation is not evidence that this working candidate builds through
  Nix. No staging, commit, or source-export change was made.
- *(Later, 2026-09-15.)* The first half of that finding is closed: the work is
  committed now, so the Git-backed flake source carries every module. Re-checked
  at `c3ab0f8`: the evaluated source at
  `/nix/store/h6yq4cahfgddfja1b1qbw6ah7rvzv8ar-source` holds 128 `.rs` files,
  the same count the working tree has, including both modules that were missing
  and the floating-geometry module added since; `nix flake show` evaluates all
  aarch64-darwin outputs, `.#spool.version` is `0.4.4`, `meta.mainProgram` is
  `spool`, and the package-contract, launch-arguments, darwin-module and devShell
  outputs all resolve to derivations.
  The second half is still open and now scoped: `nix build .#spool` plans **615
  derivations** (the vendored Rust dependency tree, Bevy and LuaJIT included),
  which is a cold build and was not run. The Intel half of the finding above is
  unchanged: the lock still identifies itself as 26.11 and rejects
  `x86_64-darwin`.

## Release Decision

The native automated gate passes, but the candidate is not yet declared ready
for formal release. The remaining gates are:

- Authorized desktop acceptance on the intended macOS/hardware matrix,
  including actual monitor hotplug, fullscreen, AX timing, and Bar interaction.
- Intel native CI execution, resolution of the Intel Nix dependency contract,
  and validation of any Nix packages included in the release.
- Confirmation of the release version, minimum supported macOS, upgrade notes,
  and signing/notarization or distribution requirements.

No daemon installation/restart, live window manipulation, tag, or publication
was performed. The checklist records evidence, not a guarantee of absence of
defects; the release goal remains open until these decisions and checks close.

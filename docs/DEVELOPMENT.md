# Development Loop

How long the verification steps actually take in this repository, and how to run
the smallest one that answers the question you have. The instructions in
[AGENTS.md](../AGENTS.md) assume this document.

## Measured costs

Apple M4 (10 cores), `rustc` 1.97.0 from the Nix dev shell, repo-local `target/`.

| Step | Wall clock |
| --- | --- |
| Cold build of the test binary (406 crates) | 2m10s |
| `cargo test -p spool --no-run` after one source edit | 8–11s |
| ↳ of which front-end (`cargo check -p spool --tests`, same edit) | ~4.5s |
| Full suite, `--test-threads=1` | 82.7s |
| Full suite, default threads | 22.8s |
| One module (`cargo test -p spool -- tiling::`) | 0.7s |
| `cargo test --workspace` (6 test binaries) | 35.4s |

Two conclusions follow.

**The rebuild is a fixed ~9s and does not depend on what you changed.** Touching
`src/main.rs`, `src/ecs.rs` (high fan-out) and a 120k-character test module all
cost about the same. The suite lives in one test target — the `spool` bin — so
every edit re-typechecks the crate (~4.5s) and relinks a ~170MB binary (~4.5s).
That is the floor for the current crate layout; nothing below is worth tuning.

**Test *execution* was the actual waste, not the build.** The suite is CPU-bound
(`user` time 87s against 83s wall on one thread, at 1,143 tests) and isolates its
own state: each `TestHarness` builds its own Bevy `App` and mock
`WindowManager`, state files are named by pid plus a nonce, and the IPC tests
bind temporary sockets. Running them on one thread bought nothing and cost ~60s
per run.

## Running `--test-threads=1` is not the default

Earlier instructions ran every gate with `--test-threads=1`. That habit is what
made "run the tests after each edit" expensive, and it is not required for
correctness.

One test did depend on serial execution, and it was worth understanding rather
than working around. `reader::tests::a_second_daemon_is_refused_until_the_first_exits`
dropped the first daemon and asserted that a re-bind succeeded in the next
statement. Under parallel execution that failed about one run in ten.
Instrumentation settled it: the dropped guard did run its `InstanceLock` drop,
yet the very next `flock` on that same path still returned `EWOULDBLOCK`, and
delaying the re-acquire by ~50ms made it pass every time. The test was asserting
an instant the process cannot guarantee. It now retries the re-acquire against a
10s deadline, so it still fails if the lock is never released.

With that fixed, the whole suite runs in parallel: 30 consecutive full runs plus
every workspace run during the investigation, all green. Keep
`--test-threads=1` as a debugging tool, not a gate — if you are chasing an
order-dependent flake, serial execution makes the interleaving deterministic.
Do not put it in scripts, CI, or agent instructions.

## The loop

Pick the cheapest step that can fail for the reason you are worried about.

1. **Type-level mistakes — `cargo check -p spool --tests`** (~4.5s). Catches
   borrow and type errors without codegen or linking. Note that `cargo check`
   keeps a *separate* cache from `cargo test`, so the first `--all-targets` check
   in a fresh tree costs ~75s; it is not a cheap general pre-flight.
2. **The behaviour you just changed — `cargo test -p spool -- <filter>`**
   (~10s including the rebuild). Test names are their module path, so the filter
   is the source file stem: `tiling::`, `window_state_sync::`,
   `command_dispatch::`, or the owning production module (`lua::`,
   `manager::windows::`). 23 tests run in 0.7s once built.
3. **Before you call a work item done — `cargo test -p spool`** (~32s). The
   whole suite in parallel. This is the step AGENTS.md requires before finishing.
4. **Before handoff or release — `cargo test --workspace`** (~35s), or the full
   gate in [RELEASING.md](RELEASING.md).

`cargo fmt` is not on this list because it is free; run it whenever.

## Measured and rejected

Do not re-litigate these without new measurements.

| Change | Result |
| --- | --- |
| `-C link-arg=-fuse-ld=lld` | Binary 171MB → 118MB, rebuild 8.7s → 10.2s. The 9s is front-end plus symbol work, not linker inefficiency. |
| `[profile.test] debug = "line-tables-only"` | Binary 171MB → 166MB, rebuild unchanged. macOS keeps DWARF out of the binary, so debug level is not the cost. |
| `RUST_LOG=off` | 23.15s vs 22.8s — noise. The libtest harness captures output either way. |
| `cargo check --all-targets` as a pre-flight | ~75s cold on its own cache; it does not warm `cargo test`. |

Also avoid, unless you mean it:

- `cargo clean` — 2m10s to get back, and the incremental caches are what make
  the 9s rebuild possible.
- Alternating `--no-default-features` and the default `lua` feature inside one
  loop. Each switch recompiles the crate and its dependency graph; pick the
  variant you actually need and stay on it.

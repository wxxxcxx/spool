# Resource CLI implementation review

Date: 2026-09-13. Baseline: `685f0856f79856a94fcd91687538bfa63cc13e8e`.
Review scope is the baseline plus the complete uncommitted implementation and new files.
Specification: [implementation plan](../CLI_IMPLEMENTATION_PLAN.md), [native inspection](../NATIVE_INSPECTION.md), ADRs 0004–0006.
Standards: `AGENTS.md`, `docs/ARCHITECTURE.md`, and the woz-code-review smell baseline.
The two axes were reviewed independently by parallel agents and re-reviewed after fixes.

## Standards

Two documented-standard findings were fixed and independently confirmed closed:

- Command admission now uses the project Error/Result. A rejection retains its stable code and a diagnostic message; native/system failures retain their underlying cause.
- Native acquisition and value conversion live in the platform adapter. The inspection layer contains supervision, evidence protocol, and pure projections. Native capture checks the process main-thread marker.

Two non-blocking heuristics remain: finite outcome strings could become an enum, and display ownership/geometry validation could share a helper. These are maintainability suggestions, not additional documented-standard violations. They are left visible rather than broadening this migration with another representation refactor.

## Spec

The initial four findings were fixed and independently confirmed closed:

- Window Space/display filters compare window→Spaces and Space→windows observations. Display membership maps those Space sets through topology; screen intersection is not membership.
- Issues from conclusively excluded rows remain in the response, but do not downgrade requested coverage.
- Lists use reserved `match_status: matched|unresolved` metadata; text output partitions those sets.
- Human summaries accept both retained flat rows and native identity records, including application PID identity.

Follow-up review found two more issues: unselected Space identity fields affected completeness, and the CG group demanded optional fields absent from the native dictionary. Both were corrected with selection-plan regressions. Review caught a regression in the first correction: nested Session/Display Space summaries lost fields. The plan now distinguishes a requested nested summary from Window membership dependencies; a regression covers both nested resource contexts. The Spec reviewer independently confirmed this correction closed.

## Validation boundary

Automated validation and read-only capture results follow. No installation, daemon restart, desktop mutation acceptance, system activation, or push is part of this change. Mock ECS acceptance is not live multi-display acceptance.


- Formatting, workspace typecheck, strict workspace/all-target Clippy, strict no-default-features Clippy, strict Lua module Clippy, and the final default debug build passed.
- Full default main-program run: 1027 passed, 3 failed, 2 ignored. Full no-default run: 893 passed, the same 3 failed, 2 ignored. Failures identified unmigrated test fixtures: root restart spelling, numeric ordinal focus spelling, and a synthetic subscription peer omitting the now-required admission acknowledgement. The fixtures now use `service restart`, `window focus --nth`, and an explicit acknowledgement. Each failed test passed its targeted rerun in both configurations.
- After the final fixes, inspection-related regressions passed 18/18 and service regressions passed 11/11 in both configurations. These include the late nested-Space correction and failed-unload preservation of registration/sidecar bytes.
- Remaining workspace crates passed: IPC 23, Lua 6, shared types 93, inspection transport/filter integration 4 (126 total). They ran with the same vendored LuaJIT features supplied by the default main program; omitting that feature provider in the first isolated invocation produced a build-configuration error, corrected before execution.
- The built LuaJIT module loaded through `spool script run scripts/verify-lua-module.lua` without daemon access.
- Both Nix module constructors evaluated to `[executable, "service", "run"]` using an already-installed Nixpkgs source. The initial flake-based attempt failed with a disk-space error; no system activation/build was attempted. This verifies the actual module argument expressions, not a full Home Manager/Darwin activation graph.
- Real native display list: complete, exit 0, one display, zero issues, 24 operation records.
- Real native window list with a two-second budget: partial, exit 3, 211 retained records, 2096 ms, 2806 operation records. Permission/source failures and the interrupted remainder remained explicit. This demonstrates bounded partial collection, not successful access to every application's AX data.

Standards: zero remaining documented-standard findings; two non-blocking maintainability heuristics. Spec: zero remaining reviewer findings after corrections. Full runs were followed by scoped reruns of their failures and changed seams, rather than another unchanged full run.

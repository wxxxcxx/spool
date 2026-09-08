# Main Program Boundary Review

Date: 2026-09-07. Baseline: `4611ee06a787e3f260ab9d680a10cb50b267dade`.
The worktree was clean at the start of this iteration.

## Scope

Continue reviewing and fixing the executable and its shared runtime libraries,
with emphasis on reproducible bugs and boundary conditions. Nix and unrelated
packaging are excluded from the current goal. No installation, daemon startup
or restart, live window manipulation, publishing, or Git commit is performed.
Earlier release artifacts do not validate this new source snapshot.

## Window Commands

All regressions dispatch actual actions through the mock ECS harness and inspect
the resulting platform frame; rejection cases also inspect frame-write counts
and the retained maximize marker.

- **P1: resize arithmetic overflow.** An `i32::MAX` configured resize step
  panicked before the bounds clamp. Saturating arithmetic now precedes the
  clamp for floating dimensions and tiled stack height.
- **P1: inverted resize bounds.** A positive 50px usable dimension called
  `clamp(100, 50)` and panicked. The effective minimum cannot exceed the usable
  dimension. Both axes and grow/shrink directions are covered.
- **P1: floating move arithmetic overflow.** A maximum move step panicked
  before the viewport clamp. Component-wise saturated addition preserves the
  intended edge in all four directions on positive and negative displays.
- **P2: empty viewport still mutated geometry.** Excessive padding left no
  usable area, but move/center still moved the window and maximize removed
  its restoration state. Move, center, resize, and maximize now return before
  geometry or marker changes when either usable dimension is nonpositive.
- **P2: invalid exact widths changed state.** Zero was treated as an ordinary
  grow request; a tiny positive ratio moved the window even though its requested
  width rounded to zero. Validation now rejects nonpositive, nonfinite, and
  unrepresentable pixel widths before removing the maximize marker. Ordinary
  oversized widths remain supported.

The initial command regression run had three failures after the first two resize
fixes. Center, maximize-marker preservation, and sub-pixel widths were then
independently reproduced before their fixes. Final focused command tests: 13
passed. Existing tiling tests: 20 passed, including two-display-width windows,
resize cycling, floating movement, and maximize/restore behavior.

## Persistent State

- **P1: nonfinite values made saved state unreadable.** Infinity serialized as
  `{"Float":null}`, so the loader rejected the entire file and lost access to
  unrelated valid entries. Persistent encoding now rejects nonfinite numbers
  before mutation, including nested values. Binary rejected-write transport
  remains lossless.
- **P1: accepted nesting exceeded the loader's depth limit.** Saving a 64-level
  List/Map succeeded with only 35,438/35,950 bytes, but loading failed with
  `recursion limit exceeded`. The shared store now validates a 62-container
  depth budget before cloning or mutation, and validates loaded state too.
  Tests exercise 61/62/63 levels, every scalar leaf, empty containers, mixed
  branches, and actual file round trips. There is no recursive JSON reparse.
- **P2: loading bypassed live key and capacity limits.** Empty or over-512-byte
  keys and over-1-MiB compact stores entered live state from a file. Shared
  deserialization now enforces the same constraints as writes, with exact-limit
  and multibyte-key coverage.

The depth regressions failed before the fix (14 daemon state tests passed,
4 failed), then all 18 passed. Rejected writes preserve existing values,
revision, and dirty status. Temporary-write and rename failure tests also
preserve the prior destination and allow a later retry. No extra atomic-write
bug was confirmed by these tests; power-loss durability is not established.

## IPC

- **P1: shutdown depended on the published socket path.** Renaming the path
  prevented Drop from waking the blocked accept thread. The old regression
  exceeded its two-second bound (2.01s); a private socket-pair wakeup now lets
  it finish immediately (0.00s), independent of the published endpoint.
  Accepted streams explicitly restore blocking mode after the nonblocking
  listener handoff; the BSD inherited-flag case has a regression test.
- **P1: trickled frames bypassed total deadlines.** Per-syscall timeouts let
  each partial read restart the budget. Both a handshake and a query reply
  completed after their 100ms budget in the old-semantics control (two failures,
  0.73s). Shared exchange deadlines now cover headers, payloads, read/write
  direction changes, and retries. Both regressions return `TimedOut` (two
  passes, 0.22s). The control did not alter the working tree.

All 18 IPC tests pass. Further coverage includes an expired clock with ready
sockets, a full write buffer, and subscriptions remaining usable after an idle
period longer than their handshake budget. Intentionally idle subscription
reads remain unbounded. No live daemon shutdown or long-duration load test was
performed, and no worker execution session remains running.

## Standards

Integration review retains the existing ECS command and frame-commit paths;
no new AppKit call crosses a worker boundary. Size arithmetic is shared by
floating resize and tiled stack-height resize. Store validation is centralized
in the shared type, and rejected values do not reach live state. Transport
budget enforcement is private to the IPC library and preserves the wire format.
No confirmed introduced standards violation remains in the reviewed changes.

## Spec

An independent read-only regression pass found no confirmed introduced geometry
defect against `CONTEXT.md`, `WINDOW_POLICY.md`, and existing tiling tests.
Exact 2.0 display-width ratios remain supported, floating windows still do not
occupy strip slots, and requests continue through the existing application-size
constraint, partial-success, and bounded frame-retry machinery.

## First Pass Verification

- `cargo fmt --all -- --check`: passed.
- Locked workspace and Lua-free `cargo check`: passed.
- Strict Clippy, workspace/all targets and Lua-free/all targets: passed.
- Standalone Lua module strict Clippy: passed.
- Locked workspace tests: 735 passed, 2 ignored (657 daemon, 18 IPC,
  6 Lua client, 54 shared types).
- Locked Lua-free daemon tests: 571 passed, 2 ignored.
- The first sandboxed Lua-free run had five temporary-socket permission
  failures. The approved rerun passed without source changes. Both matrix
  runs use serial tests and temporary endpoints, not a live daemon socket.
- The two ignored tests inspect private SkyLight Space-move runtime objects;
  they remain outside the authorized automatic acceptance scope.
- Logs: `/tmp/spool-main-review-20260907-workspace.log` and
  `/tmp/spool-main-review-20260907-without-lua.log`.
- Native desktop behavior is not established by mock ECS tests.
- The broader review goal remains active; this is not a claim of zero bugs.

## Configuration and Queue Follow-up

The next pass starts from the same HEAD plus the first pass's preserved changes.
It remains limited to the main program, with no Nix or live daemon work.
This pass began on 2026-09-07; final validation completed on 2026-09-08
(Asia/Shanghai).

- **P1: multibyte colors panicked.** A six-byte non-ASCII color reached byte
  slicing inside a UTF-8 character. The configuration accessor regression
  failed with `end byte index 2 is not a char boundary`. Parsing now checks
  ASCII before slicing, preserving the existing invalid-color fallback. Six
  color-related tests passed after the fix.
- **P1: nonfinite configuration reached publication.** The setup path accepted
  `swipe.sensitivity = 0/0`, and a reload published `ConfigChanged` instead of
  rejecting the candidate. Both setup and actual worker reload regressions
  failed before the fix. A private configuration-validation module now checks
  numeric options, swipe settings, decorations, presets, and window-rule values
  before publication. Tests cover NaN and both infinities, and preserve normal
  finite clamps, automatic radii, and oversized width ratios. Failed reloads
  retain the installed configuration.
- **P2: malformed grids became different valid placements.**
  `2:bad:2:1:1:1:1` became `(0.5, 0.5, 0.5, 0.5)` because invalid fields were
  discarded. Grid parsing now requires exactly six valid finite fields and
  rejects nonfinite ratios, including overflow from subnormal divisors.
  Existing owner-display and geometry-default tests also pass (six grid tests).
- **P1: Lua queues had no bounded main-thread pass.** Three regressions failed:
  a newly queued request joined the live batch, an expired batch kept receiving,
  and one ECS store pass answered all 1024 writes without yielding. Effect,
  world-read, and store-access queues now share a frozen, capped, lazy drain.
  Limits are 256 requests and a soft 4ms between requests, matching the event
  pump's budget sizes. Expiry is checked before receive, so remaining messages
  stay queued. The first pending request guarantees progress. Regression tests
  verify FIFO completion of all 1024 writes and replies across subsequent
  passes, late-message deferral, and expiry without losing the next item.
  World queries no longer collect their whole batch before extraction.

These are configuration and mock-ECS/channel results, not evidence of a live
desktop freeze or a live desktop fix. Standards review keeps candidate
validation and queue budgeting private to their owning modules. No AppKit
operation moves to the worker. This follow-up used local review rather than
an additional independent-agent review.

### Follow-up Verification

- Final locked workspace tests: 742 passed, 2 ignored (664 daemon, 18 IPC,
  6 Lua client, 54 shared types).
- Final locked Lua-free daemon tests: 573 passed, 2 ignored.
- Formatting and diff-whitespace checks: passed.
- Locked workspace and Lua-free compilation: passed.
- Strict Clippy: workspace/all targets, Lua-free/all targets, and standalone
  Lua module all passed. The initial lint run caught exact-float test assertions
  and a branch-style issue; both were resolved before the final matrix.
- The final workspace run includes lazy world-query consumption after the
  last production edit. Tests remain serial and use temporary IPC endpoints.
- Logs: `/tmp/spool-main-review-20260907-config-workspace-final.log` and
  `/tmp/spool-main-review-20260908-config-without-lua.log`.
- The same two private Space-operation tests remain ignored. No native desktop
  acceptance, daemon deployment, release publication, or zero-bug claim is made.
  The broader goal remains active.

## Lua Layout and Frame Follow-up

This pass completed its focused regressions on 2026-09-08 (Asia/Shanghai),
against the same HEAD and preserved earlier changes. It does not deploy or
restart Spool, manipulate native windows, or change Nix or packaging.

- **P2: explicit stacking targeted the wrong column.** Replaying
  `stack(2, 0)` on `[0] [1] [2] [3]` produced `[0] [1,2] [3]`.
  Moving an entry out of a stack also moved its unrelated siblings, and a
  self-target request changed the strip. All four initial ECS tests failed.
  A separate named-target `LayoutStrip` operation now validates both endpoints
  before editing and moves one stack item. The existing imperative whole-column
  stack-left command is unchanged. Native tabs stay grouped when entering or
  leaving another stack; no native tab group is created by these tests.
- **P2: invalid stack destinations destroyed the predicted layout.**
  Self-stacking removed the source record; a floating destination had no column
  to receive a removed record. Cross-Space stacking predicted a membership
  change without a native move command. The tree now requires distinct tiled
  columns in the same Space before extracting anything. Repeating a stack
  request within the same column is idempotent, while stale intent is still
  recorded for best-effort replay.
- **P2: unstack used the active Space instead of the source.** A window from
  an inactive Space moved into the active Space's predicted layout; with no
  active Space the record disappeared. A single or floating window was also
  unnecessarily removed and appended. Unstack now changes only an actual
  source stack, inserts next to that stack, and retains its width. Record
  removal also keeps the same selected record when an earlier sibling leaves.
- **P2: swapping copied focus to the slot instead of the window.** The tree's
  focused ID stayed unchanged while its per-record focus flags pointed to a
  different window. Focus now travels with the exchanged record.
- **P2: deferred mode changes reordered or split later layouts.** A float
  followed by a stack changed surviving columns from `[1] [2]` to `[2] [1]`.
  Tracking pending flags fixed that case but a sink followed by a stack still
  became `[1] [0] [2]` instead of `[1,0] [2]`: the delayed retile observer
  reinserted the window after stacking. Both failures were reproduced. Replay
  now uses one cached ECS system run per operation, flushing its observers
  before the next run instead of maintaining a parallel pending-state model.
  Sink reuses `RetileWindow`, including native membership and capability
  checks, rather than appending to an arbitrary active strip. Tests cover both
  directions, same- and separate-batch requests, and a refused sink followed
  by stacking.
- **P1: two independent script-coordinate overflows.** A finite extreme
  relative offset panicked during display-origin addition. Saturating global
  addition fixes that path. Separately, a `SetFrame` with `x = i32::MAX` and
  width 1 panicked in the actual ECS frame-request pipeline. That shared
  pipeline now checks both endpoint additions and positive dimensions before
  updating either layout input. Rejection preserves Position, Bounds, Desired
  Window Frame, observed mock geometry, and platform-write count. Negative
  origins and existing minimum-one-pixel normalization remain supported.
  The endpoint check is also used before script markers are queued: a further
  regression showed an invalid second frame discarded a valid first frame in
  the same batch. That regression now preserves the first request. Direct
  legacy-marker injection separately covers the shared pipeline's rejection,
  so the early script check cannot hide a missing downstream guard.

The first pure-tree run had 19 passes and 8 failures; the original four ECS
stacking regressions all failed. The separate frame-pipeline regression also
failed with `attempt to add with overflow`. The expanded suite includes 12 Lua
layout-op tests and a separate feature-independent frame-request test, including
32 successive snapshot-transform/replay comparisons
of plain-stack column order. These assertions do not certify arbitrary extreme
coordinate arithmetic elsewhere in the program or exact nested-tab predictions.

### Remaining Audit Targets

The broader release goal remains active. The next pass must investigate the
other Lua replay operations: the inspected swap arm still works in column
indices, the width arm writes a ratio rather than a resize request, and the
stack arm does not distinguish the `tabs` flag. These are unresolved contract
risks, not part of this pass's fixed-and-verified list. The flat snapshot model
also needs a separate check for native tabs nested inside stacks. Native
multi-display desktop acceptance remains outstanding; mocks are not proof of
WindowServer behavior. No zero-bug or release-ready claim is made.

### Layout Follow-up Verification

- Locked workspace tests: 764 passed, 2 ignored (677 daemon, 18 IPC,
  6 Lua client, 63 shared types). This pass adds 22 regression tests.
- Locked Lua-free daemon tests: 574 passed, 2 ignored. Lua replay tests follow
  the production `lua` feature gate; the shared frame-request rejection test
  runs in both configurations.
- The first Lua-free matrix incorrectly ran ten Lua-only regression cases
  against a build that deliberately does not register replay. Their feature
  gating was corrected, and the common frame test was separated rather than
  changing Lua-free runtime scope. Strict Clippy also identified the new
  Lua-only strip helper as unused in that build; it now follows its caller's
  feature gate.
- Locked workspace and Lua-free compilation, strict workspace/all-targets,
  Lua-free/all-targets and standalone Lua-module Clippy, formatting, and
  diff-whitespace checks passed.
- Final logs: `/tmp/spool-main-review-20260908-layout-workspace-final.log` and
  `/tmp/spool-main-review-20260908-layout-without-lua-final.log`.
- The same two private SkyLight Space-operation tests remain ignored. All
  fixture tests run serially using temporary IPC endpoints. No native desktop
  acceptance, installation, daemon restart, release publication, or Git commit
  was performed.

## Named Swap and Width Follow-up

This pass continues from the preceding working tree on 2026-09-08
(Asia/Shanghai). It remains a main-program source and regression review,
without Nix changes, daemon deployment, native window manipulation, or a Git
commit. The previous turn made concrete source and test progress; the broader
release goal remains active.

- **P2: named swaps exchanged columns, not entries.** Swapping two entries in
  one stack did nothing. Swapping 1 and 2 in `[0,1] [2,3]` produced
  `[2,3] [0,1]` rather than `[0,2] [1,3]`. Both actual replay tests failed.
  `LayoutStrip::swap_items` now changes the named slots and preserves existing
  native tab groups as indivisible items. Exchanging members of the same
  native tab group is a no-op. The imperative whole-column swap is unchanged.
- **P2: swaps carried the wrong column widths.** A separate regression showed
  a 256px entry remained 256px after moving into a 768px destination column.
  Replay now prepares all exchanged item sizes before mutation, applies each
  destination's width, and leaves unrelated siblings and focus in place.
- **P2: width requests only changed metadata.** A 0.75-width request against
  a 1024px viewport left both stacked windows at 400px instead of 768px.
  Requests now enter the shared resize/frame pipeline for every column member,
  including native tabs. The viewport is resolved through the strip's owning
  display, not the focused display. `WidthRatio` remains a projection updated
  by frame commits. Pending resize heights are retained when a later width
  operation runs in the same frame.
- **P1: representable window widths still overflowed strip accumulation.**
  A requested width of `i32::MAX` fit one window's local frame but adding
  neighbouring columns panicked in `LayoutStrip::column_positions`.
  The same failure was reproduced through both Lua and the ordinary resize
  command. Both use a shared checked-width budget before mutating layout
  inputs or maximize restoration state. Exact-total-limit, one-over-limit,
  missing-frame and missing-window cases have pure regressions; the normal
  command regression also runs without Lua. Two-display-width columns remain
  supported.
- **P2: invalid transforms polluted predicted state.** Swapping with a float
  exchanged records between floating and tiled containers; cross-Space swaps
  predicted a native membership change without a native command. Nonfinite
  and nonpositive widths also entered the predicted column metadata. Both
  regression groups failed before the guards were added. Such transforms now
  preserve the prediction while retaining their best-effort intent records.

The initial ECS run reproduced three failures. The expanded run independently
reproduced width inheritance and strip-overflow failures, and the pure-tree
run reproduced two invalid-transform failures. Successful controls include an
unfocused owner display, native tab-group integrity, unchanged focus, and a
2.0 viewport-width request. These are mock-ECS and pure-tree results, not native
desktop acceptance.

### Scope Still Open

`ws:tab` still reaches a replay branch that does not distinguish its `tabs`
flag from vertical stacking. The snapshot's flat column records also cannot
fully describe native tab groups nested within stacks; full chained prediction
fidelity for such groups remains unresolved. Those are release-blocking
semantic gaps, not solved by the swap/width fixes or hidden by an ignored test.
Other extreme-coordinate producers and native multi-display acceptance also
remain in the broader audit. No release-ready or zero-bug claim is made.

### Build Space

Available disk space fell below 500 MiB during verification. After dry-run
inspection and approval, four explicitly selected debug incremental caches were
removed; this recovered insufficient physical space. A further approved
`cargo clean -p spool --profile dev` removed Cargo-selected debug artifacts.
The verbose dry run listed only debug paths; release artifacts stayed at
975 MiB, and available disk space recovered to about 18 GiB. No source,
dependency specification, release deployment, or daemon state was changed by
the cleanup. Cargo's reported logical size is not a claim of physical space
recovered.

### Swap and Width Verification

- Locked workspace tests: 778 passed, 2 ignored (689 daemon, 18 IPC,
  6 Lua client, 65 shared types). This pass adds 14 regression tests; all
  22 Lua layout-op tests pass, including preservation of an earlier queued
  height request.
- Locked Lua-free daemon tests: 576 passed, 2 ignored. The ordinary exact-width
  and preset-grow overflow regression and pure strip-width budget checks run
  in this configuration as well.
- Locked workspace and Lua-free compilation, strict workspace/all-targets,
  Lua-free/all-targets and standalone Lua-module Clippy passed. Strict Clippy
  initially required a semicolon in the new swap error-log arm; the correction
  was included in the final workspace test run.
- Final test logs:
  `/tmp/spool-main-review-20260908-swap-width-workspace.log` and
  `/tmp/spool-main-review-20260908-swap-width-without-lua.log`.
- The same two private SkyLight Space-operation tests remain ignored. Fixture
  tests ran serially with temporary IPC endpoints. No native desktop acceptance,
  daemon deployment or restart, release publication, or Git commit was performed.
- The release goal remains active; passing this matrix does not resolve the
  semantic gaps listed above or certify all geometry arithmetic.

## Nested Native Tab Snapshot Follow-up

This pass continues on 2026-09-08 (Asia/Shanghai). The previous pass completed
source fixes and its validation matrix; the release goal remains active. No
Nix, deployment, daemon restart, native window manipulation, or Git commit is
part of this pass.

- **P2: flattened snapshots split native tab entries in predictions.** Four
  actual mock-ECS regressions failed before the model change. With native groups
  `[1,2]` and `[4,5]` nested in columns `[0,1,2] [3,4,5]`, swapping 2 and 3
  predicted `[0,1,3] [2,4,5]` rather than `[0,3] [1,2,4,5]`. Stacking and
  unstacking likewise moved only one tab in the prediction, while replay moved
  the native group. Swapping members within one native group also changed the
  prediction despite replay being a no-op.
- `ColumnSet` now owns canonical `StackItemSet` entries instead of a flat record
  vector. Native groups remain explicit inside vertical stacks. The immutable
  transforms resolve, move, and exchange complete entries. Removing an
  individual record normalizes only its affected entry; unrelated selections,
  native group order, focus records, and destination column widths are retained.
- Snapshot extraction preserves the same structure. Lua `columns()` and the
  window-bar adapter deliberately retain their existing flat read views.
  This changes the postcard snapshot representation, so local IPC protocol 3
  rejects protocol 2 rather than silently decoding it with a different schema.
  Persisted layout-state and public query-document versions are unchanged.
- The four initial regressions and all 26 layout-op tests passed after the
  first implementation. Further validation covers chained transforms, 108
  successive full-entry comparisons against ECS, Lua read/transform behavior,
  structural sharing, selected-index preservation, normalization, binary/JSON
  round trips, and old-protocol rejection.
- **P2: imperative stacking removed columns before refusing fullscreen.**
  While adding feature-independent snapshot coverage, review found that
  `LayoutStrip::stack` removed its source before classifying either endpoint.
  The pure regression reproduced a three-column strip becoming only its last
  column when the left destination was native fullscreen. The source-fullscreen
  branch likewise returned after removing its source. Both endpoints are now
  checked before removal; the regression checks source and destination cases
  and runs with and without Lua. This is layout-state loss, not native window
  closure. Ordinary eligible stacking behavior remains unchanged.

### Remaining Scope

The earlier flat-snapshot limitation is addressed for existing native groups
and the tested swap/stack/unstack contracts. `ws:tab` still has an independent
native-creation versus vertical-stack replay mismatch; this pass does not hide
that behavior or redefine native tabs as overlapping windows. Other transform
contracts, numeric ingress, stale snapshot identities, and real multi-display
desktop acceptance remain in the broader audit. No zero-bug or release-ready
claim is made.

### Nested Snapshot Verification

- Locked workspace tests: 793 passed, 2 ignored (698 daemon, 19 IPC,
  6 Lua client, 70 shared types). This pass adds 15 regressions: seven Lua
  layout-op tests, two feature-independent daemon tests, five shared-type
  tests, and one explicit old-protocol rejection test.
- Locked Lua-free daemon tests: 578 passed, 2 ignored. The nested snapshot
  projection and fullscreen endpoint preservation tests pass in both modes.
  Standalone shared-type tests without Lua: 67 passed.
- Locked workspace and Lua-free compilation, strict workspace/all-targets,
  Lua-free/all-targets and standalone Lua-module Clippy, formatting, and
  diff-whitespace checks passed on the final source.
- The initial expanded full run found an outdated two-window expectation in
  the bar test after its fixture gained a third native tab. The expected flat
  IDs were updated to all three windows, retaining the selected-anchor check.
  Strict lint cleanup also moved single-column projection into a private
  function and removed the new unstack constructor's `expect`. The complete
  matrix was rerun after these changes and the later fullscreen guard.
- Final workspace and Lua-free daemon logs:
  `/tmp/spool-main-review-20260908-nested-tabs-workspace-verified.log` and
  `/tmp/spool-main-review-20260908-nested-tabs-without-lua.log`.
  The standalone shared-type log is
  `/tmp/spool-main-review-20260908-nested-tabs-shared-without-lua.log`.
- The same two private SkyLight Space-operation tests remain ignored. Tests
  ran serially using mock state and temporary IPC endpoints; no native desktop
  acceptance, deployment, daemon restart, publication, or Git commit occurred.
  The broader release goal remains active.

## Floating Ownership and Snapshot Follow-up

This pass continues the main-program review on 2026-09-08 (Asia/Shanghai).
The preceding pass made verified source and regression progress. Nix and
unrelated packaging remain out of scope; no daemon deployment or native desktop
manipulation is part of this pass.

- **P2: float/sink predicted a move to the first active Space.** Both an
  inactive-Space fixture and a two-display fixture reproduced the wrong owner.
  With no active Space, three records became two. Mode changes now resolve the
  source before extracting a record and mutate only that Space, independent of
  active display/Space markers.
- **P2: repeated requests changed predicted structure.** Sinking an already
  tiled stacked window extracted it into a separate default-width column.
  Repeated floating reordered the floating list. A move to the current Space
  also extracted and reinserted the window. Those requests now preserve their
  predicted structures while still recording their best-effort intent.
- **P2: shifting a float put it in a tiled container.** The target column held
  a record still marked floating. The prediction now keeps it in the target
  floating list. The existing native-move regression additionally compares the
  predicted and actual owner/classification for both resizable and fixed-size
  floating windows, without claiming exact geometric prediction.
- **P2: layout snapshots omitted known inactive/hidden floats and accepted
  uncertain ownership.** All five original ECS snapshot regressions failed:
  inactive and hidden floats were absent, duplicated native IDs produced
  duplicate records, and incomplete/overlapping membership was reported as a
  definite owner. `WindowSet` extraction now uses the shared complete membership
  scan, retaining native order while filtering duplicate/ambiguous IDs. An
  unconfirmed floating entry is omitted from the value, not removed from ECS;
  regression checks preserve its entity, Floating marker, physical frame and
  write count, then verify recovery. Tiled projections are retained. A tiled-only
  snapshot performs no additional membership scan.
- **P2: geometry alone incorrectly implied visibility.** After inactive floats
  were included, two regressions reproduced both floating and tiled records on
  an inactive Space as visible. Visibility now also requires the owning display
  to report that native Space as visible in the current topology observation.
  Unknown visibility does not discard an otherwise known floating membership.

The pure prediction run reproduced six failures, with 35 existing tests passing.
The first snapshot run reproduced five failures; the next run passed four tests
and isolated the two visibility failures. Additional controls cover complete
topology requirements, unknown visibility and recovery, native ordering, absence
of unnecessary membership reads, and repeated Lua mode replay on an unfocused
display. These are pure-value and mock-ECS results, not WindowServer acceptance.

### Remaining Scope

The release goal remains active. This pass does not solve `ws:tab` native
creation, stale snapshot/window incarnation binding, `follow`/`view` prediction,
or configuration-dependent retile ordering and geometry. Those contracts and
the remaining numerical ingress paths need further review, and real
multi-display acceptance is still outstanding. No release-ready claim is made.

### Floating Follow-up Verification

- Locked workspace tests: 809 passed, 2 ignored (708 daemon, 19 IPC,
  6 Lua client, 76 shared types). This pass adds 16 tests: nine feature-independent
  snapshot regressions, one Lua mode-replay regression, and six pure prediction
  regressions. Existing topology and native floating-move tests also gained
  ordering and prediction/replay assertions.
- Locked Lua-free daemon tests: 587 passed, 2 ignored. Standalone shared-type
  tests without Lua: 73 passed. All nine snapshot tests run without Lua.
- Locked workspace and Lua-free compilation, strict workspace/all-targets,
  Lua-free/all-targets and standalone Lua-module Clippy, formatting, and
  diff-whitespace checks passed. A private `Window` import in a new test was
  corrected to the public manager type before the complete test runs.
- Final logs: `/tmp/spool-main-review-20260908-floating-workspace-final.log`,
  `/tmp/spool-main-review-20260908-floating-without-lua.log`, and
  `/tmp/spool-main-review-20260908-floating-shared-without-lua.log`.
- The same two private SkyLight Space-operation tests remain ignored. Fixtures
  ran serially with mock state and temporary IPC endpoints. There was no native
  desktop acceptance, deployment, daemon restart, publication, or Git commit.
  The broader release goal remains active.

## Snapshot Identity Follow-up (2026-09-08)

Scope: delayed `WindowSet` results, embedded Lua callbacks, `spool.windows`,
the IPC round trip, per-operation replay, and the native Space command handoff.
This supersedes the stale-snapshot identity item listed as open in the preceding
passes. No daemon was deployed, started, or restarted.

### Confirmed and Fixed

- **P1: old snapshots acted on replacement windows with reused numeric IDs.**
  Two regressions reproduced the original behavior: an old `float(1)` floated
  the new window 1, and an old `swap(0, 1)` exchanged the replacement window's
  slot. Operations carried only `WinID` and resolved it again at execution.
  `LayoutPlan` now carries the original `LayoutSnapshot`, with a random daemon
  session and each available tracked window's ECS entity bits and native
  incarnation. Changing either identity component, or the session, rejects the
  old request. Missing-at-capture IDs cannot bind to later arrivals.
- **P1: native Space handoff could discard an already checked identity.**
  Replay converted `shift` into an ordinary numeric-ID action before the
  native command system ran. It now queues `LayoutSpaceRequested` with the
  original binding. Submission rechecks that binding, including associated
  windows, before issuing the native intent. Existing transaction-level
  entity/incarnation checks continue to protect reconciliation and follow-focus.
  A replaced or unbound associated window skips the whole move; ordinary
  explicit CLI actions retain their existing current-ID semantics.
- **P2: implicit native-group/column members also require identity checks.**
  A valid named window does not authorize a newly replaced tab sibling or
  column member. Swap/stack/unstack validate affected native groups, and width
  validates the complete affected column before mutation. Checks happen per
  operation after preceding observers flush, so one stale operation does not
  discard later operations on unchanged windows.

The binding map is immutable and independent of the predicted layout, surviving
transform chains, branching, Lua return conversion, and serialization. A returned
old value keeps its old provenance instead of borrowing the current callback's
snapshot. `ops()` remains an inspection view; all production transports use
`plan()`. Session checks also apply to `view`, which has no window endpoint.
These checks are not client authentication or a frozen-layout transaction.

The binary representation is incompatible: local IPC protocol 4 rejects both
the former flat-snapshot protocol 2 and the unbound-operation protocol 3 before
dispatch. Public query documents and persisted layout versions are unchanged.
The daemon explicitly uses the already-locked `uuid` 1.23.1 package for its
session identity; no new package version was added to the lockfile.

### Remaining Scope

The release goal remains active. Native creation for `ws:tab`, `follow`/`view`
prediction, configuration-dependent retile ordering/geometry, further numerical
ingress review, and authorized real multi-display acceptance remain outstanding.
Snapshot identity regressions do not establish WindowServer behavior or release
readiness.

### Snapshot Identity Verification

- The original primary/secondary ID-reuse regressions both failed before the
  fix and passed afterwards. Focused layout replay: 32 passed before the
  additional boundary cases were added.
- Final locked workspace tests: 827 passed, 2 ignored (722 daemon, 20 IPC,
  6 Lua client, 79 shared types). This pass adds 18 tests: ten layout identity
  regressions, four native handoff/associated-window regressions, three shared
  snapshot/return/wire regressions, and one protocol-3 rejection regression.
  Existing worker tests now also assert that return values retain the original
  nondefault snapshot binding.
- Locked Lua-free daemon tests: 590 passed, 2 ignored. Shared types without
  Lua: 75 passed. Native session and associated-window checks are exercised in
  the Lua-free build as well.
- Strict workspace/all-targets, Lua-free/all-targets, and standalone Lua-module
  Clippy passed. Formatting and diff-whitespace checks passed. Locked workspace
  and Lua-free compilation both passed.
- Logs: `/tmp/spool-main-review-20260908-snapshot-identity-red.log`,
  `/tmp/spool-main-review-20260908-snapshot-identity-focused.log`,
  `/tmp/spool-main-review-20260908-snapshot-identity-workspace.log`,
  `/tmp/spool-main-review-20260908-snapshot-identity-without-lua.log`, and
  `/tmp/spool-main-review-20260908-snapshot-identity-shared-without-lua.log`.
- The two private SkyLight Space-operation tests remain ignored. All window
  operations used mock state; IPC tests used temporary endpoints. No live
  desktop manipulation, installation, daemon restart, publication, staging,
  or Git commit occurred. The release goal remains active.

## Command Atomicity and Column Offsets (2026-09-08)

Scope: ordinary stack/resize command rejection, complete `Balance`/`Equalize`
size planning, unstack width admission, and horizontal layout projection. This
pass continues the main-program release review; Nix and unrelated packaging
remain excluded. No native desktop manipulation or daemon deployment occurred.

### Confirmed and Fixed

- **P1: representable individual widths could overflow the strip projection.**
  A three-column regression with widths `i32::MAX - 1`, `1`, and `1` panicked
  at `left_edge += width`. `column_positions` now checks the entire offset
  sequence before exposing an entry. An invalid sum returns no partial prefix;
  the strip's membership and ordering are not truncated. A control with total
  width exactly `i32::MAX` retains every column and its positive frame.
- **P2: rejected commands erased maximize restoration state.**
  The old leftmost stack toggle cleared `FullWidthMarker` and requested a
  reshuffle despite no layout change. Native fullscreen source/destination
  cases follow the same rejection path. Height resize outside a vertical stack
  also cleared the marker despite being inapplicable. The regression run failed
  both assertions before repair. Stack/unstack now report whether they changed
  anything; the toggle plans against an unchanged source strip, validates the
  candidate, and only then replaces it and clears restoration state. Tests also
  verify that a rejected toggle does not mark the ECS strip changed. Rejected
  or unchanged tiled-height requests preserve the marker.
- **P2: `Balance` submitted an invalid or partial resize batch.**
  The original width-budget regression queued three roughly one-billion-pixel
  widths. Each fit individually, but their total could not be represented.
  Planning now validates the complete proposed width sum, available affected
  members, and each pending-position/new-size endpoint before modifying any
  resize or restore marker. Native fullscreen columns remain part of the sum
  but are not resized. A separate endpoint-overflow regression preserves all
  prior markers; a positive control accepts the maximum uniform width that
  actually fits the strip.
- **P2: `Equalize` overwrote previously queued width requests.**
  The regression showed a queued width of 600 being reset to the old width of
  400. Both batch sizing commands now use `requested_frame`, retaining earlier
  orthogonal-dimension requests. Equalize validates its viewport, division,
  available members, and all proposed endpoints before queuing any changes.
  Missing geometry or one overflowing member preserves the whole prior batch.
- **Unstack admission and recovery:** ordinary toggles and Lua unstack validate
  the proposed strip before replacing the current one. Tests reject a split
  whose extra column exceeds the width budget, then allow the same split after
  widths become representable, retaining the ordinary command's restoration
  state until successful execution.

### Verification

- The first three command regressions failed before repair. The projection
  regression reproduced an integer-overflow panic, with its exact-boundary
  control passing. The Equalize regression independently reproduced the lost
  pending width. All now pass.
- Locked workspace tests: 839 passed, 2 ignored (734 daemon, 20 IPC, 6 Lua
  client, 79 shared types). This pass adds 12 tests: nine ordinary-command
  regressions/controls, two pure offset tests, and one Lua unstack regression.
- Locked Lua-free daemon tests: 601 passed, 2 ignored. Eleven new tests run in
  this configuration; the Lua replay test is feature-gated.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks all passed.
- Logs: `/tmp/spool-main-review-20260908-command-atomicity-red.log`,
  `/tmp/spool-main-review-20260908-column-projection-red.log`,
  `/tmp/spool-main-review-20260908-equalize-red.log`,
  `/tmp/spool-main-review-20260908-command-atomicity-workspace.log`, and
  `/tmp/spool-main-review-20260908-command-atomicity-without-lua.log`.
- The same two private SkyLight operation tests remain ignored. Evidence is
  from pure layouts, mock ECS worlds, and temporary IPC endpoints, not native
  WindowServer acceptance. No deployment, daemon restart, publication, staging,
  or commit occurred.

### Remaining Scope

This is not an exhaustive geometry audit. Next candidates include ordinary
resize's use of current rather than pending frames, maximize/snap mutation
admission, and arithmetic that translates large logical offsets into global
coordinates. Missing-member height projection also needs a separate semantics
review. Native `ws:tab` creation, follow/view prediction, configuration-dependent
retile placement, and authorized real multi-display acceptance remain open.
The release goal remains active; no release-ready claim is made.

## Resize, Maximize, and Snap Geometry (2026-09-08)

Scope: ordinary command inputs, restore-state admission, native tab width
consistency, viewport centering/clamping, and explicit snap translation. This
pass continues the main-program review without Nix, unrelated packaging, or
live desktop changes.

### Confirmed and Fixed

- **P1: resizing a valid extreme-coordinate frame overflowed its center.**
  The initial regression panicked inside integer-vector addition when computing
  `IRect::center`, even though both endpoints and the requested size were
  individually representable. Centering now uses wide intermediate sums and
  preserves the existing integer rounding. Viewport clamping intersects the
  panning range with representable origins and positive-size endpoints.
- **P1: snap could overflow the strip translation.**
  A representable focused frame combined with a logical `i32::MIN` offset
  panicked in vector subtraction. Both translation axes now use checked
  subtraction; an unrepresentable result leaves the previous request untouched.
- **P2: ordinary height resize discarded an earlier size request.**
  The red test queued `600 x 300`, then requested a height increase. Reading
  observed geometry instead of the pending request left it at `600 x 300`
  instead of `600 x 340`. Ordinary resize and maximize now read
  `Windows::requested_frame`. `moving_frame` also overlays newer resize and
  reposition requests on the desired projection, so snap does not resurrect an
  older desired position or size.
- **P2: invalid maximize restoration cleared the saved restore marker.**
  The original zero-ratio regression lost its marker before width validation.
  Restore ratios, saved floating rectangles, all affected column members, and
  the complete proposed strip width are now validated before any membership,
  request, or restoration-state change. Maximize plans an unstack on a cloned
  strip and only commits it after admission.

### Additional Boundary Coverage

- A pending floating frame survives maximize and an immediate second maximize,
  including both its origin and size. Zero-size, reversed, and excessively wide
  saved rectangles are rejected without new requests or lost restore state.
- Native tab members resize together. Switching focus to another member after
  maximizing still finds the group's restoration marker and restores its prior
  pending width. Native fullscreen columns are not resized, maximized, or snapped.
- Width changes prepare every affected member before changing any request.
  A sibling whose new endpoint would overflow preserves the whole previous
  batch and every restore marker, for both vertical stacks and native tabs.
- A maximize split that would exceed the strip width budget preserves membership
  and requests. Reducing the retained sibling's width allows the same operation
  to succeed later.
- Pure clamp tests cover coordinate minima/maxima and oversized windows, assert
  representable endpoints, permitted panning ranges, and idempotence. Center
  controls retain existing rounding for odd dimensions and negative origins.
  Moving-frame tests reject invalid desired, pending, and fallback geometry.

### Verification

- All four initial command regressions failed before repair, including two
  integer-overflow panics. All now pass; the expanded command suite passes 34
  tests.
- Locked workspace tests: 853 passed, 2 ignored (748 daemon, 20 IPC, 6 Lua
  client, 79 shared types). This pass adds 14 tests: eleven command regressions
  and controls, two pure geometry tests, and one moving-frame test.
- Locked Lua-free daemon tests: 615 passed, 2 ignored. All 14 new tests run in
  both daemon configurations.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  The initial lint run requested the standard integer midpoint helper and
  explicit default/tolerant floating-point assertions in new tests; those
  were corrected without changing the tested rounding contract.
- Logs: `/tmp/spool-main-review-20260908-geometry-commands-red.log`,
  `/tmp/spool-main-review-20260908-geometry-commands-focused.log`,
  `/tmp/spool-main-review-20260908-geometry-commands-workspace.log`, and
  `/tmp/spool-main-review-20260908-geometry-commands-without-lua.log`.
- The two private SkyLight operation tests remain ignored. Tests used pure
  geometry, mock ECS worlds, and temporary IPC endpoints. No native desktop
  manipulation, deployment, daemon restart, publication, staging, or commit
  occurred.

### Remaining Scope

The release goal remains active. Global-coordinate translation in layout
reshuffling/projection and missing-member height projection still need review;
this pass does not claim an exhaustive arithmetic audit. Native `ws:tab`
creation, follow/view prediction, configuration-dependent retile placement,
and authorized real multi-display acceptance remain open.

## Layout Projection and Coordinate Conversion (2026-09-08)

Scope: missing-member projection, column-width consistency, ensure-visible
request batches, global frame arithmetic, viewport rebasing, and a persistence
defect exposed by the full regression suite. Nix and unrelated packaging remain
excluded. No live desktop operations or daemon deployment occurred.

### Confirmed and Fixed

- **P2: missing geometry misaligned stacked height allocation.** Heights were
  filtered independently and then zipped with the original unfiltered items.
  A missing master suppressed the entire column; a missing interior item could
  prevent a later eligible item from receiving a frame. Projection now keeps
  eligible members paired with their representative sizes before allocation.
  Missing/invalid geometry is omitted only from the projection; membership and
  ordering are retained. Existing native tab items choose their first eligible
  member and emit only eligible siblings, without inventing a new group.
- **P2: a wider follower created a transient inter-column gap.** Projected
  windows adopted their master's width, but offsets used the widest member.
  The red test rendered a 300-pixel column and started the next at 700 instead
  of 300. Both calculations now use the same eligible master. Complete strip
  offset validation remains in place.
- **P2: an earlier ensure-visible no-op starved later requests.** The first
  already-visible window returned from the whole system. Invalid targets,
  missing geometry, and newly active strips had the same batch-abort shape.
  These cases now skip only their own request; a later valid request is still
  handled. Tests include a no-op, an unrepresentable translation, and missing
  geometry before a window that needs scrolling.
- **P1: global layout arithmetic could panic before reaching a valid result.**
  Red tests reproduced overflows when adding strip/window offsets, calculating
  a discarded vertical reshuffle offset, and applying display-origin changes.
  Global frame calculations now remain wide through existing padding/sliver
  projection and narrow only a representable final frame. Ensure-visible does
  the same for its horizontal candidate. Reshuffle checks its horizontal
  translation and total width, and uses a full-range movement distance.
- **P2: viewport rebasing partially mutated state before failure.** A valid
  current origin was changed before an overflowing pending target panicked.
  Both candidates are now validated before changing either or advancing the
  saved viewport. A recovery control makes the pending target representable
  and verifies that the retained old viewport still produces the correct rebase.
- **P2: incomplete native observations still overwrote the durable snapshot.**
  The first full run exposed an existing save test failing only when the wall
  clock crossed a second. Save admission checked the retained ECS structure but
  never the current native observation, so it rewrote the file despite unavailable
  topology. Both periodic and exit saves now require a complete native catalog
  before snapshot extraction. Tests seed a fixed old timestamp, preserve the
  exact file on failure, and permit saving again after observation recovers.

### Additional Coverage

- A mock WindowServer omission of each of three stacked windows reflows the
  other members without gaps, preserves stored order and the missing member's
  dimensions, and restores the projection when the surface reappears.
- Pure projection controls cover absent first/interior/last members, partial
  native tab availability, invalid rectangle spans, and nonpositive viewports.
  Height allocation rejects invalid inputs and avoids minimum-height
  multiplication overflow.
- Sliver controls retain horizontal-padding compensation, negative-display
  origins, swipe behavior, and stacked heights. Extreme logical offsets are
  accepted when their final projected frame fits; unrepresentable final
  endpoints are rejected. A representable `i32::MAX` width remains accepted.

### Verification

- The first three projection regressions failed before repair. Six additional
  global-coordinate/batch regressions then failed, including five overflow
  panics. The repaired layout suite passed 42 tests before the final controls
  were added; the mock omission/recovery integration also passed.
- The first complete run passed 761 daemon tests but exposed the existing state
  save defect. Seeding a deterministic timestamp reproduced both the original
  unavailable-display case and a new partial-native-catalog case. After repair,
  all 19 state tests passed, including periodic/exit save recovery.
- Final locked workspace tests: 868 passed, 2 ignored (763 daemon, 20 IPC,
  6 Lua client, 79 shared types). This pass adds 15 tests: thirteen layout
  projection/geometry/batch regressions and controls, one mock omission/recovery
  integration, and one periodic/exit persistence regression. The existing
  unavailable-display save test now uses a deterministic timestamp as well.
- Locked Lua-free daemon tests: 630 passed, 2 ignored. All 15 new tests run in
  both daemon configurations. The same two private SkyLight operation tests
  remain ignored.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Final logs: `/tmp/spool-main-review-20260908-layout-projection-workspace-final.log`
  and `/tmp/spool-main-review-20260908-layout-projection-without-lua.log`.
- Evidence logs: `/tmp/spool-main-review-20260908-layout-projection-red.log`,
  `/tmp/spool-main-review-20260908-layout-global-red.log`,
  `/tmp/spool-main-review-20260908-layout-global-focused.log`,
  `/tmp/spool-main-review-20260908-layout-membership-integration.log`,
  `/tmp/spool-main-review-20260908-layout-state-save-red.log`, and
  `/tmp/spool-main-review-20260908-layout-state-save-focused.log`.

### Remaining Scope

This pass is not proof that every command, frame-comparison, or platform-padding
calculation is safe. Native `ws:tab` creation, follow/view prediction, and
configuration-dependent retile placement remain open. Real multi-display and
WindowServer acceptance still requires authorization; mock tests are not a
substitute. No installation, daemon restart, publication, staging, or commit
occurred. The release goal remains active.

## Focus and Space Prediction (2026-09-08)

Scope: `WindowSet` focus/current/view/follow predictions and the native
visibility flags supplied by snapshot extraction. No native backend, move
transaction, desktop, or deployment behavior was changed. Nix and unrelated
packaging remain excluded.

### Confirmed and Fixed

- **P2: snapshots confused per-display visibility with global activity.**
  `WorkspaceSet.active` used `ActiveWorkspaceMarker`, although it means the
  Space shown on its own display. A visible Space on a secondary display was
  marked inactive, and retained global markers supplied a current Space even
  when native visibility was unknown. Extraction now uses the current topology
  observation. `current()` also requires unique active-display and visible-Space
  observations instead of choosing the first display or first active entry.
  A final audit also reproduced stale `ActiveDisplayMarker` fallback when the
  active-display query failed but visible-Space queries succeeded. Both activity
  levels now come from the same topology epoch; a failure or unknown display ID
  leaves `current()` unknown while preserving known visible windows and strips.
- **P2: focus predictions left ownership and column selection stale.**
  Focusing a known window updated only focus flags, leaving `current()` on the
  previous display/Space and native-tab navigation on its previous selected
  member. Focus now predicts the owning display/Space and selects the existing
  member without changing tab order. A missing window can no longer become a
  nonexistent focused ID; only its original intent is recorded.
- **P2: view predictions retained the previous display, focus, and visibility.**
  A target Space was marked active without activating its display. The root and
  window records could retain focus on a different Space, whose windows also
  remained marked visible. View now updates both activity levels and preserves
  other displays' visible Spaces. It retains focus only when the known focused
  window already belongs to the target, and clears visibility on newly hidden
  Spaces without claiming that newly shown windows have become visible.
- **P2: following moves ignored `follow` in the predicted tree.**
  Membership changed, but the source remained current and focused. Requesting
  a window's existing Space returned early even when following was requested.
  Follow now predicts destination activity and explicit window focus while
  preserving same-Space order and width. A non-following move of the focused
  window clears its stale focus; unrelated focus is retained. Moving also
  invalidates the old visibility observation and preserves floating mode.
- **Boundary control: ambiguous Space targets are not selected arbitrarily.**
  Focus/view/shift resolve a unique destination before mutation. Missing or
  duplicate Space targets retain the tree and record only the requested op.
  Focus and activation helpers do not append extra operations or rebind the
  original immutable snapshot.

### Coverage and Evidence

- Eight new pure regressions failed before repair. Two new extraction tests
  also failed, alongside nine passing existing extraction controls. Logs:
  `/tmp/spool-main-review-20260908-focus-prediction-red.log` and
  `/tmp/spool-main-review-20260908-focus-extraction-red.log`.
- Sixteen tests were added: eleven pure prediction tests, one Lua-chain test,
  three mock extraction tests, and one same-display native-follow integration.
  Controls include floats across displays, unchanged same-Space requests,
  missing targets, duplicate Space IDs, unknown/ambiguous activity, immutable
  branching, native-tab selection/order, and original plan provenance.
- The native-follow integration verifies that the pending observed snapshot
  still identifies the source, then compares current Space, membership, and
  focus after membership and visibility confirmation. It does not compare
  predicted geometry or claim cross-display native focus support.
- Initial focused suites passed: twelve shared prediction/Lua tests, eleven
  snapshot extraction tests, and the new native-follow integration. Existing
  structured stack prediction tests also passed. Logs use the `focus-prediction-focused-final`,
  `focus-extraction-focused`, and `focus-native-follow` suffixes under
  `/tmp/spool-main-review-20260908-`.
- The first locked full workspace run passed: 883 passed, 2 ignored (766 daemon,
  20 IPC, 6 Lua client, 91 shared types). The ignored tests still require the
  private SkyLight runtime. Log: `/tmp/spool-main-review-20260908-focus-workspace.log`.
- The subsequent active-display regression failed before repair, including a
  failed query with otherwise valid visibility. Its controls cover an unknown
  display ID, a new observed active display while ECS markers remain unchanged,
  and recovery. Red log: `/tmp/spool-main-review-20260908-focus-active-display-red.log`.
- After the final repair, all twelve extraction tests passed, followed by the
  complete locked workspace: 884 passed, 2 ignored (767 daemon, 20 IPC,
  6 Lua client, 91 shared types). Final logs:
  `/tmp/spool-main-review-20260908-focus-extraction-final.log` and
  `/tmp/spool-main-review-20260908-focus-workspace-final.log`.
- Final locked Lua-free daemon tests passed: 634 passed, 2 ignored. Log:
  `/tmp/spool-main-review-20260908-focus-without-lua-final.log`.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Logs: `/tmp/spool-main-review-20260908-focus-check.log`,
  `/tmp/spool-main-review-20260908-focus-check-without-lua.log`,
  `/tmp/spool-main-review-20260908-focus-clippy-complete.log`, and
  `/tmp/spool-main-review-20260908-focus-clippy-without-lua-complete.log`.

### Remaining Scope

The release goal remains active. The predicted tree still does not guarantee
that native requests succeed or that later operations wait for asynchronous
Space transitions. The real backend rejects Space-focus targets outside the
currently active display; the mock's cross-display Focus implementation is not
evidence to the contrary. Associated native-tab movement may differ from the
single-window `shift` prediction. Native `ws:tab` creation, configuration-dependent
retile placement, broader platform arithmetic, and authorized real multi-display
acceptance remain open. No daemon installation/restart, publication, staging,
or commit occurred.

## Native Move Transactions (2026-09-08)

Scope: runtime move admission, captured layout, native confirmation, follow
cancellation, and visibility gates in `src/ecs/native_space.rs`. Nix and unrelated
packaging remain excluded. All runtime evidence below is mock ECS/native-backend
evidence, not real desktop acceptance.

### Confirmed and Fixed

- **P1: whole-column confirmation orphaned additional associated windows.**
  The native request included associated IDs outside the requested column, and
  confirmation removed all captured members from their source strips. The
  destination received only the requested column, so an additional tracked
  tiled member disappeared from every strip while its move barrier was released.
  Column transactions now capture those members' existing source layouts too,
  preserving stacks and native-tab order rather than inventing a new tab group.
- **P2: confirmation resurrected a window's old tiled classification.**
  A member changed to floating while the move was pending was still appended
  through the captured column. Captured layouts are now filtered using current
  floating state, for both requested and additional associated members. Empty
  results do not create columns, and surviving groups retain their structure.
- **P2: retained visibility authorized follow-focus after a failed native read.**
  A stale `VisibleNativeSpaceMarker` was enough to focus a moved window despite
  a failed current visibility read or a successful observation of another Space.
  Follow now requires current topology visibility on a unique owning display,
  together with the existing identity, availability, membership, and live-target
  checks. A positive fresh observation works before marker projection catches up.
- **P2: disabling Space control did not cancel delayed follow actions.**
  Reconciliation could submit Space focus after control was disabled, or execute
  an already queued window focus. Uncompleted follows are now discarded on an
  observed disabled configuration, including while movement confirmation is
  pending. Submitted movement still reconciles; reenabling does not revive an
  old follow, and no speculative native rollback is issued.
- **P2: same-Space requests could split or reorder existing layout.**
  They always submitted native movement and froze the windows, then replayed
  a captured column or tab group over the existing layout. A regression changed
  a two-window stack into two single columns. Complete native membership plus
  matching ECS placement now permits a true no-op, with optional follow-focus.
  If admission cannot prove that no-op, confirmation preserves an already
  complete target layout rather than regrouping it.
- **P2: a newer accepted move left an older follow pending.**
  A newer stay request still allowed the old follow to focus the window later.
  Accepted moves now cancel pending follows only for their affected members,
  including confirmed same-Space requests. Unrelated pending follows remain.

### Coverage and Evidence

- The first focused run reproduced all three initial failures: stale visibility,
  disabling control, and late floating mode. Log:
  `/tmp/spool-main-review-20260908-native-transaction-red.log`.
- Later red tests reproduced same-Space movement, an old follow surviving a
  new stay request, stack splitting after an unknown admission read, and the
  missing associated member. Logs use the `native-transaction-follow-fixed-final`,
  `native-follow-supersede-red`, `native-noop-confirmation-red`, and
  `native-associated-red` suffixes under `/tmp/spool-main-review-20260908-`.
- A guard added during this pass also exposed an incomplete no-op optimization:
  native membership could be current while ECS still retained the source layout.
  The corrected path skips the duplicate OS move but still reconciles layout.
  Corrective regression: `/tmp/spool-main-review-20260908-native-noop-lag-red.log`.
- Thirteen tests were added. Controls include single/column requests with and
  without following, observation failure and recovery, stale and lagging markers,
  cancellation before/after Space-focus submission and during membership waits,
  unrelated follows, original and associated tab groups, partial/all floating
  transitions, native/ECS observation lag, and released transaction barriers.
- All 25 focused native-move tests passed, including existing reused-identity,
  partial-move timeout, source-order recovery, unexpected-destination recovery,
  floating-window, and script snapshot-binding cases. Final focused log:
  `/tmp/spool-main-review-20260908-native-transaction-focused-final.log`.
- Locked full workspace tests passed: 897 passed, 2 ignored (780 daemon,
  20 IPC, 6 Lua client, 91 shared types). Locked Lua-free daemon tests passed:
  647 passed, 2 ignored. All thirteen new tests run in both daemon configurations.
  The ignored tests still require the private SkyLight runtime. Final logs:
  `/tmp/spool-main-review-20260908-native-transaction-workspace.log` and
  `/tmp/spool-main-review-20260908-native-transaction-without-lua.log`.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Logs use the `native-transaction-check`, `native-transaction-check-without-lua`,
  `native-transaction-clippy`, and `native-transaction-clippy-without-lua`
  suffixes under `/tmp/spool-main-review-20260908-`.

### Remaining Scope

The release goal remains active. This pass does not make deferred layout plans
synchronous or implement native `ws:tab` creation. Associated movement versus
the pure single-window `shift` prediction, raw associated-window availability,
hidden/minimized transitions, direct focus commands interleaving with pending
follows, configuration-dependent retile placement, and broader platform
arithmetic still need review. Real multi-display/WindowServer acceptance remains
unauthorized and unperformed. No daemon installation/restart, publication,
staging, or commit occurred.

## Native Move Visibility and AX Availability (2026-09-08)

### Confirmed and Fixed

- **P2: movement confirmation reinserted hidden or minimized members.**
  The captured layout was filtered only by floating classification. A visibility
  observer removed the member, but confirmation then restored it into a live
  strip while leaving its remembered route at the source. Confirmation now
  projects only visible tiled members, records the confirmed destination and
  captured insertion index for hidden tiled members, and clears an obsolete
  remembered route when a visible member is placed. Show/restore uses the
  existing retile observer instead of a second layout mechanism.
- **P2: delayed follow could focus a window after it was hidden or minimized.**
  Availability alone did not exclude these windows. An observed hide/minimize
  now cancels both the movement's unsubmitted follow and an already queued
  window follow, including during membership or AX waits. Showing again does
  not revive it. Already hidden members are not followed at move admission.
  Already-issued native Space-focus events cannot be recalled.
- **P2: hidden same-Space requests unnecessarily froze and reinserted windows.**
  No-op detection required live strip membership even though hidden windows
  intentionally have none. It now accepts a matching remembered destination
  with no live owner. A mismatched route still reconciles. Admission also
  retains the remembered insertion index when no source column is present.
- **P2: unavailable tracked associations were omitted from move identities.**
  Native movement included their numeric IDs while the available-only identity
  collection excluded them, bypassing their transaction ownership and geometry
  freeze. Admission now distinguishes an untracked native association from a
  tracked but temporarily AX-unavailable one and rejects the latter before
  issuing movement. Recovery permits a fresh request with all tracked members.
  Withdrawal after acceptance continues to defer confirmation without deleting
  source layout or releasing the geometry barrier early.

### Coverage and Evidence

- Four new failing regressions reproduced the four issues while all 25 existing
  native-move tests passed. Evidence:
  `/tmp/spool-main-review-20260908-native-visibility-red-final.log`.
- Eight tests were added in `src/tests/native_move.rs`. Controls cover hidden
  and minimized members before/after submission, partial/all hidden columns,
  one-time restoration, same-Space no-ops, retained insertion index, follow
  cancellation before/after native Space-focus submission and during failed
  membership reads, unavailable associations, AX recovery with and without
  hiding, and timeout recovery through the ordinary membership audit.
- The timeout control confirms that transaction ownership is released while
  an unavailable member's geometry barrier remains; its later hidden recovery
  records the confirmed destination without native rollback or expired follow.
- All 33 focused native-move tests passed. Log:
  `/tmp/spool-main-review-20260908-native-visibility-focused-final.log`.
- Locked full workspace tests passed: 905 passed, 2 ignored (788 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-native-visibility-workspace.log`.
- Locked Lua-free daemon tests passed: 655 passed, 2 ignored. All eight new
  tests run in both daemon configurations. The ignored tests still require
  the private SkyLight runtime. Log:
  `/tmp/spool-main-review-20260908-native-visibility-without-lua.log`.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Logs use the `native-visibility-check`, `native-visibility-check-without-lua`,
  `native-visibility-clippy`, and `native-visibility-clippy-without-lua` suffixes
  under `/tmp/spool-main-review-20260908-`.

### Remaining Scope

The release goal remains active. Direct focus commands interleaving with pending
follows, configuration-dependent retile placement, associated movement versus
the pure single-window `shift` prediction, and broader platform arithmetic still
need review. This phase does not change explicit focus-command policy, promise
native tab creation, or infer real macOS behavior from mock ECS tests. Real
multi-display/WindowServer acceptance remains unauthorized and unperformed.
No daemon installation/restart, publication, staging, or commit occurred.

## Explicit Focus and Deferred Follow Ordering (2026-09-08)

### Confirmed and Fixed

- **P2: an older native follow overrode a new explicit window or Space choice.**
  Direct focus did not cancel either a movement's unsubmitted Space follow or
  its already queued window follow. A later accepted explicit window request
  or successful Space selection now discards those follows, while preserving
  movement confirmation and geometry-barrier recovery. Unknown/stale window
  endpoints and rejected platform Space-focus requests leave a valid follow
  intact. Native window-focus APIs still have their existing asynchronous,
  no-result interface; this is request admission, not proof of OS focus success.
- **P2: script focus could execute before a preceding deferred movement.**
  `shift_following(0, target, true).focus(2)` ended focused on window 0 rather
  than window 2. Script focus now retains its original snapshot through the
  same deferred handoff as move/view. Named focus, Space selection, and native
  moves share a command executor that flushes each command's observers before
  accepting the next. The reverse order still follows window 0 normally.
- **P2: two unrelated accepted follows both claimed focus.**
  An older follow remained queued after a newer followed move, producing focus
  requests `[0, 2]` instead of just `[2]`. A newly accepted follow now supersedes
  all older pending follows. A move without following still cancels only its
  affected members' follows. For a confirmed same-Space follow, replacement is
  conditional on successful platform Space-focus submission.

### Behavioral Boundary

`FocusRequestKind` distinguishes explicit interaction from automatic restoration.
Native follow completion, Space focus restoration, tab detection, and retaining
the previous focus for a `dont_focus` window use the automatic path. These do
not cancel the follow they may be helping to complete. Ordinary AX observations
remain observations rather than cancellation signals. Already-issued platform
events cannot be recalled, and no speculative Space rollback was introduced.

### Coverage and Evidence

- The initial focused run reproduced all three first regressions: new window
  focus, new Space selection, and script operation ordering. The previous 33
  native-move tests passed. Log:
  `/tmp/spool-main-review-20260908-native-focus-order-red.log`.
- Additional controls reproduced the competing-follow failure, with 41 passing
  tests and one failure. Log:
  `/tmp/spool-main-review-20260908-native-focus-order-controls.log`.
- Ten tests were added. They cover cancellation before/after Space-focus
  submission, source and destination Space selections, both script operation
  orders, both same-batch named-focus/move orders, automatic restoration,
  unknown endpoints, stale sessions, identity replacement after script handoff,
  rejected platform focus, rejected same-Space follow, and a newer unrelated
  followed move. Existing selective stay-cancellation tests remain unchanged.
- All 43 focused native-move tests passed. Log:
  `/tmp/spool-main-review-20260908-native-focus-order-focused-final.log`.
- Locked full workspace tests passed: 915 passed, 2 ignored (798 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-native-focus-order-workspace.log`.
- Locked Lua-free daemon tests passed: 663 passed, 2 ignored. Eight new tests
  run in both daemon configurations; the two script-handoff/order integration
  tests require Lua. The ignored tests still require the private SkyLight
  runtime. Log:
  `/tmp/spool-main-review-20260908-native-focus-order-without-lua.log`.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Logs use the `native-focus-order-check`, `native-focus-order-check-without-lua`,
  `native-focus-order-clippy`, and `native-focus-order-clippy-without-lua` suffixes
  under `/tmp/spool-main-review-20260908-`.

### Remaining Scope

The release goal remains active. This executor does not establish global order
across all independent command-system readers or across raw actions and script
plans before their deferred handoff. Direct-focus visibility/membership policy,
configuration-dependent retile placement, associated movement versus the pure
single-window `shift` prediction, and broader platform arithmetic remain review
targets. Mock tests do not prove native multi-display focus or AX ordering.
No daemon installation/restart, real desktop manipulation, publication, staging,
or commit occurred.

## Named Window Focus Visibility (2026-09-07 UTC)

### Confirmed and Fixed

- **P2: a retained visible marker authorized focus on a now-invisible Space.**
  Ordinary named focus inspected the window's ECS strip without observing the
  native Space at the request. It still focused after a native visibility
  change or read failure. Admission now samples topology and visibility inside
  the command executor, checks complete unique native membership, and requires
  one currently visible owning display before sending window focus.
- **P2: windows outside live strips bypassed the Space check.**
  Floating, hidden, minimized, and not-yet-placed windows had no strip that the
  guard could reject. They could therefore trigger focus despite belonging to
  an invisible Space. All tracked window modes now use the native membership
  check. Missing, overlapping, or unreadable membership is not replaced with
  the active display or retained layout ownership.
- **P2: stale inactive layout rejected actually visible windows.**
  Focus was rejected when native visibility or membership had already changed
  but strip markers had not caught up, including a successful Space selection
  earlier in the same command batch. Current observation now outranks the old
  projection. It does not wait for native transitions: if the earlier Space
  request has not become observable, the later ordinary focus is still rejected.

### Behavioral Boundary

`NativeTopology` owns fresh named-focus admission and a shared unique-visible-
Space predicate used by follow completion. A secondary visible display need not
be active or have a known global active-display identity. Only its own visibility
must be known, while globally complete Space topology remains necessary to prove
unique membership. Duplicate entries within one native Space are harmless;
duplicate owning displays are not. A visible fullscreen Space is focusable.

The new `FocusSpacePolicy` names the existing distinction explicitly: ordinary
window-by-ID focus stays on currently visible Spaces, while bound script `focus`
retains its native-activation request behavior and identity validation. Space
control need not be enabled for an ordinary visible-window focus request.
Hidden/minimized state does not bypass ownership, but does not itself revoke
the existing explicit window-focus API. A rejected ordinary request does not
cancel a valid pending follow, and failed visibility continues to defer that
follow until a fresh observation recovers.

### Coverage and Evidence

- Five new regressions failed before the fix, covering stale visible markers,
  non-layout windows, missing/ambiguous/failed membership, stale inactive layout,
  and same-batch Space selection. The filter also ran one existing script-focus
  identity test, which passed. Log:
  `/tmp/spool-main-review-20260908-native-focus-visibility-red.log`.
- Twelve tests were added in `src/tests/native_focus.rs`. Positive and recovery
  controls cover visible detached windows, private-control-disabled focus,
  secondary displays with unknown active identity, unrelated visibility errors,
  incomplete topology, duplicate ownership, display removal, fullscreen,
  duplicate membership entries, retained follows, and bound script activation.
- All 12 focused tests and all 43 native-move tests passed. Logs:
  `/tmp/spool-main-review-20260908-native-focus-visibility-focused-final.log` and
  `/tmp/spool-main-review-20260908-native-focus-visibility-moves.log`.
- Locked full workspace tests passed: 927 passed, 2 ignored (810 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-native-focus-visibility-workspace.log`.
- Locked Lua-free daemon tests passed: 675 passed, 2 ignored. All twelve new
  tests run in both daemon configurations. Log:
  `/tmp/spool-main-review-20260908-native-focus-visibility-without-lua.log`.
- Locked workspace and Lua-free compilation, strict workspace/all-targets and
  Lua-free/all-targets Clippy, formatting, and diff-whitespace checks passed.
  Logs use the `native-focus-visibility-check`,
  `native-focus-visibility-check-without-lua`, `native-focus-visibility-clippy`,
  and `native-focus-visibility-clippy-without-lua` suffixes under
  `/tmp/spool-main-review-20260908-`. Run filenames retain the host-local date;
  the log timestamps and this phase heading use UTC.

### Remaining Scope

The release goal remains active. Other focus entry points, global ordering
across independent command readers and raw/script handoffs, configuration-driven
retile placement, associated movement versus single-window prediction, and
broader platform arithmetic remain review targets. Neither the new admission
tests nor the existing native-move mocks prove real macOS Space/focus ordering.
No daemon installation/restart, real desktop manipulation, publication, staging,
or commit occurred.

## Ordered Runtime Actions (2026-09-08 UTC)

### Confirmed and Fixed

- **P2: repeated commands were processed only one at a time per frame.**
  Most window handlers stopped at the first matching message. Three queued
  right-focus actions produced only one request, and two maximize actions did
  not restore the original state in the batch. The unified dispatcher consumes
  every action once and preserves its input order.
- **P2: independent readers reordered native, script, and window actions.**
  Named focus followed by directional focus produced the wrong destination.
  Script focus returned to the message bus behind a later named action, and
  local script operations competed with ordinary window operations. All runtime
  mutations now share `commands::dispatch_actions`. Cached nested systems flush
  their observers between actions and between operations inside a script plan.
  Internal cross-display fallbacks stay within the current command instead of
  publishing a new action behind later input, and use the same requested target
  as the preceding local movement.
- **P2: a following mutation targeted the previously confirmed focus.**
  Focus-then-resize resized the old window while the new request awaited native
  confirmation. Mutations now use the accepted pending request, without changing
  `FocusedMarker`. Layer toggles use that same target, so two toggles return to
  the original layer. A hidden/minimized pending target does not mutate the old
  window; normal lifecycle cancellation restores confirmed-focus eligibility.
- **P2: pending focus on a different display used the source viewport.**
  A following center/balance could modify the remote window or unrelated source
  columns before display focus projection caught up. Geometry and strip-relative
  operations require active Space ownership, using strip membership for tiled
  windows and a successful native membership read for floating windows.
- **P2: repeated floating moves overwrote instead of accumulated.**
  Movement read observed geometry, ignoring earlier pending movement. It now
  uses requested frames. Center and cross-display size capture likewise include
  pending geometry; endpoint validation and native write barriers remain intact.

### Coverage and Evidence

- All five initial new regressions failed before implementation; one existing
  same-batch native-focus control passed. Log:
  `/tmp/spool-main-review-20260908-command-dispatch-red.log`.
- Twelve tests in `src/tests/command_dispatch.rs` cover immediate consumption,
  no replay, mixed focus order in both directions, script/local operations,
  pending resize, invalid-action continuation, floating movement accumulation,
  layer toggles, hidden/minimized targets, lifecycle cancellation, and multi-
  display geometry rejection. The native-move acceptance-order test now also
  covers directional focus before/after a pending follow.
- Existing direct-handler tests now use the production action dispatcher.
  Native deferred-payload identity tests retain explicit bound snapshots and
  inject replacement identities before native dispatch; no obsolete independent
  reader is retained solely for tests.
- Final locked workspace tests passed: 939 passed, 2 ignored (822 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-command-dispatch-workspace-final.log`.
- Final locked Lua-free daemon tests passed: 685 passed, 2 ignored. Ten of the
  twelve new tests also run without Lua. Log:
  `/tmp/spool-main-review-20260908-command-dispatch-without-lua-final.log`.
- Locked workspace and Lua-free compilation, strict all-targets Clippy in both
  configurations, formatting, and diff-whitespace checks passed. Logs use the
  `check`, `check-without-lua`, `clippy`, and `clippy-without-lua` suffixes under
  `/tmp/spool-main-review-20260908-command-dispatch-`.

### Remaining Scope

The release goal remains active. This phase establishes action submission and
local ECS ordering, not synchronous native completion. Lua callbacks remain
asynchronous; state queries and subscriptions are not serialized with actions.
Read-after-command behavior, configuration-driven retile placement, associated
movement versus single-window prediction, cross-display continuation, and broader
platform arithmetic remain review targets. Native multi-display acceptance is
still separate from mocks. No daemon deployment/restart, real desktop manipulation,
publication, staging, or commit occurred.

## Cross-Display Command Admission (2026-09-08 UTC)

### Confirmed and Fixed

- **P2: one directional action both reordered a stack and crossed a display.**
  Moving the lower item north to the top of its stack then tested the new edge
  position and transferred it again. Successful local movement now returns;
  a subsequent directional action may still cross the boundary.
- **P2: an unavailable destination left source layout and geometry half changed.**
  The old path removed the source item and warped the pointer before reading the
  target Space. Floating windows bypassed that read entirely. Preflight now
  validates fresh native ownership, target strip identity/parent, known matching
  display geometry, usable viewports, and the complete proposed layout before
  publishing any of those effects. Partial topology retains the source and a
  later successful observation permits retry.
- **P2: moving one native tab split its recorded group across displays.**
  The complete tab cohort is now preserved as one target item. Every member must
  be available, visible, still uniquely native-owned by the source Space, and
  outside another native transfer. Width-budget validation includes the proposed
  target group before either source or destination changes.
- **P2: a delayed resize retained stale transfer state.**
  The 150ms callback held a bare entity and original width ratio, without checking
  incarnation, current ownership, or newer geometry requests. It has been removed;
  the command now plans target size immediately alongside position. The unused
  generic `Timeout::callback` entry point was removed as well; existing timer
  duration tests still cover zero, nanosecond, and maximum durations.
- **P2: valid endpoint coordinates could overflow viewport/center arithmetic.**
  Cross-display preflight uses checked padding/Dock calculations, safe midpoints,
  and existing checked frame/column helpers. Tests cover both coordinate limits,
  invalid viewports, negative or overflowing Dock extents, and target strip width
  exhaustion without source or pointer changes.

### Coverage and Evidence

- The first four new regressions failed before implementation. Log:
  `/tmp/spool-main-review-20260908-cross-display-red.log`.
- Twelve regressions in `src/tests/display_commands.rs` cover local/transfer
  boundaries, tab cohesion, missing target rows, wrong parents, partial topology,
  recovery, pending geometry, superseding size requests, unavailable members,
  source membership freshness/ambiguity, and coordinate/width limits. Target
  failure controls cover both tiled and floating windows.
- Final locked workspace tests passed: 951 passed, 2 ignored (834 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-cross-display-workspace-final.log`.
- Locked Lua-free daemon tests passed: 697 passed, 2 ignored. All twelve new
  cross-display tests run in both configurations. Log:
  `/tmp/spool-main-review-20260908-cross-display-without-lua.log`.
- Locked workspace and Lua-free compilation, strict all-targets Clippy in both
  configurations, formatting, and diff-whitespace checks passed. Logs use the
  `check`, `check-without-lua`, `clippy`, and `clippy-without-lua` suffixes under
  `/tmp/spool-main-review-20260908-cross-display-`.

### Remaining Scope

The updated goal remains active: continue reviewing/refactoring the main program
with emphasis on bugs and edge cases, excluding Nix and other work unrelated to
the main program.
Frame-based cross-display layout still migrates at request submission, not after
AX/native acknowledgement. Rejected or constrained frame writes, delayed native
membership, associations beyond recorded native tabs, and first-peer selection
on three or more displays remain review targets. These mocks are not proof of
real macOS cross-display acceptance. No daemon deployment/restart, desktop
manipulation, publication, staging, or commit occurred.

## Frame-Based Display Transfer Confirmation (2026-09-08 UTC)

### Findings and Changes

- **P1: Submission was treated as successful migration.** After preflight,
  `ToNextDisplay` removed the source layout and applied focus/pointer effects
  before the shared AX committer ran. Rejected or constrained writes left the
  ECS destination ahead of native membership. The first new regression failed
  immediately on retained-source membership before the fix; evidence:
  `/tmp/spool-main-review-20260908-display-confirmation-red.log`.
- Ordinary frame-based display moves now share native transaction ownership,
  complete membership confirmation and timeout recovery. They do not submit
  private Space-control intents or require experimental Space control. Source
  layout and source geometry inputs remain intact until every captured member
  has a current entity/incarnation and uniquely belongs to the target Space.
- `DisplayTransferFrame` is an identity-bound, one-attempt request processed
  only by the shared frame committer. It is the controlled exception to the
  existing reassignment barrier. Commit-time checks reject stale membership,
  missing/misowned destinations, stale geometry, hidden/unavailable windows,
  and changed floating classification. Initialization still pending blocks
  admission. Default, pointer, audit and animation writes cannot use this
  exception to bypass the transfer's ownership.
- `DisplayTransferReadback` stages physical acceptance without overwriting the
  source Position, Bounds, desired frame or width ratio. Confirmation commits
  compatible staged geometry; classification changes do not replay geometry
  captured under the old tiled/floating policy. Timeout retires the one-shot
  request/readback and releases transaction ownership while the existing
  membership audit retains authority over the geometry barrier. No speculative
  native rollback or per-frame transfer retry is introduced.
- Follow and Stay restoration happen after confirmation, remain identity-bound
  and are superseded by later accepted focus commands. Display follow uses the
  current usable target viewport. A mismatch between cached topology and ECS
  geometry defers focus until a fresh topology observation rather than warping
  to a captured, stale coordinate.
- The tab cohort remains in the source projection on partial native arrival.
  Floats preserve size and never enter a tiled strip. Existing display tests
  now assert the retained-source stage and explicitly supply native membership
  before checking the target layout. AX mock writes still do not fabricate
  native membership confirmation.

### Coverage

Ten added tests bring `src/tests/display_commands.rs` to 22 tests. They cover
confirmation ordering, staged source geometry, AX rejection/constraining, delayed
Stay, superseding focus, current viewport refresh, floating transfer, pre-write
target/membership/visibility/classification changes, timeout without replay,
retired tab identity and pending initialization. Existing tab and two display
tests now cover partial and explicit final native confirmation.

### Verification

- All 22 cross-display regressions passed; log:
  `/tmp/spool-main-review-20260908-display-confirmation-boundaries-final.log`.
- Final locked workspace tests passed: 961 passed, 2 ignored (844 daemon,
  20 IPC, 6 Lua client, 91 shared types); log:
  `/tmp/spool-main-review-20260908-display-confirmation-workspace-final.log`.
- Locked Lua-free daemon tests passed: 707 passed, 2 ignored; log:
  `/tmp/spool-main-review-20260908-display-confirmation-without-lua.log`.
- Locked workspace and Lua-free compilation, strict all-targets Clippy in both
  configurations, formatting and diff-whitespace checks passed. Logs use the
  `check`, `check-without-lua`, `clippy` and `clippy-without-lua` suffixes under
  `/tmp/spool-main-review-20260908-display-confirmation-`.
- The intermediate full-suite failures were outdated immediate-migration
  expectations and missing mock topology refresh / legitimate visibility-policy
  changes in new tests. Final assertions retain native confirmation, deferred
  focus, zero stale writes and target non-admission checks rather than teaching
  mock AX writes to fabricate native success.

### Remaining Scope

The main-program review goal remains active. Associated windows outside recorded
native tab groups, three-or-more-display selection, concurrent layout commands
during pending movement, and destination-width admission after intervening
layout changes remain review targets. Real macOS AX/membership timing and
multi-display acceptance are not proven by mock tests. No live daemon action,
desktop manipulation, deployment, Nix work, publication, staging or commit was
performed.

## Layout Mutation Admission During Reassignment (2026-09-08 UTC)

### Findings and Changes

- **P1: Commands could mutate a transaction's retained source layout.** A native
  move captures its original column/tab structure, but local reordering,
  unstack/maximize and script mutations continued to treat retained source
  membership as writable placement. New commands could temporarily change that
  structure only to have completion restore the older captured shape. Three
  initial regressions reproduced direct reordering, indirect unstacking through
  an unowned sibling, and script swapping through a protected column. Log:
  `/tmp/spool-main-review-20260908-layout-admission-red.log`.
- `Windows::layout_is_writable` centralizes command admission using AX
  availability and both `NativeMoveOwner` and
  `WindowSpaceReassignmentPending`. A timed-out transaction therefore does not
  bypass a still-unresolved membership barrier. This query adds no mutable
  access to ownership components and follows the existing named-query pattern.
- Local geometry/structure commands reject a protected focused endpoint before
  changing restoration state or publishing geometry. Width, height equalizing,
  balance, maximize and stack operations preflight their affected members or
  original/destination columns. Reordering and bar column dragging include every
  shifted column between the endpoints, preventing indirect mutation through an
  apparently unrelated dragged window.
- Script `Swap`, `Stack`, `Unstack`, `SetWidth` and `SetFrame` use the same
  endpoint/column checks while preserving snapshot identity validation. A
  rejected operation does not cancel independent operations later in the plan.
  Rejected mutations are not stored for replay with stale source geometry after
  native movement completes.
- Focus and floating classification keep their own admission paths. Unrelated
  columns remain writable; transaction confirmation or ordinary membership
  recovery restores normal layout admission when both markers have cleared.
  Native transaction completion and timeout algorithms were not changed in
  this phase.

### Coverage and Verification

Nine regressions in `src/tests/native_move.rs` cover direct and indirect
mutations, script cohorts and frame requests, independently writable columns,
crossed-column protection, a standalone recovery barrier, timeout/confirmation
release, and preservation of focus/classification. Seven run without Lua; two
exercise script-plan replay. All nine focused tests passed; log:
`/tmp/spool-main-review-20260908-layout-admission-boundaries.log`.

Final locked workspace tests passed: 970 passed, 2 ignored (853 daemon, 20 IPC,
6 Lua client, 91 shared types). Log:
`/tmp/spool-main-review-20260908-layout-admission-workspace.log`.

Locked Lua-free daemon tests passed: 714 passed, 2 ignored. Log:
`/tmp/spool-main-review-20260908-layout-admission-without-lua.log`.
Strict all-targets Clippy passed for both workspace/default and Lua-free builds;
logs use `clippy` and `clippy-without-lua` suffixes under
`/tmp/spool-main-review-20260908-layout-admission-`.
Locked workspace and Lua-free `cargo check`, formatting and diff-whitespace
checks also passed; compilation logs use `check` and `check-without-lua` suffixes
under the same prefix.

### Remaining Scope

The main-program review goal remains active. Target-width admission after
intervening destination changes, three-or-more-display selection and native
associations outside recorded tab groups remain separate review targets. The
documented native-tab-creation mismatch is still a release blocker. Mock tests
do not establish real macOS multi-display/AX acceptance. No live daemon action,
desktop manipulation, deployment, Nix work, publication, staging or commit was
performed.

## Deterministic Multi-Display Command Selection (2026-09-08 UTC)

### Findings and Changes

- **P1: Directional transfer and focus depended on the first ECS peer.** With
  three or more displays, commands could miss a valid destination or choose a
  farther display. Directional focus also checked one display before a separate
  cursor-based selector could choose another. Three red regressions reproduced
  nearest-peer selection, focused-display versus cursor-display confusion, and
  next-display cycling that skipped a third screen. Log:
  `/tmp/spool-main-review-20260908-display-navigation-red.log`.
- A shared pure selector in `src/commands/display_navigation.rs` now chooses
  targets from fresh native inventory. Explicit next-display commands cycle by
  X origin, Y origin and display identity. North/south fallback uses the focused
  display, prefers horizontal overlap and ranks directional edge distances;
  it does not wrap when no target exists. Local layout navigation remains first.
  Window movement retains the staged native-confirmation transaction.
- **P2: Pointer navigation trusted stale placement and unsafe geometry.** The
  command now requires complete topology, unique visible-Space ownership,
  matching native/ECS geometry, and a unique target strip with the correct
  display parent. An unready selected destination is rejected, not replaced by
  an arbitrary peer. Explicit mouse cycling resolves unique cursor ownership
  using half-open bounds, avoiding ambiguity at shared display edges.
- Candidate windows must be available, visible, uniquely present in the target
  native Space, and positively intersect its usable viewport. Selection has a
  stable identity tie-break. Empty eligible targets land at the usable center;
  candidate targets land inside their visible intersection. Widened distance
  arithmetic and overflow-safe midpoints handle extreme coordinate origins.
- Removed obsolete first-peer accessors from active-display system parameters.
  Architecture and configuration documentation describe the common selection
  policy and its admission boundaries.

### Coverage

Eight pure selector tests cover all input permutations, directional preference,
diagonal fallback, duplicate/missing identities, invalid geometry, stable ties,
integer endpoints, half-open ownership and positive visible intersections.
Seven command-level tests cover nearest-peer transfers, focus source identity,
three-display cycling, spatial window-transfer order, unknown/stale topology,
invalid window candidates and extreme landing coordinates. The direction-focus
test asserts the exact destination center, so a no-op cannot pass merely because
the cursor already started on the target display.

### Verification

- Final locked workspace tests passed: 986 passed, 2 ignored (869 daemon,
  20 IPC, 6 Lua client, 91 shared types). Log:
  `/tmp/spool-main-review-20260908-display-navigation-workspace-final.log`.
- Final locked Lua-free daemon tests passed: 730 passed, 2 ignored. Log:
  `/tmp/spool-main-review-20260908-display-navigation-without-lua-final.log`.
- Strict all-targets Clippy passed for default workspace and Lua-free builds.
  Locked `cargo check` passed in both configurations. Logs use the `clippy`,
  `clippy-without-lua`, `check` and `check-without-lua` suffixes under
  `/tmp/spool-main-review-20260908-display-navigation-`.
- Formatting and diff-whitespace checks passed. Final full-suite counts include
  the current checkout's separate overlay continuity regression; this phase
  adds 15 display-navigation tests and preserves unrelated changes.

### Remaining Scope

Target-width admission after intervening destination changes, native window
associations outside recorded tab groups, and gesture-specific edge-warp
selection remain review targets. Pointer-only navigation to an empty display
may also need to cancel an earlier pending follow; that interaction is a review
hypothesis, not yet a reproduced finding or a claimed fix. The documented
native-tab-creation mismatch remains a release blocker. Mock tests do not
establish real macOS multi-display/AX acceptance. No live daemon action, desktop
manipulation, deployment, Nix work, publication, staging or commit was performed.

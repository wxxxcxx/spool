# One command admission boundary for runtime actions

Status: Accepted, 2026-09-14; the single-reader routing and shared recipes are
implemented. Desktop/native acceptance is not required for a routing and
refusal-code change, and is not claimed.

## Context

`dispatch_actions` used to match each incoming `Action` by hand. Some actions
went through the checked `admission::execute` path and produced a receipt; many
others called a domain system directly (`command_move_focus`, `move_to_display`,
`command_center_window`, `toggle_floating_window`, native Space commands, and so
on). Each domain module then re-derived the same admission questions — is the
window visible, is its layout writable, does one strip own the Space, is the
column eligible — with its own duplicated closures and bare rejection strings.
Because the direct branches skipped the shared path, whether an action respected
the lifecycle and `session_not_writable` gates depended on which branch it
happened to take.

[ADR 0006](0006-declarative-state-driven-window-management.md) makes accepted
state and native realization separate; that separation needs a single place that
decides whether an action is accepted before any domain effect runs.

## Decision

**One reader for every action.** `commands::admission::execute` (the checked,
receipt-bearing path) and `commands::admission::execute_dispatched` (the
fire-and-forget bus path) are the only readers of a runtime `Action`. Both call
one ordered pipeline: evaluate the lifecycle gate, run effect-only actions
(`MissionControl`, `ShowDesktop`, `PrintState`, `ReconcileWindows`, the two
lifecycle actions, `ToggleBarCollapse`), resolve a default target, then dispatch
to the domain executor. `dispatch_actions` becomes a thin reader: it turns
`CheckedActionRequested` into an `AdmissionReceipt` and forwards every
`ActionRequested` to `execute_dispatched`.

**The `Layout(plan)` batch is a deliberate second boundary.** A script's
returned plan is not a single action; `layout_ops::apply_layout_plan` replays it
operation by operation against its captured `LayoutSnapshot`, and each operation
still takes its own admission path. Folding the replay into the top-level reader
would either lose the snapshot-bound batch semantics or re-order operations
behind the actions that followed.

**Typed rejection, unchanged wire.** `Rejection` is a bounded enum covering the
reasons the shared recipes produce. `Rejection::code` is the IPC contract: every
variant serializes to the byte-for-byte string the domain executors already
published, and `From<Rejection> for Error` keeps the existing `admission_code()`
result. Domain and native executors keep their own reason codes for the failures
the recipes do not model.

**Recipes consolidated, strategies preserved.** An `Admission` system parameter
bundles the window query, `NativeTopology`, `WindowManager`, `Config`, displays
and strips, and owns the shared recipes: resolve a target window, require it
visible and writable, find the unique strip, resolve a column, and establish
native Space membership. `targeted`, `transfer`, `layout_edit`, `column_width`
and the command helpers call these recipes instead of reconstructing the checks.

## Considered options

- **Keep the per-action readers and per-domain direct calls.** Rejected: order
  and admission keep drifting between branches, recipes stay duplicated, and
  actions that took a direct branch continue to bypass the lifecycle and
  `session_not_writable` gates.
- **Type the entire rejection vocabulary** (every bare `rejected("...")` string
  in the command and native-space tree) **instead of a bounded set.** Rejected:
  most of those strings are produced inside domain or native executors whose
  failure context the shared recipes do not have. Typing them all would force a
  single failure policy across unrelated modules and enlarge the wire contract
  for no simplification of the seam.
- **Fold plan replay into admission per operation.** Rejected: a plan is one
  coherent batch captured against a `LayoutSnapshot`; replay already applies it
  best-effort with per-operation identity and structure checks. Routing it
  through the top-level reader would break that batch boundary.
- **One entry point with a receipt flag** instead of `execute` and
  `execute_dispatched`. Rejected: a checked command defers the lifecycle effect
  until after its receipt is attempted, while a dispatched action has no receipt
  and runs the effect inline; the two are genuinely distinct, and a boolean would
  obscure that at every call site.

## Consequences

The change is a routing and refusal-code change, not a native behaviour change:
observable effects are locked by equivalence tests (dispatched `Center`,
`ToggleFloating`, `ToggleTiledVisibility`, and `Quit`), and the rejection strings
are locked by a contract test. The deliberate consequence is that actions which
previously took a direct branch now respect admission's lifecycle and
`session_not_writable` gates; an action that used to run while initialization,
Mission Control, or exit restoration was in progress may now be refused. The
`Layout(plan)` boundary keeps its snapshot semantics, and the migrated
executors now share one recipe vocabulary instead of reconstructing it.

Desktop and native acceptance are not required for this routing change and were
not run. Whether a specific native write is accepted remains the domain of the
existing platform and reconciliation work.

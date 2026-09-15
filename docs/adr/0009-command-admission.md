# One command admission boundary for runtime actions

Status: Accepted, 2026-09-14; the single-reader routing, the exhaustive session
reach, the shared recipes, and one implementation per operation are implemented.
Desktop/native acceptance is not required for a routing and refusal-code change,
and is not claimed.

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
happened to take. `Center`, `Snap` and `ToggleFloating` were the sharpest case:
the bus branch called a system that resolved its target from the active display
and checked no Space ownership, display freshness or target writability, while
the checked request took the explicit-target command and did. `Center` also moved
the pointer on one path and not on the other, ignoring `mouse_follows_focus` on
the one that did.

[ADR 0006](0006-declarative-state-driven-window-management.md) makes accepted
state and native realization separate; that separation needs a single place that
decides whether an action is accepted before any domain effect runs.

## Decision

**One reader for every action.** `commands::admission::execute` (the checked,
receipt-bearing path) and `commands::admission::admit` (the fire-and-forget bus
path) are the only readers of a runtime `Action`. Both call one ordered
pipeline: evaluate the lifecycle gate, answer a session-ending action, run
effect-only actions (`MissionControl`, `ShowDesktop`, `PrintState`,
`ReconcileWindows`, `ToggleBarCollapse`), resolve a default target, then dispatch
to the domain executor. `dispatch_actions` becomes a thin reader: it turns
`CheckedActionRequested` into an `AdmissionReceipt` and forwards every
`ActionRequested` to `admit`.

**One decision ends the session, and the intake concludes it.**
`admission::aftermath` is a pure, total function over an action: `Stop` for
`Quit`, `Restart` for `Restart`, `Nothing` otherwise. The table marks the
lifecycle `Stopping` in both cases — so every other action is refused while the
requester still waits — and then returns without performing the effect. Whoever
owns the transport performs it: a checked request answers first and calls
`conclude_from_wire`, a fire-and-forget `admit` concludes in-world as soon as
the decision is made. `command_quit_handler`, `command_restart_handler` and the
reader's own derivation are gone, so the transport no longer has to know what a
`Quit` means.

**The `Layout(plan)` batch is one admitted action, replayed op by op.** A
script's returned plan is admitted by the session's writability like any other
Layout State edit, so a plan can no longer edit an unwritable session. The
replay stays a separate boundary: `layout_ops::apply_layout_plan` applies the
batch operation by operation against its captured `LayoutSnapshot`, and each
operation still takes its own admission path. Folding the *replay* into the
top-level reader would either lose the snapshot-bound batch semantics or
re-order operations behind the actions that followed.

**Session reach is exhaustive.** `SessionReach` classifies every `Action`
variant as needing a writable session or only a running one, and the compiler
forces an answer for a new variant. The hand-written list this replaces named
only the variants it happened to remember, so a new action could bypass the
gate by omission. `Writable` is the geometry barrier: the actions whose
realization has to wait for a quiescent layout. Everything else keeps the
lighter reach, including the state edits, which are deferred rather than refused
(below).

**A plan's deferred continuation takes the same gate as its action.**
`Event::LayoutSpaceRequested` is not an `Action` — it carries a snapshot binding
no variant can hold — but `admission::session_is_ready` is one function asked by
both the ordered pipeline and the continuation, so a plan cannot reach the
native command system at a moment when the command it continues would have been
refused.

**One implementation per operation.** `Center`, `Snap` and `ToggleFloating` had
a second implementation on the keybinding path; those are gone, and the
explicit-target command is the only one. Floating classification keeps a lighter
admission — it edits Layout State but not geometry, so a native move's ownership
barrier does not refuse it, a fact
`native_move_layout_admission_preserves_focus_and_floating_classification`
already pinned — and that exception now lives inside the one implementation
instead of in a second one. Pointing is part of `Center`: the warp moved to the
one implementation, under `mouse_follows_focus`.

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
- **Fold the plan's replay into admission, one operation per reader pass.**
  Rejected: a plan is one coherent batch captured against a `LayoutSnapshot`.
  Admitting the batch while replaying it as a batch keeps both the session gate
  and the snapshot binding; splitting the replay across reader passes would lose
  the latter and re-order operations behind the actions that followed.
- **One entry point with a receipt flag** instead of `execute` and `admit`.
  Rejected: a checked command defers the lifecycle effect until after its receipt
  is attempted, while a dispatched action has no receipt and runs the effect
  inline; the two are genuinely distinct, and a boolean would obscure that at
  every call site. The pipeline itself takes no such flag: `admit` is `execute`
  plus the conclusion the requester does not own.

## Consequences

The change is a routing and refusal-code change, not a native behaviour change:
observable effects are locked by equivalence tests (dispatched `Center`,
`ToggleFloating`, `ToggleTiledVisibility`, and `Quit`), intake parity is locked
by `one_action_is_admitted_the_same_way_from_either_intake`, and the rejection
strings are locked by a contract test. The deliberate consequence is that
actions which previously took a direct branch now respect admission's lifecycle
and `session_not_writable` gates; an action that used to run while
initialization, Mission Control, or exit restoration was in progress may now be
refused, and a script's plan can no longer edit an unwritable session. `Center`
moves the pointer only under `mouse_follows_focus`, on both the default and the
explicit target. A `quit` or `restart` arriving from a keybinding, the Bar or Lua
now marks the session `Stopping` as the checked path already did, and stops the
session once rather than once per intake. A plan's deferred continuation takes
the same lifecycle gate as the action it continues instead of reaching the
native command system while the session was still starting or shutting down. The
migrated executors now share one recipe vocabulary instead of reconstructing it.

Three exceptions are deliberate and easy to lose later:

- **Activation Intent is deferred, not refused.** A focus request during Mission
  Control, initialization or exit is held and realized later, so it must not be
  gated as a Layout State mutation. A Space focus preference and the native
  Space commands keep the lighter reach for that reason: the preference is
  state-only, and an activation is held pending by `focus::reconcile_activation`
  and reported blocked when availability cannot be proven. Those commands
  revalidate their own *snapshot identity* inside the native command system
  (`LayoutSession::accepts`), which is a different question from these gates.
  `mission_control_defers_state_edits_instead_of_refusing_them` pins the
  distinction against a future promotion.
- **Floating classification keeps its lighter admission.** It edits Layout State
  but not geometry, so no Space, display or viewport has to be resolved, and a
  native move's ownership barrier does not refuse it. It is still refused by the
  session gate.
- **Session termination cannot follow its decision immediately.** A checked
  `quit`/`restart` must have its receipt delivered before the process stops, so
  the table decides and marks the lifecycle `Stopping`, and the intake that owns
  the transport performs the termination after the receipt is written. A
  fire-and-forget termination has nobody to answer and terminates as soon as it
  is admitted, once. `Event::LayoutSpaceRequested` stays the one named
  non-action: it is a script plan's deferred continuation, and its gate is the
  one its action takes.
- **A bound Lua callback is not decided here.** `Action::Lua` is read off the
  same bus by `lua::command_lua_handler`, which hands the callback to a worker
  whose handlers are user code of unbounded duration. A wire request for one is
  refused (`unsupported_operation`) rather than owed a receipt that could not
  promise the handler ran. The consequence worth remembering: a callback reached
  from a keybinding is not gated by this table.

Desktop and native acceptance are not required for this routing change and were
not run. Whether a specific native write is accepted remains the domain of the
existing platform and reconciliation work.

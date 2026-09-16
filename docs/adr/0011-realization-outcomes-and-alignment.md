# Realization outcomes: align to the display, and do not gate the edit

Status: Accepted, 2026-09-16. **Not implemented.** Today the code still refuses
some layout edits through `session_reach` / `session_is_writable` and keeps every
unrealized intent without ever reconciling state to the display. The slice that
implements this records the ARCHITECTURE.md invariant exception when it lands.

## Context

Two things are wrong at once.

First, the admission gate is dead for the edits it names. `session_reach`
classifies `Action::Window`, `TargetedWindow` and `SpaceLayout` as needing a
writable session, but `execute_action` dispatches the column-width and
layout-edit recipes *before* the one `session_is_writable` check, so exactly
those actions are accepted while Mission Control is open, during initialization
and during exit — while `Center` and a Lua layout plan are refused by the same
gate. A classification that no longer decides anything is worse than no
classification: it reads as a guarantee it does not provide.

Second, an intent that never reaches the screen stays in state forever. The
display shows one value and retained state says another, with the disagreement
parked as a `blocked` diagnostic. ADR 0006's separation of accepted state from
native realization was read as "never write back from the observation", so
nothing converged the state onto what was actually displayed.

## Decision

**An accepted edit's realization ends in one of three outcomes.**

1. **Realized**, including a constrained realization: the display meets the
   derived effective target while a constraint prevents full satisfaction of the
   original intent. Nothing to reconcile.
2. **Definitively refused**: the platform answered that this exact request is
   invalid for this window instance, or the target provably ceased to exist
   (confirmed destruction, a replaced instance, a Space or column that no longer
   exists).
3. **Not yet decidable**: unreadable observations, unsatisfied constraints,
   Mission Control open, initialization, a native move in flight, or a write that
   was issued and never confirmed by the end of its grace period.

**Outcome 2 is followed by alignment: read the displayed value, write it into the
authored intent, and the two agree again.** The evidence is what external
adoption already demands — a fresh, stable observation, not a value read while
the window is in motion — plus a version binding: alignment applies only while
the failed edit is still the newest edit of that field, so the failure of an
older request never rewrites a field a newer edit owns. Without that evidence the
outcome stays 3. Alignment corrects an authored intent field only; derived
targets are recomputed, never reconciled.

**The grace period is the existing readback sequence** (250 ms, 1 s, 5 s). If the
window has by then shown a stable value that differs from the target, that is
outcome 2 and alignment follows. An unreadable or in-motion observation *extends*
the grace period rather than ending it: unknown is not a shown value, and a
mid-animation frame must never become an intent.

**Conversion back to intent is per domain.** A floating frame and a declared
Space are adopted identically. A column width is converted back to its logical
slot width and keeps its variant: a viewport ratio stays a ratio recomputed
against the current viewport, and an inherited width becomes absolute, because
the display value is always concrete. Stack weights are renormalised from the
observed heights. Focus memory has no display counterpart and never aligns.

**Layout state edits are not gated.** Mission Control, initialization and
in-flight native transitions are handled by outcome 3, not by refusing the edit.
The one exception is the exit handover: while the session is restoring
pre-Spool frames and no later realization exists, every edit is refused.
`session_not_writable` survives only for that moment.

**The realizer's invariants.** It writes only the windows whose desired frame
differs — never a full re-layout. It animates to the target instead of snapping.
It never adopts a value read while a window is in motion. And state may be
written continuously: the realizer chases the newest target, while an older
request's failure cannot touch a field a newer edit owns.

## Consequences

- ARCHITECTURE.md's single-direction invariant ("no projection writes backward
  into an earlier layer") gains exactly one exception, on the failure path, with
  the evidence rules above. The intended invariant while the daemon manages the
  desktop is `state == display`; it is suspended at the exit handover, not
  violated, and ADR 0010 removes the persisted document that used to outlive it.
- `session_reach` loses its `Writable` reach judgement. What remains is the exit
  refusal and the lifecycle readiness check; the slice must re-derive that
  classification rather than leave a table that no longer decides anything.
- Issue 03's rule that a constraint keeps the original intent is untouched: a
  constrained realization is outcome 1, not a failure.
- The admission-blocked diagnostics that today report "blocked" forever are
  replaced by an outcome plus, where alignment happened, a bounded record of what
  was adopted and why.
- `Center`'s pointer warp is an immediate side effect rather than a realized
  target; the implementing slice decides it explicitly (execute when it can,
  otherwise skip and report) instead of leaving it implicit.

## Considered options

- **Refuse the edit when the session cannot realize it.** Rejected: it discards
  the user's action for a condition that passes in seconds, and it keeps a
  per-action policy table that must be kept in step with the dispatch order —
  which is the defect being fixed.
- **Keep the intent and never reconcile.** Rejected: retained state would then be
  a permanent claim about a screen that never showed it, which is the opposite of
  the model this project is moving to.

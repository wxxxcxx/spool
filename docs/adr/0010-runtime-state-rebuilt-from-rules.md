# Retained state is runtime-only; rules rebuild it after a restart

Status: Accepted, 2026-09-16. **Not implemented.** The persistence and recovery
machinery this replaces is still in the tree (`state.json`, `StatePersistence`,
`RestoreCandidates`, the import seam, `spool session restore`), and retiring it is
a scheduled slice. Nothing below describes behaviour the code has today.

## Context

Spool persisted a snapshot of per-window and per-Space intent to
`$XDG_STATE_HOME/spool/state.json`: column widths, stack height weights, floating
frames, declared Spaces, and per-Space focus memory. The recovery line — issues
24, 26, 28 and 29 — read that file back as isolated *candidates*. Nothing matched
windows automatically: a startup owner had to prove every mapping by hand,
because a saved window number and a saved Space number are both scoped to one
login session, and the ticket that would decide whether a window number may be
reused at all is still open.

Two consequences of that design were structural, not incidental:

- The saved document held runtime state, so it could disagree with the screen
  (a width the platform refused, a frame that was never realized), which is what
  motivated the alignment discussion recorded in ADR 0011.
- A per-window-instance record is not a preference. It names a window that will
  not exist next session, so restoring it needs a human step every time, and the
  step can bind the wrong window.

## Decision

**Retained desired state exists only while the daemon runs.** A restart rebuilds
it from rules and current native observation:

| Rebuilt | Rule | Observation | Default |
| --- | --- | --- | --- |
| Floating or tiled | `floating` | — | admission default |
| Column width | `width` (a ratio) | the admitted window's width, adopted once | config default ratio |
| Stack height weights | — | observed heights | equal |
| Floating position | `grid` | the window's own position at launch | placed in the viewport |
| Which Space a window is in | — | native membership | the visible Space |
| Which window a Space focuses | — | the window macOS currently focuses | the first eligible window |
| Column order | `index` | discovery order | discovery order |

**Cross-session preference is expressed as rules, and the rule vocabulary grows
one ticket at a time.** Where no rule expresses something, it is not preserved,
and that loss is accepted rather than papered over with a snapshot. Nothing about
the runtime state is persisted, no candidate document is read at startup, and no
import seam exists.

## Consequences

- `state.json`, `StatePersistence`, the capture/commit/version protocol,
  `RestoreCandidates`, the trusted-binding import seam, `Action::RestoreIntents`,
  `spool session restore`, and the save systems and diagnostics that read them
  are retired in a scheduled slice.
- Issue 24's save half, and issues 26, 28 and 29 in full, lose their subject.
  Issue 25's 2026-09-15 recovery adjudication is reversed by this decision; the
  tickets and the wayfinding map are updated when the slice lands.
- Issue 27 (what a native window number guarantees) no longer gates anything:
  with no cross-session identity there is no binding to authorize. It stays open
  as a platform fact for the runtime identity paths.
- Accepted losses, to be stated in user-facing documentation: a column width
  dragged at runtime, a stack height adjusted at runtime, an exact floating
  position, and per-Space focus memory do not survive a restart unless a rule
  expresses them. Arrangement structure (columns, stacks, tabs and their order)
  was never importable even before this decision; it is rebuilt from discovery
  order, which `index` can influence.
- Native observation becomes the main source for where a window starts, since
  macOS restores window positions and Spaces for many applications.

## Considered options

- **Keep the file but stop binding automatically.** Rejected: the document still
  has to be kept honest against the screen, which is the alignment cost, and the
  human mapping step — the fragile part — remains.
- **Keep persisting, but only preferences.** Rejected as a distinction without a
  mechanism: a record keyed by a window instance is not a preference, and a
  preference keyed by application is a rule, which is what this decision chooses.

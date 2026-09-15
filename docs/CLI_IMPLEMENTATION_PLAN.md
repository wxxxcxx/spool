# Resource CLI and native inspection implementation plan

Date: 2026-09-13. Status: implementation authorized by the user's explicit
`woz-implement` invocation after final plan review. Implementation and isolated
validation are complete; see the [implementation review](reviews/resource-cli-implementation-review-2026-09-13.md)
for results and the separate live-deployment boundary. The original R1–R7 design
review closure was documentation-only.

The [design](NATIVE_INSPECTION.md) records decisions reached in the interview.
This plan makes the remaining defaults and implementation choices concrete for
one final review. Items explicitly called proposed below are included in the
subsequently authorized implementation scope. Deployment, installation, and restarting the live
daemon are separate from implementing and testing this change.

The [initial design review](reviews/resource-cli-design-review-2026-09-13.md)
identified R1–R7. The revisions below address those findings as concrete
proposals; review closure means the documentation is sufficient, not that code
or live behavior has been verified.

## Outcome and architectural boundaries

Users address resources instead of choosing between top-level action/query
interfaces. Reads default to Spool's retained state; `--source native` performs
independent, read-only collection even without a functioning daemon. Native
evidence is source-attributed and can remain partial or contradictory. Commands
report admission by the daemon, not merely successful delivery or physical
completion.

Follow [ADR 0006](adr/0006-declarative-state-driven-window-management.md):
commands express state transitions and effects reconcile them with native
observations. This change establishes diagnostic and admission boundaries; it
does not implement the entire long-term declarative architecture. The first
version's visible-only geometry/arrangement mutation gate is temporary. Do not
embed visibility as an intrinsic validity rule in pure layout transforms.

## Complete command families

All examples omit the leading `spool`. Bare root/resource/subgroup commands
show help. Resource IDs are native identities; a column ordinal is explicitly
one-based and uses the complete retained layout.

| Family | Planned operations |
| --- | --- |
| `window` | `list`, `inspect <id>`, `focus <id\|direction\|step\|layer>` or `focus --nth <n>`, `move <direction>`, `center`, `grow <width\|height>`, `shrink <width\|height>`, `maximize`, `snap`, `toggle floating`, `toggle stack`, Space/display transfer, `reconcile` |
| `space` | `list`, `inspect <id>`, `focus <id>`, `create`, `delete <id>`, `layout` subgroup |
| `space layout` | `inspect`, `equalize`, `balance`, `toggle tiled-visibility`; `--space <id>` selects the owner |
| `display` | `list`, `inspect <id>`; this migration does not invent a new display-focus operation |
| `app` | `list`, `inspect <pid>`, including explicit `--source native --show windows.ax` |
| `session` | `inspect`, `watch`, `mission-control`, `show-desktop` |
| `service` | `run`, `start`, `stop`, `restart`, `quit`, `install`, `uninstall`, `reinstall`, `logs`, `dump-state`, `migrate-state` |
| `launcher` | `install`, `uninstall` |
| `bar` | `toggle-collapse` |
| `mouse` | `next-display` |
| `script` | `run <file>`, `run -e <code>`, `run -` for stdin; preserve trailing script arguments |

Single-window mutations accept `--window <id>` and otherwise use the focused
window. They resolve that target directly, not through a focus-then-act trick.
`toggle stack` remains window-anchored: it moves that tiled item into/out of an
adjacent stack, unlike equalizing an entire column. Existing incidental layout
effects, native constraints, and atomic multi-member admission still apply.

`space layout equalize --column <n>` selects a stack column;
`space layout balance --reference-column <n>` takes a width reference for the
whole Space. No omitted selector may borrow a reference from another Space.
Out-of-range explicit IDs/ordinals fail without fallback. A valid directional
navigation request at an existing bounded edge can remain an accepted no-op.

### Migration table and proposed remaining spellings

The design already confirms the resource names and most verbs. Exact spellings
for combined focus and transfer commands below are proposed for final review.

| Existing entry | New entry |
| --- | --- |
| no arguments / `launch` | `service run`; no arguments now shows help |
| `install`, `uninstall`, `reinstall`, `start`, `stop`, `restart` | same verb under `service` |
| `log` / `logs` | `service logs` |
| `install-app`, `uninstall-app` | `launcher install`, `launcher uninstall` |
| `query state`, `query active` | `session inspect`, `session inspect --show active` |
| `query spaces` | `space list`; nested window details are available through `space inspect <id>` |
| `query on-screen` | `window list --on-screen true`; preserve the Spool source's existing sliver-excluding meaning |
| `subscribe [--json] [--raw]` | `session watch [--json] [--raw]` |
| `action window focusid <id>` | `window focus <id>` |
| `action window focus <number>` | `window focus --nth <number>`; existing navigable-layout semantics |
| directional, first/last, next/previous, and layer focus | retain selectors under `window focus`; canonical layer token is `other-layer` |
| `action window focus-in-space <wid> <sid>` | proposed `window focus <wid> --space <sid>`; explicit switch-and-follow semantics, not ordinary focus |
| `action window move-to-space <wid> <sid> follow\|stay` | proposed `window move-to-space <sid> --window <wid> --follow\|--stay` |
| `action window nextdisplay`, `nextdisplaysend` | proposed `window move-to-display next --follow`, `... --stay` |
| `action space create <display-id>` | proposed `space create --display <id>` |
| `action space focus/delete <id>` | same paths without `action` |
| window move/center/grow/shrink/maximize/snap/toggle floating/stack | same verbs without `action`, plus optional `--window` |
| window balance/equalize/toggle tiled-visibility | corresponding `space layout` operations |
| mouse/bar/overview/reconcile/printstate/script/migrate-state | mappings confirmed in the design's supplementary command table |
| `action quit`, `action restart` | `service quit`, `service restart` |

Proposed transfer default: require exactly one of `--follow` or `--stay` so
focus behavior is explicit. An omitted window selector still uses the focused
window. First-version display transfer keeps the existing `next` destination;
arbitrary destination display support is not implied by reorganizing the CLI.
Do not expose internal-only `Action` variants as new public commands merely
because they exist. In particular, rule-driven `SetWidth` must not be serialized
as a different public command with different semantics.

Remove old top-level paths and their compatibility aliases in this one breaking
change. Update repository-maintained callers and current guides. Historical
research/review quotations remain historical evidence, not executable examples
to mechanically rewrite. Update shared action argv parsing/formatting and
Lua's `spool.run` vocabulary together. Keep typed Lua method names, existing
Lua query return shapes, and the single-value `spool.run` call shape unless a
specific internal adaptation is needed; do not route Lua queries through the
new CLI JSON envelope.

## Read projection and selection

Spool source: add a pure read projection over retained daemon state and existing
recorded observations. The existing `QueryStateParams::extract` can perform
native reads, so it must not simply be relabeled as a pure internal snapshot.
Record unavailable fields honestly rather than fetching AX/Space data while
answering the new Spool-source inspection. Reuse already recorded data where
possible; any additional cached observations must be owned by the normal
observation pipeline, not collected as a side effect of inspection.

Native source: enumerate source inventories independently, retaining ID-less
AX windows and WindowServer surfaces Spool would ignore. Share low-level
read-only conversions where appropriate, not daemon inventory, admission,
observer registration, or cache-mutating window wrappers. No layout refresh,
focus/raise, Space switch, attribute setter, native tab selection, or recursive
child-control traversal is part of collection.

### Proposed default views and groups

| Resource/source | Default detail | Explicit expansion |
| --- | --- | --- |
| Window / Spool | identity; desired/presented/observed geometry; retained layout membership and the last membership attempt; availability, visibility, known motion/migration/blockers | `geometry`, `layout`, `membership`, `state`, or their documented leaf paths |
| Window / native | identity; common AX role/title/geometry/state; WindowServer fields; native Space membership evidence | `ax` reads all advertised window attributes; `cg`; `spaces`; `ax.AXRole` and other literal native attribute names; proposed `actions` and `parameterized-attributes` list names only |
| Space | identity, kind, order, display/visibility relation, member window summaries from the selected source | `windows`; Spool arrangement stays under `space layout inspect` |
| Display | identity, name/UUID, recorded or native bounds/usable frame/scale, main status, visible-Space relation | documented identity/geometry/Space groups supported by that source |
| App | PID, bundle ID/name, hidden/frontmost state and known window count/inventory status | `windows` summaries; native `windows.ax` reads each top-level window's attributes |
| Session | selected-source displays, Spaces, window summaries, apps, active/focus information, capabilities and collection issues | `active`, `displays`, `spaces`, `windows`, `apps` |

Lists read summary fields only, plus filter dependencies. Defaults are explicit
sets of fields, not shorthand for all groups. `--show` replaces the default
selection; a group expands to its complete documented field set and a leaf
requests only itself plus dependencies. Selecting both a group and its leaf
does not duplicate work. Always retain minimal identity and response metadata.
An invalid group/source combination is an argument error. A valid dynamic AX
attribute that is not exposed by this object is a recorded read outcome, not
an invented value. `--show ax` never means recursive AX tree traversal.
List output has fixed summary columns in this version; `--show` is a detail
selector and is rejected on `list`. Each selected AX attribute schedules a
value read and a separately reported settability read. Group expansion and
deduplication happen before scheduling and completion classification; selecting
`ax` and `ax,ax.AXTitle` produces the same work and status when AXTitle is in
the advertised set. An attribute-name enumeration that fails leaves the group
scope incomplete even if some explicitly named attributes can still be read.

### Proposed exact filtering and unresolved candidates

First-version window flags: `--pid`, `--bundle-id`, `--space`, `--display`,
`--title`, `--on-screen true|false`, `--minimized true|false`.
Space lists additionally need `--display`, `--kind user|fullscreen`, and
`--visible true|false`; app lists need `--pid`, `--bundle-id`, `--name`, and
`--hidden true|false`; display lists need `--name` and `--main true|false`.
Unsupported resource/source combinations fail explicitly.

Repeated values of a field are OR; different fields are AND. Proposed textual
matching is case-sensitive literal substring for title/name and exact matching
for bundle IDs. Numeric identities are exact. Build the required evidence plan
before evaluating candidates. Reading order and early availability of one source
must not change the filter's meaning.

Document provenance per field: native `on-screen` uses WindowServer's reported
status, not inferred focus or an occlusion guarantee; Spool's corresponding
filter preserves its existing sliver exclusion. Native `--space` uses native
membership; `--display` uses the observed Space/display relationship, not an
invented unique owner from a window's centre point. Unresolved membership or
topology stays unknown. Spool filters use its retained projections and must not
fall back to native collection.

#### Native filter dependency table

The native adapter has fixed source roles: AX for accessibility attributes,
CG for WindowServer window records and display geometry/IDs, AppKit running-app
metadata for application fields, and native Space topology/membership endpoints
for Space relationships. Display names use one documented native display-name
adapter; they never fall back to an application's AX name or a guessed label.

| Resource and filter | Required evidence per candidate |
| --- | --- |
| Window `--pid` | Owner PID from each applicable AX/CG record and resolved association; ID alone cannot select one owner's evidence |
| Window `--bundle-id` | Unambiguous owner association plus running-app bundle ID for that PID |
| Window `--title` | AXTitle and CG window title from every applicable AX/CG record |
| Window `--minimized` | AXMinimized; CG onscreen state is not a substitute |
| Window `--on-screen` | CG reported onscreen field; absence of a CG record is not false |
| Window `--space` | Complete membership set from the declared window-to-Space and Space-to-window endpoints; reconcile these sets without assuming one membership |
| Window `--display` | The same membership dependencies, plus Space-to-display topology; evaluate the resulting set of displays |
| Space `--display`, `--kind`, `--visible` | Native topology's display relation, Space kind, or visible-Space set, respectively |
| App `--pid` | Running-app PID, or independently established window-owner PID for an owner-only app record |
| App `--bundle-id`, `--name`, `--hidden` | Running-app metadata for the resolved PID; no substitution from window titles |
| Display `--name`, `--main` | Native display-name adapter, or CG main-display identity, respectively |

Applicability is determined from source inventory outcomes and the association
rules below, never from which field happened to arrive first. AX applicability
requires the owner application's AXWindows, AXFocusedWindow and AXMainWindow
endpoint outcomes; their union may establish a match. A complete inventory
with no correlatable object can establish that this candidate has no AX record
within that inventory scope. Denied, failed, truncated, or ambiguous inventory
cannot establish non-applicability. Apply the corresponding rule to CG scope;
an ID-less AX object cannot be correlated to CG by title or geometry. Retain
each inventory endpoint's errors rather than flattening them into one empty list.

For a field shared by AX and CG, omit only a source conclusively not applicable
to this candidate. For applicable records, definitive field absence or
unsupported means this predicate is unknown, not an empty string, false boolean,
or evidence from the other source. Missing required inventory is also unknown.
For single-source fields such as minimized, no applicable source leaves the
predicate unknown. If every required applicable value is available, evaluate
each against the OR of supplied values: all true means true; all false means
false; disagreement means unknown. Different titles that both satisfy the
predicate still match. Membership/display sets use intersection with requested
IDs; different source sets that give different answers remain unknown.

Across filter fields use three-valued AND: a confirmed false field makes the
candidate a non-match even if another field is unknown. A skipped read justified
by such a final false result is `not_needed`, not budget-skipped missing work;
it cannot erase source errors already collected. Do not shortcut within the
required sources of a shared field. Summary fields are needed for retained
matched/unresolved records, not confirmed excluded candidates. Global inventory
failures still make coverage incomplete regardless of known non-matches.
Unfiltered source disagreement alone does not make a complete observation partial.

Proposed JSON list representation: retain an array in `data`; each result has
`match_status: matched|unresolved` in reserved per-record metadata. Confirmed
non-matches are excluded, unresolved candidates retain known identity/evidence
and referenced issues. Human output has separate matched/unresolved sections.
With no filters, records are matched within the declared inventory scope.

## Result and timeout contracts

Use the confirmed envelope: `schema_version`, `source`, `resource`, `status`,
`collection`, `data`, `issues`. Proposed schema version is 1 for this new CLI
envelope, separate from saved-layout and existing Lua/wire data versions.
Proposed timestamps are UTC RFC 3339; duration accounting uses a monotonic clock.
`collection` records requested scope, selected paths, start/end and duration.
Recorded observation freshness, when available, is separate from request time.

Native value entries preserve native names, value types, origin, read status,
and observed values. Attribute read failure and settability-query failure are
separate outcomes. Classify the expanded request using this outcome table,
regardless of whether a field came from a default, group, or leaf selector:

| Read outcome | Observation completeness |
| --- | --- |
| Value, including a successfully read empty string/array or false boolean | Complete for that read |
| Definitive native unsupported, absence, or non-applicability | Complete terminal outcome, explicitly retained; never invent a value |
| Permission denied, read failure, ambiguous no-value result, timeout, interrupted operation, or required work skipped by the budget | Incomplete |
| Value obtained but representation unsupported or requested content truncated | Incomplete; retain native type and available evidence |

An advertised AX attribute returning definitive unsupported follows the second
row for both `--show ax` and an explicit leaf. A missing CG key or unavailable
app property is unknown unless the adapter can establish definitive absence;
possible redaction cannot be presented as a successful absent value. A terminal
absence may complete inspection of a property while leaving a filter that
requires its value unresolved. Selection spelling never affects this distinction.
Unavailable capability for an entire requested source is incomplete, not a
blanket non-applicability shortcut. Group discovery is itself requested work.

Bound large strings, arrays, and records; any truncation is explicit and affects
completeness when requested data was lost. Preserve raw geometry and its native
units/convention alongside any labeled normalized representation.

Exit codes for reads are 0 complete, 1 failed/conclusively not found, 2 invalid
arguments, 3 usable partial result. Missing IDs caused by incomplete enumeration
are unknown, not conclusively not found. Failures after collection starts retain
a JSON envelope; command syntax errors can use normal CLI diagnostics. stderr
must never corrupt stdout JSON. `session watch --json` remains NDJSON events,
not an expanding snapshot document.

Aggregate status is computed after filtering: missing required coverage or
retained-record data yields partial when usable evidence exists, failed otherwise.
Resolved absence of an explicit detail target yields not_found only when all
required target-resolution inventories completed. Unresolved association or
incomplete enumeration cannot establish not_found. Read failures for confirmed
excluded candidates remain issues but do not by themselves make the filtered
result incomplete; global inventory gaps and unresolved retained candidates do.

### Native identity and sampling correlation

Every raw source object gets an observation-local evidence reference, including
objects with native IDs. Each source sample/read retains an operation ID, source,
start/end time (and monotonic offsets within collection), owner PID and native
window ID where available. Retain AX object identity inside the helper and
serialize only its local reference; no process pointer or daemon incarnation
is exposed as a reusable native identity.

Raw records remain separate. A convenience association lists member references,
the matching evidence, sampling range, and status `consistent|unresolved`.
`consistent` means consistent within the sampled evidence, not atomic identity
proof. To associate AX and CG records, require matching non-missing owner PID
and window ID, a unique AX object for that candidate, and no observed identity
change or conflicting record within the collection. Re-check that AX object's
PID/ID after its selected reads before finalizing an association; retain the
initial and final samples. If this validation cannot finish within the budget,
the association is unresolved. Same PID/ID cannot rule out unobserved reuse;
the output does not promise a native lifecycle token that macOS did not supply.

An observed AX identity replacement, owner mismatch, ambiguous match, or vanished
object splits candidate evidence and prevents a consistent cross-source join.
Never transfer prior fields to a replacement object or merge AX identities
merely because a native-tab transition looks plausible. Numeric-ID-only Space
membership remains source-attributed and is not assigned conclusively across
an observed window-identity conflict. Selection of an ambiguous detail ID keeps
the candidate records and issues in its detail object with partial status.

Filter shared fields only across consistent associations; ambiguous records
remain unresolved unless an independent confirmed-false predicate excludes
them. Missing association does not discard raw records. An unfiltered command
may be complete while reporting unresolved correlation if all requested source
reads completed; failed required identity validation is still an incomplete
read. Selecting only CG detail does not invent an AX dependency solely to build
an unrequested cross-source view.

Proposed defaults: `--timeout 5s` for one-shot reads and CLI admission waits;
accept positive durations with `ms` or `s`, reject zero/infinite values. Use an
AX messaging timeout no greater than 250 ms or the remaining total budget.
Native collection does not automatically prompt for permissions: collect what
is available and report permission-limited sources. Never start/restart a
daemon to satisfy an inspection.

### Proposed enforceable native collection boundary

Use a short-lived helper process running the same binary in a private internal
mode. It performs platform reads on its own main thread and streams completed
evidence records over an inherited pipe. It never takes the daemon singleton
lock, starts Bevy or observers, or opens UI. The CLI supervisor applies the total
deadline, retains complete records, terminates only its helper if necessary,
and reports unfinished reads. OS scheduling and bounded serialization/cleanup
add overhead; do not advertise hard real-time response guarantees.

This costs process startup, but allows a blocked FFI read to be stopped without
blocking the daemon or throwing away earlier evidence. Per-call AX timeouts
remain useful but are not the only deadline mechanism. Plain owned data crosses
the process boundary; no AX references or ECS borrows do. Cap protocol record
sizes and total buffered output, reject malformed/truncated records explicitly,
and reap the child on completion, timeout, or interruption. The private worker
mode is not a second public command family.

#### Incremental helper protocol

Use bounded, length-delimited frames with a helper-protocol version, capture ID
and monotonically increasing sequence. The helper is single-threaded for native
reads, so at most one leaf platform operation is in flight. The supervisor owns
the plan and progress ledger; messages carry plain data only:

| Frame | Required information / effect |
| --- | --- |
| `WorkDeclared` | Operation ID, target/source, dependencies and requested scope; includes inventory/discovery operations |
| `OperationStarted` | Operation ID and start time; write the complete frame before entering the native call |
| `EvidenceResult` | Operation ID and one completed field outcome; values and settability use separate leaf operations |
| `InventoryChunk` | Discovery operation ID and newly discovered source objects or attribute names, with local references; declare dependent work as soon as known |
| `OperationFinished` | Operation ID, end time, terminal outcome, and whether inventory enumeration completed |
| `CollectionFinished` | Terminal scope/coverage and completion marker, emitted after all required operations are accounted for; the marker may describe a partial collection |

Normal order is declaration, start, results/chunks, finish. A declared operation
proven unnecessary by final false filtering may instead finish as `not_needed`
without a start; this never asserts that a native read occurred. Discovery
parents may remain open while declared leaf reads run, but only one platform
call is active. Reuse of an operation ID for a new declaration is invalid;
multiple lifecycle frames referring to the same declared ID are expected.

Never buffer an entire window's attributes before publishing successful fields.
Inventory-producing calls may return a whole native array; once returned,
publish bounded chunks before scheduling attribute reads. If enumeration or
chunk transfer is interrupted, retain received objects and an unknown remainder;
do not invent names/counts for work that was never discovered. A complete
EvidenceResult retains its outcome even if the following finish frame is lost;
the missing finish still leaves operation bookkeeping incomplete.

On deadline, terminate the helper, drain only bounded complete frames already
available, then reap it. Started operations without a result are timed-out;
declared required operations without a start are budget-skipped. A native call
may not actually have begun after its start frame was sent: the status describes
the observed operation attempt, not proof of where the OS was blocked. A crash
or user interruption uses interrupted rather than pretending an AX timeout.
Undeclared dynamic work remains an unknown discovery remainder. Missing final
marker, malformed frames, truncated frames, duplicate declarations or invalid transitions
are explicit protocol/incomplete-collection issues, never successful completion.
Received EvidenceResult frames survive these failures. If output limits require
dropping content, preserve an explicit truncation issue and incomplete status.

Set AX timeouts for the actual object being called (or a verified process-wide
setting); do not assume an application element's timeout propagates to windows.
The supervisor deadline remains the final bound for non-AX and blocked calls.

## Accepted action semantics and IPC

`RequestReader` currently acknowledges enqueueing; move checked CLI admission
to the daemon's ordered command path. Resolve current identities/incarnations
and complete affected-member sets there, validate first, and only then publish
the state transition/effect intent and its admission receipt. This is the
running-state path; startup dispatch is specified below. A valid bounded
navigation no-op may be accepted; an invalid explicit target is rejected.
Do not reroute targets through focus or leave partial group mutations on failure.

Proposed receipt includes a request ID, `accepted|rejected`, and a machine-readable
reason plus human message on rejection. It acknowledges admitted intent, not
native convergence. CLI exit 0 requires that receipt; rejection or missing
receipt returns nonzero. A timeout means acceptance is unknown, not cancelled;
do not retry automatically. Ensure quit/restart admission replies are delivered
before shutdown can sever the transport where feasible; otherwise preserve the
unknown-outcome distinction.

### Startup and shutdown request handling

The listener currently starts before Accessibility permission and Bevy setup.
Maintain explicit lifecycle state `starting|waiting_for_permission|running|stopping`
at the request admission boundary. Negotiate transport versions independently
of lifecycle; readiness does not require a World just to answer a request.

| State | Process-level `service quit` | ECS-dependent controls, Spool reads and watch |
| --- | --- | --- |
| starting / waiting_for_permission | Admit quit and attempt its receipt before exiting, without waiting for Bevy | Reject controls with `not_ready`; reads return failed envelopes with `not_ready`, watch returns an explicit subscription error |
| running | Admit via ordered process shutdown and deliver receipt before closing where feasible | Use normal ordered execution / pure projections |
| stopping | Reject new requests with `shutting_down` | Return `shutting_down` while transport is open |

Process state transitions and request classification must be serialized; requests
rejected as not_ready are never retained for later execution. The daemon's
lifecycle controller owns the reply path until delivery or deadline; publishing shutdown must not
drop it before an attempted receipt. A lost reply remains unknown to the client,
even if the daemon accepted quit. Connection refusal/closure after shutdown is
a transport failure, not a fabricated typed rejection. Existing typed Lua wire
queries use their existing error shape, not the CLI envelope; unknown startup
requests must not disappear in the permission-wait event loop.

CLI `service start|stop|restart|install|uninstall|reinstall|logs` are local
service-management operations and do not require daemon ECS admission. Their
success describes the requested service-management operation, not desktop
convergence. Lua `spool.run("service restart")` retains the existing daemon
restart action and spawning of the migrated `service restart` helper; it is
rejected as not_ready before the action executor exists. Sharing the spelling
does not export local installation or service management into Lua.

### Version negotiation and v5 compatibility rejection

Extend typed shared requests/responses, but retain legacy query message types
needed by unchanged typed Lua read APIs without retaining old CLI entry points.
Use wire version 6 for the new envelope and preserve the outer length framing.
Version rejection must precede decoding action/inspection payloads.

The first exchange on each new connection is a frozen v5-encodable bootstrap:
the v5 ClientFrame shape, `version = 6`, `mode = Call`, and the v5 encoding of
`Query(State)`. This is a transport sentinel, never an actual state query. A v5
daemon can decode it and returns its existing ServerFrame::Error for mismatched
version before dispatch; no native read or ECS event occurs. A v6 daemon reads
the leading version before version-specific payload decoding, recognizes the
exact sentinel, sends the frozen ServerFrame::Ack and keeps the same authenticated
connection for one version-6 request (or a subscription). This Ack certifies
protocol compatibility only and must not be used as action admission.

On a v6 listener, any non-6 frame gets the frozen error encoding without decoding
its request. A version-6 connection that omits or corrupts the bootstrap is
rejected before dispatch. After a successful bootstrap, use a version-6 frame
with a separately readable version header and its typed payload; subsequent
future-version rejection also happens before payload decode. Never reconnect
between bootstrap and the request: that would allow endpoint replacement to
invalidate negotiation. Handshake and execution share the one command deadline.

Freeze bootstrap encodings as transport fixtures independent of future Request
enum edits. New clients surface the old server's mismatch message without
parsing arbitrary text as a successful negotiation. Failure to receive a valid
Ack is transport/protocol failure and sends no action. Distinguish this
pre-dispatch failure (not submitted) from a missing admission receipt after the
request may have been sent (unknown acceptance). Do not automatically upgrade,
restart, downgrade, or retry an action. Keep Lua worker round trips nonblocking
for the main thread and preserve typed Lua return shapes.

### Startup artifact migration and ownership

Migrate source generators and deployed artifacts as separate work items. The
following are release requirements; the commands below are upgrade instructions
to implement/document, not authorization to operate this desktop now.

| Entry / owner | New artifact | Explicit existing-installation path |
| --- | --- | --- |
| Rust service generator, `src/platform/service.rs` | Program plus argv containing executable, `service`, `run` | For a Spool-managed installation: `spool service stop`, `spool service reinstall`, then `spool service start`; reinstall itself is not a promise of starting |
| nix-darwin, `nix/darwin.nix` | Corresponding ProgramArguments in the declarative launch agent | Update the Spool module/package together and apply the user's normal nix-darwin activation; do not repair with the Rust installer |
| Home Manager, `nix/home.nix` | Corresponding ProgramArguments in the declarative launch agent | Update module/package and use the user's normal Home Manager activation |
| Graphical launcher, `src/platform/app_launcher.rs` | Quoted executable followed by `service start` | `spool launcher install` replaces a verified Spool-owned bundle; foreign bundles remain rejected |
| Daemon restart helper and repository scripts | `service restart` and the resource command paths | Rebuild/update callers together; release notes enumerate externally maintained caller changes |

Before local `service install|start|restart` reports success or changes launchd,
validate existing registration arguments. A known old no-argument registration
returns `migration_required` with its path and owner-appropriate instructions;
install must not silently skip it as already installed. Compatible custom args
may remain if they explicitly launch `service run`. Malformed or ambiguous
registration is a configuration error; do not guess or overwrite it. `stop`
and diagnostic logs remain available for recovery without grammar validation.

Managed paths/symlinks and ownership uncertainty must prevent Rust
install/reinstall/uninstall from overwriting or deleting a Nix-managed or
unrecognized registration. Only a verified Spool-owned, locally managed target
is eligible for replacement. Preserve unrelated registrations and the launcher
bundle ownership check. A failed stop/unload must abort replacement rather than
delete a still-loaded registration and report success. Document that a currently
running old daemon remains old until an explicitly requested restart/activation;
changing disk artifacts alone is not protocol compatibility. A confirmed
already-unloaded service is a successful stop state, including the second stop
inside `reinstall`; permission/transport errors are not evidence of absence.

Ownership classification is explicit and separate from launch-argument validity:

- New Rust-managed installations write a non-launchd sidecar
  `<plist-path>.spool-owner.json` with schema version 1, manager `spool-cli`,
  absolute registration path and SHA-256 of the actual plist bytes. Validate
  regular-file/current-user ownership and the binding before using it. Missing,
  mismatched or malformed metadata is not proof of ownership. A partial write
  must not be reported as a completed installation; replacement rechecks the
  classified files and leaves a recoverable prior artifact if it fails.
- Recognize unmarked legacy registrations only at the expected per-user path,
  as a current-user-owned regular file, with no managed/symlink indication, and
  with a complete typed match to a frozen supported Rust v5 generator fixture.
  Match its key set, label, no-argument Program shape, KeepAlive/process settings,
  log fields and environment structure; allow only the generator's documented
  variable path/string values. Extra fields or custom structure are unknown.
  Label, filename, executable basename, or an executable located in the Nix
  store alone cannot establish the registration's manager. Keep both current
  Nix-generated fixtures in the negative ownership tests. This is recognition
  of a supported legacy format, not a claim of cryptographic provenance.
- Nix/other-manager evidence takes precedence over a legacy format match.
  Return manager-specific activation instructions and leave its files intact.
  Stale sidecar metadata must not authorize replacing a subsequently managed file.
- For unknown/custom registrations, report `ownership_unknown`, the exact path,
  and recovery instructions: review and back up the registration, then either
  migrate it through its known manager or explicitly retire it using that
  manager/manual administration. Only after the path is intentionally vacated
  does `service install` create a new Rust-owned registration. No force/adopt
  option or automatic deletion is part of this migration. The generic
  stop/reinstall/start sequence is only for recognized local registrations.

An old launcher cannot show the new migration diagnostic because its old command
is rejected; release notes must explicitly instruct users to run `launcher install`
with the new binary. No compatibility alias, auto-rewrite, or native inspection
side effect is introduced to mask this breaking migration.

## Implementation sequence and files

1. **Contracts and pure logic:** shared request/response, selection, filtering,
   outcome, and receipt models in `crates/shared_types`; resource CLI parsing
   behind a focused CLI module instead of further expanding `src/main.rs`.
   Keep plain protocol data separate from platform resources.
2. **Internal read projection and admission:** focused modules under `src/ecs`
   and `src/commands`, integrated through `src/events.rs`, `src/reader.rs`, and
   `src/commands/query.rs`; resolve window/Space/column targets once in the
   command execution step and separate pure layout edits from effect gates.
   Include pre-Bevy lifecycle responses in `src/main.rs` and the request reader.
   Implement frozen bootstrap/version negotiation in `crates/local_ipc` before
   sending new typed requests; do not infer compatibility from a running socket.
3. **Native collection:** a dedicated inspection module and read-only platform
   adapter, plus bounded helper supervision. Reuse low-level code in
   `src/util.rs`, `src/manager/display.rs`, and `src/manager/skylight.rs` only
   after checking read behavior; avoid `WindowOS::update_frame` and application
   inventory paths that mutate caches, register observers, or apply admission.
4. **CLI presentation and Lua grammar:** shared envelope rendering, summaries,
   selectors, errors, exit codes; update `src/client.rs`, shared argv
   parsing/formatting, Lua `spool.run` conversions, and command mapping tests.
5. **Migration and verification:** update generated service arguments to
   `service run`, launcher scripts to `service start`, restart helpers to
   `service restart`, release scripts, current README/guides/examples, and tests.
   Include `nix/darwin.nix`, `nix/home.nix`, `nix/checks.nix`, registration
   validation/ownership, old artifact fixtures, and user upgrade instructions
   from the migration matrix above. Remove replaced public parsers/aliases;
   preserve unrelated working changes.

The phases are one coordinated source migration; do not publish an intermediate
CLI with conflicting grammars. Existing native-tab, focus, tiling, migration,
and restore behavior remains protected by its tests and native admission rules.

## Verification and acceptance

Meaningful automated coverage:

- Root/resource help; full command migration; numeric ID versus `--nth`;
  CLI/Lua argv agreement; invalid selector and unsupported-source errors.
- Spool inspection performs no native reads; native collection performs no
  daemon connection or mutations. Fresh native collections do not share a
  stale daemon inventory, including ID-less AX and CG-only records.
- Group/leaf selection controls actual reads; no implicit recursive AX tree;
  JSON formatting does not add collection work. Native errors and conflicting
  evidence survive serialization with field/source attribution.
- AND/OR/unknown filter logic, retained unresolved candidates, successful empty
  versus failed enumeration, requested versus unrequested missing fields, and
  all four read exit codes.
- Helper timeout after some records, a stuck read, malformed output, premature
  exit, interruption, and child cleanup using controlled test workers; do not
  depend on deliberately hanging a user's application.
- Desired/presented/observed separation; retained column numbering despite
  hidden members; explicit secondary-display target; no implicit focus/Space
  switch; rejection during unresolved topology/native migration; atomic
  multi-member validation and no fallback to focused targets.
- Admission receipt is not merely an enqueue acknowledgement and does not wait
  for animation; rejected operations do not return success; protocol mismatch
  is explicit; Lua worker round trips remain nonblocking for the main thread.
- Service, launcher, and restart argument generation uses only new paths.

Review-specific acceptance gates (all are isolated checks, not live deployment):

| Finding | Required regression / acceptance evidence |
| --- | --- |
| R1 | Old/current/malformed plist and old launcher fixtures; migration_required instead of silent success; legacy-template versus Nix/custom classification; sidecar binding and stale metadata; owner/symlink protection; already-unloaded succeeds, failed unload prevents replacement; both Nix module evaluations assert executable + `service run` argv; launcher reinstall outputs `service start` |
| R2 | Same expanded group/leaf set produces identical reads and completion for unsupported, absence, timeout and truncation; failed group discovery cannot become complete |
| R3 | CG/AX completion order cannot change match status; title conflicts, unavailable inventory, single-source absence, multiple Space/display memberships and AND short-circuit coverage are explicit; `--show` on list is invalid |
| R4 | Five completed attributes followed by a blocked sixth survive; start without result, missing finish/final marker, half frame, incomplete inventory, output cap and child cleanup retain correct partial evidence |
| R5 | Frozen actual-v5 codec/peer accepts the bootstrap only far enough to reject version; zero dispatch/native reads; old CLI with changed/unknown payload receives version rejection from v6; no new action before bootstrap Ack; same-connection negotiation; shared deadline and pre/post-submit failure distinction |
| R6 | Permission-wait quit attempts receipt and exits; controls/reads/watch reject not_ready; no deferred execution after readiness; stopping responses and lost-receipt distinction; typed Lua errors retain their shapes |
| R7 | Same ID with different owner PID or AX identity stays separated; disappearance or timeout during revalidation preserves partial evidence; CG-only selection stays independent; association ambiguity cannot become a conclusive missing detail target |

The reviewed plans/specification and these acceptance gates form the approval
scope. Automated tests should assert outcomes and observed operations, not copy
the production algorithm into expected-value helpers.

Run `cargo fmt --check`, `cargo check`, strict Clippy, relevant integration tests,
and the repository's default/no-default-features and Lua module validation
matrix as appropriate to the touched code. Inspect `scripts/verify-release.sh`
before running its broader release workflow; it executes a client script and
is not a substitute for isolated tests. Real native-source reads may be checked
without changing desktop state after implementation. Newly built daemon
mutation acceptance needs a separately authorized live daemon run/restart;
mock-ECS success must not be reported as live multi-display acceptance.

## Final approval checkpoint

Approval received through `woz-implement` on 2026-09-13. Review baseline is
`685f0856f79856a94fcd91687538bfa63cc13e8e`; commit the completed, validated work
to the current branch. The following records the approved boundary.

The implementation approval covers the following concrete deliverables:

1. Resource CLI and shared Lua action grammar migrated in one breaking change,
   with the command families, explicit target semantics and proposed spellings
   in this plan; no compatibility aliases.
2. Pure retained Spool projections and independent native list/detail collection,
   selected reads, fixed-source filtering, source evidence and identity handling,
   common read envelopes and documented exit codes.
3. Bounded helper execution with incremental outcomes, and daemon checked
   admission with startup/shutdown responses and same-connection version negotiation.
4. All source launch generators/callers, ownership-aware installation handling,
   both Nix modules, upgrade instructions and the R1–R7 regression matrix.

Validation includes formatting, compilation, strict Clippy, relevant isolated
tests and Nix module evaluation as listed above. Updating source generators is
included; applying their output to the user's installed service, launcher or
Nix configuration is not. A live rebuilt-daemon acceptance run requires separate
authorization. The fully declarative invisible-Space write architecture remains
the agreed direction, with this release retaining its stated temporary gate.

Approve the agreed design plus the proposed defaults, remaining transfer/focus
spellings, result details, and implementation sequence in this plan before any
runtime code or implementation-test edit. At this checkpoint only documentation,
agent guidance, and the explicitly requested architectural memory note have
changed. No build, deployment, installation, or daemon restart has been performed
for this design task.

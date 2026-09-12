# Resource CLI and native inspection design

Status: agreed design and [implementation plan](CLI_IMPLEMENTATION_PLAN.md), revised against the [document review](reviews/resource-cli-design-review-2026-09-13.md). The user subsequently authorized implementation through `woz-implement`; implementation is in progress.

Implementation gate (2026-09-13): the user requested a complete, reviewed plan before runtime or implementation-test edits. That gate was satisfied by the final plan review and explicit `woz-implement` invocation. Live installation, deployment and service restart remain outside the authorized implementation scope.

## Confirmed scope

- Collect native observations in the CLI independently of the daemon, tracked inventory, caches, and admission rules.
- Enumerate displays, native Spaces, and windows across the current GUI session to the extent each source permits; default coverage includes offscreen and minimized candidates.
- Retain source-specific evidence, read failures, disagreements, and unmatched records. A correlated view must not overwrite source results.
- Native reads do not include daemon state, paired capture, or automatic comparison. Users request Spool state separately through the resource commands.
- Collection is read-only and does not refresh daemon state, focus windows, switch Spaces, or register window tracking.
- Resource commands distinguish filtered summary lists from single-object detail. Detail selection controls which native reads are performed, rather than merely hiding already-collected fields.
- Window detail supports attributes beyond the fixed summary fields.
- Migrate once to resource commands, removing old `action` and `query` CLI entry points without compatibility aliases. Update repository-owned CLI callers, documentation, examples, and tests as part of implementation.
- Migrate the action grammar of Lua's `spool.run` alongside the CLI control vocabulary. Typed Lua APIs and Lua query APIs remain separate interfaces.
- Numeric positional focus targets are native window IDs. Layout ordinal focus uses explicit, one-based `--nth`; IDs, ordinals, and direction/step selectors are mutually exclusive, and a missing ID never falls back to an ordinal.
- Individual-window mutations default to the focused window and accept `--window <id>` for explicit targeting. Explicit targeting must operate directly on the resolved window rather than focusing it first. Collective layout operations require separately defined scope.
- Expose Spool-owned arrangement operations under the `space layout` subgroup, including `inspect`, `balance`, `equalize`, and `toggle tiled-visibility`. Each layout belongs to a Native Space; there is no top-level `layout` resource. Layout has no independent native equivalent and does not support `--source native`.
- Use the singleton `session` resource for aggregate Desktop Session inspection and existing daemon subscriptions. `session inspect` supports one selected source; `session watch` retains daemon-backed `--json` and `--raw` subscription modes.
- Group foreground execution, service lifecycle, registration, logs, and process quit under `service`. Retain graphical launcher installation as the separate `launcher` resource; `app` refers to desktop applications.
- Bare `spool` and bare resource groups show help. Foreground daemon startup requires explicit `service run`; migrate generated launchd arguments and callers that rely on the previous no-argument startup.
- Bound native collection by a configurable `--timeout` budget, retain collected evidence on expiry, and explicitly distinguish successful empty results, failed reads, timed-out reads, and reads not started before the budget expired.
- Inspection exit codes distinguish complete results (`0`), failed requests or conclusively missing detail targets (`1`), invalid arguments (`2`), and usable partial results (`3`). Partial JSON output retains available evidence and issues.
- First-version lists use explicit filter flags rather than an expression language. Different filter fields combine with AND; multiple values of one field combine with OR; titles use literal substring matching. Unsupported filters fail explicitly, and unreadable filter evidence is reported rather than silently excluded as a confirmed non-match.
- First-version native window attribute inspection reads the window element itself and preserves element references without recursively inspecting child controls. `--show ax` does not imply an AX tree traversal.
- Layout column operations select one-based `--column` ordinals; whole-layout operations use `--reference-column` for a width reference. Use `--space` to select the owning Space. These replace the proposed window-ID anchors for layout operations; individual-window commands retain `--window`.
- Column ordinals follow the complete retained layout, including hidden or unavailable retained columns. Layout inspection and command selection use the same ordinals; filtering or temporary unavailability does not renumber columns, while actual structural changes can.
- First-version geometry and arrangement mutations are limited to confirmed visible user Spaces, including visible secondary displays. This is temporary implementation scope: the accepted long-term architecture is fully declarative and state-driven, separating valid desired-state edits from immediate native effect execution (ADR 0006).
- Default Spool window detail exposes desired, presented, and observed geometry separately, with known animation, migration, and blocker information where available. It does not disguise retained observations as fresh independent native reads.

## Proposed interface and supplementary coverage

The CLI is being replanned around singular resource nouns, grouping control, state queries, and native inspection under `spool <resource> <operation>`, with `space layout` as a subgroup for the Space-owned arrangement. Examples below are design syntax, not implemented commands; unsettled options are identified separately. `list` returns filtered summaries; `inspect <id>` returns one object's detail. A filter matching one object still returns a list. Resources include `window`, `display`, `space`, `app`, aggregate `session`, `service`, and `launcher`.

Confirmed read-source selection: both `list` and `inspect` accept `--source spool|native`, defaulting to `spool`. This keeps list/detail as the distinction in output scope and source as the distinction in provenance. Native reads remain independent of the daemon; control commands continue through the daemon and do not acquire a native-source bypass. Neither source silently falls back to the other when unavailable.

```sh
spool window list --source native
spool window list --source native --pid 123
spool window inspect 456 --source native
spool window inspect 456 --source native --show ax,cg,spaces
spool window inspect 456 --source native --show ax.AXRole,ax.AXSubrole,ax.AXPosition
spool window inspect 456 --source native --show ax --json
spool window focus 456
spool display inspect 1 --source native
spool space inspect 42 --source native
spool app inspect 123 --source native
```

Use the single `--show` selector for information groups and dotted field paths; there is no separate attribute-selection option. Select only the reads needed for identity resolution, filters, and requested output. Bulk lists must not trigger full AX attribute or child-tree enumeration. `--json` changes serialization only, preserving the collected evidence and errors without silently increasing collection scope. Unrequested fields are not collected; failed requested reads remain explicit. Proposed defaults and the fixed-source filtering rules are in the implementation plan; unresolved candidates follow the contract below.

| Subject | Evidence to collect where available |
| --- | --- |
| Displays | Native ID, UUID, name, bounds, usable frame, scale, main-display status, and separately sourced active-display evidence. |
| Spaces | Native ID, native ordering, kind, display relationship, visible Space per display, and per-source window membership lists. Preserve multiple memberships and unresolved relationships. |
| Windows | Native ID when available, owner PID, title, AX role/subrole, AX geometry, WindowServer bounds, minimized/fullscreen flags, source-specific onscreen status, layer, and available parent/association evidence. |
| Applications | Running GUI application PID, bundle ID, name, hidden/frontmost status, and AX window-enumeration outcome. Retain owners discovered through window sources even if not in the GUI application inventory. |
| Focus | Frontmost application, system AX focused application/window, application key/main window where exposed, and cursor position. Preserve disagreements; do not infer focus from window order. |
| Collection metadata | Schema version, collection start/end, source or operation timing, source scope, permission/capability results, and explicit failed, unsupported, timed-out, or skipped reads. Successful empty results remain distinct. |

Preserve the native coordinate convention and units with geometry evidence. Any normalized geometry is additional derived data with an explicit coordinate convention; it must not erase original measurements. WindowServer front-to-back ordering applies only to the sampled presented-window list, not to a global ordering of offscreen windows.

No source is assumed to enumerate every native object. All raw source records, including those with native window IDs, retain capture-local evidence identities and remain visible even when association is impossible. Association must not use title or geometry alone as proof of identity. The proposed correlation contract retains owner/ID/AX identity evidence and source sampling ranges, separates conflicts or observed identity changes, and labels a consistent association as sampled evidence rather than proof against unobserved ID reuse. See [native identity and sampling correlation](CLI_IMPLEMENTATION_PLAN.md#native-identity-and-sampling-correlation).

## Detailed contracts

### Window attribute inspection

Enumerate the attribute names advertised by each AX window and attempt to read their values, preserving native names, value types, individual read outcomes, and separately reported settability. Retain all keys returned in the WindowServer window-information record under their own source. A fixed normalized summary is additional convenience data, not the limit of inspectable properties; this does not promise access to unexposed application internals.

List advertised parameterized AX attributes and actions as metadata. Do not invent parameters or execute actions during inspection. Unsupported value types must remain identifiable instead of being silently omitted.

Confirmed first-version depth boundary: when attributes are requested, inspect the window element's own attributes and serialize referenced AX elements as capture-local references. Preserve reference-valued properties such as children and parents without recursively walking the interface tree. Recursive child-tree inspection is outside the first version; it is not implied by `--show ax`. Collection must record any truncation of reference arrays or unsupported value representations. Detailed attribute reads are on-demand single-object detail operations, not default bulk enumeration work.

### Confirmed aggregate desktop interface

Use `session` for the current Desktop Session's aggregate observation: `session inspect` replaces `query state`, and `session inspect --show active` replaces `query active`. Both support the selected read source, defaulting to Spool; `session inspect --source native` collects native evidence only and does not pair it with a daemon query. This is a singleton current-session resource, not an interface for selecting another user's session or a persisted daemon session.

Move the existing subscription to `session watch`, retaining `--json` and `--raw`. First-version watch remains daemon-backed and does not add a native event listener or polling collector.

### Confirmed lifecycle resources

Group process/service lifecycle under `service`: `run` for the existing foreground launch, `start`/`stop`/`restart` for service management, `install`/`uninstall`/`reinstall` for service registration, and `logs` for captured logs. Retain `quit` as an explicit request to the running process; it is not equivalent to stopping the launchd service. Group Spool's app-launcher installation separately under `launcher install|uninstall`, keeping the `app` resource for applications observed on the desktop. Retain the launcher feature and its separate resource rather than moving it under `service` or deleting it.

Existing daemon restart requests spawn a CLI `restart` process, so that internal invocation must migrate along with launchd arguments, app-launcher invocations, and repository scripts. Source generators and previously installed artifacts have separate migration paths, including both Nix modules. Proposed ownership checks and explicit upgrade instructions are in the [startup artifact migration matrix](CLI_IMPLEMENTATION_PLAN.md#startup-artifact-migration-and-ownership). CLI regrouping does not authorize changing the installed service or restarting the live daemon during design work.

### Confirmed bounded collection

Native collection has a finite total budget configurable with `--timeout`. Preserve completed evidence when an application stalls or the budget expires; report failures, timeouts, and budget-skipped work in the response. Optional detail not selected by the caller is outside the requested scope, not missing work. The implementation plan proposes a supervised helper, a five-second default and per-call AX bounds. Its [incremental protocol](CLI_IMPLEMENTATION_PLAN.md#incremental-helper-protocol) publishes completed fields, discovery chunks and operation states; an incomplete inventory retains an unknown remainder instead of fabricated unstarted work.

### Confirmed completion and exit-code contract

Completion is relative to the requested source, scope, filters, and detail selection; it does not claim an atomic or exhaustive view of every native object. Requested reads that cannot establish the result must remain visible even when other reads succeed. A successful empty enumeration is distinct from an unavailable enumeration. An unresolved final filter result must not silently establish a complete empty result; a confirmed false field may still conclusively exclude a candidate under AND, without hiding collected issues or global coverage gaps.

The proposed [read-outcome table](CLI_IMPLEMENTATION_PLAN.md#result-and-timeout-contracts) classifies the expanded requested field set identically for defaults, groups and leaves. Definitive unsupported/absent values are terminal inspection outcomes; permission failures, ambiguous missing values, timeouts and representation loss are incomplete. An absent value may complete its inspection while leaving a filter that requires that value unresolved. No selector spelling changes this rule.

| Exit code | Meaning |
| --- | --- |
| `0` | Completed the requested observation within its declared source coverage, including a successful empty list. |
| `1` | Request failed without a usable result, or an explicitly requested object was conclusively not found. |
| `2` | Invalid command syntax or arguments. |
| `3` | Partial result: usable evidence was obtained but requested collection was incomplete. |

When collection executes, JSON stdout retains a structured completion status, available evidence, and issues even for incomplete or failed reads. Human-readable diagnostics go to stderr without corrupting JSON stdout. Avoiding automatic whole-collection retries is proposed. Failures on conclusively excluded candidates remain issues; missing global inventory and unresolved retained candidates still affect requested coverage as specified by the implementation plan.

### Confirmed JSON response envelope

Use one common outer envelope for resource `list` and `inspect` responses from either selected source: `schema_version`, `source`, `resource`, `status`, `collection`, `data`, and `issues`. `status` distinguishes `complete`, `partial`, `failed`, and `not_found`; `collection` includes start/end timestamps and the requested scope/detail selection. Timing describes collection, not the freshness of every retained daemon observation.

`data` is an array for lists and an object for successful detail reads; a detail target without a usable result has null data and an explicit failure/not-found status. A failed list must not be mistaken for a successful empty list. Source-specific payloads remain distinct inside the common envelope: preserve AX and WindowServer evidence rather than forcing them into a single authoritative record. `issues` identify the affected source, target, and field or operation where known, retaining failure, timeout, skipped, and unresolved-filter information. The implementation plan proposes per-record `match_status`, operation evidence references and separate association records. An ambiguous detail ID retains its candidates in the detail object and reports partial, not not_found.

This envelope is for one-shot reads, not a proposal to wrap every subscription notification as a snapshot. Existing watch JSON remains a stream of events unless separately redesigned.

### Confirmed Spool window detail

Expose `desired`, `presented`, and `observed` geometry separately in default `window inspect <id> --source spool` detail and in an explicit geometry selection. The current frame pipeline already distinguishes these concepts; the read model should preserve them instead of choosing one field named actual/current frame. The observed value is Spool's retained successful readback, not an independent native collection performed by this command. Missing components or observation times remain unavailable, not guessed.

Alongside identity, layout placement, and visibility, expose known animation, native migration, availability, and geometry-commit/reconciliation state when available. A diagnostic projection may explain blockers only when supported by recorded state; differing geometry alone must not be labeled an error or proof of pending convergence. This adds read-model visibility into the existing pipeline, not implementation of the complete ADR 0006 target architecture.

Native detail retains source-specific AX/WindowServer values and does not fabricate desired or presented state. Source-specific payloads may differ while the outer envelope stays common.

### Confirmed detail selection model

Use `--show <group-or-path,...>` to replace the default detail selection. Selecting a group reads the complete group within the collection budget; selecting a dotted field path reads that field and necessary dependencies. Keep minimal target identity and the response envelope with every selection. Without `--show`, use the documented common-field defaults, not every available group. `--json` changes serialization only. The reviewed proposal reserves `--show` for detail, rejects it on list, and deduplicates groups/leaves before scheduling. AX attribute value and settability outcomes remain separate under the same selection.

For Spool window detail, groups include `geometry`, `layout`, and `state`. For native window detail, groups include `ax` (all advertised attributes on the window element), `cg` (returned WindowServer fields), and `spaces` (native membership evidence). A path such as `ax.AXRole` requests that specific attribute. The full group vocabulary and defaults must be documented per resource/source. Unavailable source-specific groups fail as invalid arguments rather than silently changing source or substituting cached native data.

Default native detail reads common identity, AX/CG geometry and state, and Space relationships, but does not enumerate every AX attribute. `--show ax` enumerates the window's advertised attributes within the collection budget and never traverses child-element attributes.

### Confirmed unresolved filter candidates

Evaluate candidates as confirmed matches, confirmed non-matches, or unresolved. A failed required read or contradictory evidence that prevents a definite filter answer must not be silently treated as either matching or absent. Keep unresolved candidates visibly separate from confirmed matches in human-readable output, retaining their known identity, evidence, and reason. The implementation plan proposes `match_status: matched|unresolved` in each JSON result's reserved metadata. If requested filtering remains unresolved, return partial status and exit code 3 when usable candidate evidence exists. Source disagreement by itself does not make an unfiltered, successfully collected observation partial.

### Confirmed list filtering model

Use explicit resource-specific flags for the first version rather than introducing an expression language. Window filters should include `--pid`, `--bundle-id`, `--space`, `--display`, `--title`, `--on-screen true|false`, and `--minimized true|false` where the selected source supports them. Combine different filters with AND, multiple values of the same filter with OR, and use literal substring matching for titles. The proposed [dependency table](CLI_IMPLEMENTATION_PLAN.md#native-filter-dependency-table) fixes sources and applicability before predicate evaluation, including AX/CG title evidence and complete membership sets. Collection order must not decide which source is authoritative.

Filters request their fixed evidence dependencies in addition to summary fields for retained candidates. Reject a filter inherently unsupported by the selected source rather than consulting another source implicitly. Runtime inability to evaluate a supported filter must be reported as uncertainty, not silently treated as a false predicate. Three-valued AND can still establish a non-match from another confirmed-false field; incomplete inventory cannot prove global absence. Preserve unresolved candidates separately as specified above.

### Confirmed native objects without window IDs

Keep AX window records even when no WindowServer ID can be resolved; raw native collection must not reuse tracking admission that drops such records. Give them capture-local evidence references, not invented native IDs that appear reusable by later commands. Detail access is through their owning application: `app inspect <pid> --source native --show windows.ax` explicitly enumerates the application's top-level AX windows and reads each window's own attributes within the time budget. Ordinary app detail does not perform this expanded collection by default.

This is a declared application-to-window projection, not recursive AX child-control traversal. It does not select windows or native tabs, and `window inspect <id>` continues to require a real native window ID. Reference identities remain local to one observation; a later invocation performs a fresh collection.

### Confirmed layout target selection

`space layout inspect --space <id>` exposes the selected Space's Spool-owned layout and column ordinals. `space layout equalize --space <id> --column <n>` equalizes the heights of the selected stack column. `space layout balance --space <id> --reference-column <n>` uses that column's width to balance ordinary columns across the selected Space. `space layout toggle tiled-visibility --space <id>` selects the strip directly. Omitted Space selectors use the current Space; omitted column selectors use the focused column when it belongs to that Space. An explicit Space without a usable focused column must not silently borrow a reference from another Space.

Column ordinals start at one, appear in layout inspection, and denote current positions rather than stable identities. Resolve the ordinal once during command execution; out-of-range or ineligible explicit targets fail without fallback or implicit focus/Space switching. Preserve existing layout-mutation admission for every affected member in this first version. Its visibility gate is the temporary implementation boundary below; selecting a target does not bypass current write protections.

Confirmed numbering basis: retained Layout State columns, including columns whose members are currently hidden or unavailable, with availability clearly shown in inspection. Filtering display output must not renumber columns. This differs from window focus's existing navigable-layout ordinal; the command reference should explain that distinction. Actual structural insertions or removals still change ordinals.

### Temporary first-version mutation visibility boundary

First-version explicit geometry and arrangement targets must belong to a confirmed visible user Space on any connected display, including a secondary display that is not active. Inspection remains available for all observable or retained Spaces. Reject targets on invisible Spaces or targets with unresolved visibility, without switching Spaces, changing focus merely to select them, or queuing delayed work. Existing per-window availability, fullscreen, capability, and in-flight move protections still apply to every affected member; visibility alone does not authorize a write. This restriction does not cover explicit native Space focus or cross-Space move commands, whose purpose is to change native location or visibility.

This gate is explicitly temporary. [ADR 0006](adr/0006-declarative-state-driven-window-management.md) requires future work to separate valid state edits from native effect eligibility: an invisible Space should ultimately accept desired-layout changes, retaining the latest state until effects can run. That is declarative reconciliation, not replaying a queue of old commands. Current protections remain until this separation is implemented; do not make visible-only state editing a permanent API assumption.

### Open decisions

The remaining proposed defaults, exact migration spellings, source payloads,
and implementation boundaries are consolidated in [the implementation
plan](CLI_IMPLEMENTATION_PLAN.md). They are reviewable proposals covered by the
final implementation approval, not additional silently accepted decisions.

### Confirmed action acceptance receipts

Code inspection shows `src/client.rs::dispatch_action` currently sends `Request::Dispatch` without an execution-admission reply; `crates/shared_types/src/wire.rs` documents that request as fire-and-forget. Transport success cannot establish that a native ID, retained column, visibility gate, or multi-member layout mutation was accepted. The new explicit-target error contract therefore requires more than reorganizing CLI parsing.

Daemon-dispatched CLI controls wait for a typed admission receipt after the daemon resolves the target and validates the operation in its ordered command execution path. Exit 0 means accepted; a rejected target or operation returns a reason and nonzero status. Neither transport delivery nor acceptance proves macOS has completed an asynchronous action. First-version commands do not wait for final animation, focus, or Space-transition convergence; users observe results separately. A response timeout reports that acceptance is unknown and must not automatically retry a potentially accepted command.

Keep native inspection independent of this daemon path. Shared Lua command vocabulary does not by itself change typed Lua API return contracts; any receipt integration must respect the Lua worker's asynchronous world-access boundary and must not block the main thread waiting on itself.

The proposal now covers [startup and shutdown](CLI_IMPLEMENTATION_PLAN.md#startup-and-shutdown-request-handling): process quit remains available before Bevy, ECS-dependent requests return `not_ready`, and rejected requests never execute later when permission arrives. [Version negotiation](CLI_IMPLEMENTATION_PLAN.md#version-negotiation-and-v5-compatibility-rejection) uses a frozen v5-decodable transport sentinel on the same connection before submitting new requests. Its compatibility Ack is distinct from an action receipt; a failed handshake means not submitted, while a lost post-submission receipt leaves acceptance unknown. No sentinel is dispatched as a state query.

Lua examples must use the existing single-value call shape, such as `spool.run("window focus 456")` or `spool.run({ "window", "focus", "456" })`; sharing command grammar does not imply a variadic Lua function.

See [ADR 0004](adr/0004-independent-native-observation.md) for the agreed architectural boundary.

## Confirmed supplementary command mapping

| Existing CLI | New CLI | Semantic boundary |
| --- | --- | --- |
| `script <file>` / `script -e <code>` | `script run <file>` / `script run -e <code>` | Retain isolated client-script execution and argument passing. |
| `action bar toggle-collapse` | `bar toggle-collapse` | Retain active-display Bar behavior. |
| `action mouse nextdisplay` | `mouse next-display` | Retain current destination-display mouse and focus behavior; the existing handler also focuses a destination window when available. |
| `action mission-control` | `session mission-control` | Request the native overview through the daemon. |
| `action show-desktop` | `session show-desktop` | Request the native desktop overview through the daemon. |
| `action reconcile-windows` | `window reconcile` | Explicitly request daemon reconciliation; never part of native inspection. |
| `action printstate` | `service dump-state` | Retain the internal state diagnostic log dump, distinct from structured session inspection. |
| `migrate-state` | `service migrate-state` | Retain file preview/apply semantics; does not require a running daemon merely because it belongs to its administration group. |

These confirmed paths regroup existing capabilities without adding a native control path. Lua's `spool.run` adopts only the corresponding daemon-dispatched action paths, not local service management, file migration, or nested client-script execution.

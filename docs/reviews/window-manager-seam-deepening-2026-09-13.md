# Window Manager Seam Deepening

Date: 2026-09-13. Baseline: `7778ae50a187340ae1991ad7a932a1f94be5132c`.
Read-only design review: no source change is proposed as already applied, and no
live window manipulation was performed.

## Scope

The macOS bridge seam: `WindowManagerApi` in `src/manager.rs`, its two adapters
(`WindowManagerOS`, `MockWindowManagerApi`), the `WindowManager` Bevy resource
that boxes it, and the callers that read native state through it. The question
is where the seam should sit and how small its interface can get.

Vocabulary is the deep-module language: **module**, **interface**,
**implementation**, **depth**, **seam**, **adapter**, **leverage**,
**locality**. Domain terms follow [CONTEXT.md](../CONTEXT.md) — in particular
**Native Observation** and source-preserving evidence.

## What the seam is today

`WindowManagerApi` is a 22-method trait over macOS `SkyLight`, Accessibility,
and CoreGraphics calls. `WindowManager(Box<dyn WindowManagerApi>)` is a Bevy
`Resource` that derefs to it. Two adapters satisfy the interface:

| Adapter | Location | Size |
|---|---|---|
| `WindowManagerOS` (production) | `src/manager.rs`, `manager/*` | ~6300 lines |
| `MockWindowManagerApi` (tests) | `src/tests/mocks.rs` | ~1800 lines |

Four items sit *outside* the trait but are part of the same seam:
`move_owned_window_to_space`, `owned_window_is_in_space`, `NativeSpaceIntent`,
and `NativeSpaceCapabilities`.

Per seam discipline, two adapters means a **real** seam, not a hypothetical one.
The problem is not that the seam exists — it is that it sits at the wrong
altitude.

## Diagnosis

### Depth at the interface is low

Of 22 methods, 13 have exactly one or two production call sites outside
`src/manager`, and each maps 1:1 onto a single platform call. The interface is
nearly as complex as the calls it wraps, so leverage per method is close to one.

Counts below are production call sites (`src/`, excluding `src/manager` and
`src/tests`); test-only sites are noted separately where they are large.

| Method | Prod call sites | What it hides beyond one platform call |
|---|---|---|
| `cursor_position` | 3 (+21 in tests) | nothing (`CGEvent` → `Option`) |
| `warp_mouse` | 5 (+6 in tests) | nothing (`CGWarpMouseCursorPosition`) |
| `dim_windows` | 4 | one `SkyLight` brightness call |
| `window_owners_in_session` | 4 | one `CGWindowListCopyWindowInfo` |
| `find_window_at_point` | 4 | hit-test loop over the window list |
| `presentation_windows_in_workspace` | 4 | `already_minimized = false` |
| `windows_in_workspace` | 20 (+2 in tests) | `already_minimized = true` |
| `observe_displays` | 1 (+2 in tests) | `CGGetActiveDisplayList` + per-display Space read |
| `present_displays` | 1 | a projection of `observe_displays` |
| `active_display_id` | 3 | one `SkyLight` read |
| `active_display_space` | 1 | one `SkyLight` read |
| `workspace_is_fullscreen` | 2 | a Space-type bit test |
| `get_associated_windows` | 2 | one `SkyLight` read |
| `find_existing_application_windows` | 1 | a filtered window-list scan |
| `new_application` | 2 | `ApplicationOS` construction |
| `perform_native_space_intent` | 3 | gesture/shortcut synthesis (genuinely substantial) |
| `perform_system_overview` | 1 | a `Mission Control` launch |
| `native_space_capabilities` | 1 | a class-introspection probe |
| `quit` | 1 | an `Event::Exit` send |
| `window_order_in_session` | 1 | one `CGWindowListCopyWindowInfo` |
| `request_window_notifications` | 1 | one `SkyLight` subscription refresh |
| `setup_config_watcher` | 2 | a `notify` watcher over the config path |

The high-leverage methods (`windows_in_workspace` 20 sites, `warp_mouse` 5)
are high-leverage because they are *frequent*, not because they are *deep*. The
difficulty they carry is not inside them — it is around them, in the callers.

### The interface states an invariant it violates

`present_displays` is a default method:

```rust
fn present_displays(&self) -> Vec<(Display, Vec<WorkspaceId>)> {
    self.observe_displays()
        .unwrap_or_default()          // ← failed inventory becomes empty inventory
        .into_iter()
        .filter_map(DisplayObservation::into_known_topology)
        .collect()
}
/// A failed physical inventory is not an empty inventory. Failed Space
/// reads retain their physical display in the successful observation.
fn observe_displays(&self) -> Result<Vec<DisplayObservation>>;
```

The doc comment on the very next declaration forbids what the default body
does. `unwrap_or_default()` erases the failure that ADR
[0004](../adr/0004-independent-native-observation.md) requires be preserved:
"Read failures, timeouts, and work skipped on budget expiry remain distinct from
successful empty results." A caller cannot tell an empty desktop from an
unreadable one, and the one caller that matters (`native_space.rs:640`, the
known-user-Space precondition) silently degrades.

### Five different unavailability encodings in one interface

| Encoding | Methods | Failure looks like |
|---|---|---|
| `Result<T>` | 9 | `Err` |
| `Result<T>` where *empty* is also `Err` | 2 | `Err(NotFound)` |
| `Option<T>` | 3 | `None` |
| bare `bool` | 1 | `false` |
| bare `Vec<T>` | 1 | `vec![]` |

`get_associated_windows` returns `Vec<WinID>` with no error channel at all, so a
failed read is indistinguishable from "this window has no children".
`workspace_is_fullscreen` returns `bool` with the same problem. Meanwhile
`windows_in_workspace` and `presentation_windows_in_workspace` overload
`Err(NotFound)` to mean *empty Space*.

> **Fixed for one of the two.** `get_associated_windows` had a worse problem
> than a missing error channel: its `SkyLight` binding declared the return as
> `NonNull<CFArray<CFNumber>>` while its own doc comment said the call returns
> `NULL` "if not found or an error occurs", and the call site passed that
> straight to `CFRetained::retain`, whose parameter is a `NonNull`. The sibling
> binding for the same nullable "Copy rule" shape,
> `SLSCopyManagedDisplaySpaces`, is declared `*mut` and guarded with
> `NonNull::new`. The binding now matches that convention and the call site
> guards the pointer. `NULL` is mapped to an empty list rather than an error,
> because the API does not separate "no associated windows" from "failed" and
> both callers already treat the result as a list that may be empty; what
> changed is that the outcome is a defined empty list instead of an invalid
> `NonNull` reaching `CFRetain`. `workspace_is_fullscreen` still returns a bare
> `bool` and is left alone: giving it a failure channel means the fullscreen set
> in `NativeTopology` needs a third "unknown" state, which changes which Space
> operations are refused, and that is a design decision this review should not
> make implicitly.

### Method-by-method failure contract inventory

Reading each implementation against its declaration, rather than trusting the
counts above. `Failure looks like` is what the *production* adapter actually
does, not what the doc comment claims.

| Method | Declared | Failure looks like | Failed vs empty/negative distinguishable? | Contract documented? |
|---|---|---|---|---|
| `perform_system_overview` | `Result<()>` | `Err` | n/a (effect) | yes |
| `native_space_capabilities` | `NativeSpaceCapabilities` | infallible class probe | n/a | yes |
| `perform_native_space_intent` | `Result<()>` | `Err` | n/a (acceptance, not completion) | yes |
| `new_application` | `Result<Application>` | `Err` | n/a | yes |
| `get_associated_windows` | `Vec<WinID>` | `NULL` → `vec![]` | **no** | yes (added) |
| `observe_displays` | `Result<Vec<DisplayObservation>>` | outer `Err`; per-display `Err` | **yes** | yes (added) |
| `active_display_id` | `Result<u32>` | `Err` | n/a | yes |
| `active_display_space` | `Result<WorkspaceId>` | `Err` only from the UUID conversion; **the Space read itself is never checked** | **no** | claims Ok/Err the read cannot produce |
| `workspace_is_fullscreen` | `bool` | **collapses a three-valued domain** | **no** | **no** |
| `warp_mouse` | `()` | infallible | n/a | yes |
| `find_existing_application_windows` | `Result<(Vec<Window>, Vec<WinID>)>` | `Err` from the AX list; the *supplementary* inventory failure is **swallowed** by `unwrap_or_default()`, logged at debug | **no**, for the supplementary half | claims Ok/Err |
| `find_window_at_point` | `Result<WinID>` | `Err(InvalidWindow)` at id `0` | n/a | yes |
| `windows_in_workspace` | `Result<Vec<WinID>>` | `Err` | **yes** | yes (added) |
| `presentation_windows_in_workspace` | `Result<Vec<WinID>>` | `Err` | **yes** | yes (added) |
| `quit` | `Result<()>` | `Err` (channel send) | n/a | yes |
| `setup_config_watcher` | `Result<Box<dyn Watcher>>` | `Err` | n/a | yes |
| `cursor_position` | `Option<CGPoint>` | `None` | n/a (no empty case) | yes |
| `dim_windows` | `()` | silent return on overflow; platform failure only at debug | **no** | no (levels only) |
| `window_owners_in_session` | `Option<HashMap<WinID, Pid>>` | `None` | n/a | **no** — `None`'s meaning is unstated |
| `window_order_in_session` | `Option<Vec<(WinID, Pid)>>` | `None` | n/a | yes |
| `request_window_notifications` | `Result<()>` | `Err`; **also `Ok` without asking on macOS < 15** | **no** | no (short-circuit unstated) |

Three findings from this pass that the aggregate view hid:

1. **A three-valued platform answer is stored as one bit.**
   `SLSSpaceGetType` documents `0` = user/desktop, `2` = system (e.g.
   Dashboard), `4` = fullscreen, and documents **no failure value**.
   `workspace_is_fullscreen` keeps only `== 4`, so a system Space reads as a
   user Space — and `NativeSpace::kind`, which is derived from that same bool,
   labels it `SpaceKind::User`. The predicate is not missing an error channel;
   it is answering a question with more answers than `bool` can hold. `SpaceKind`
   already exists as the domain vocabulary but has only `User` and `Fullscreen`
   variants.
2. **An unchecked read is returned as a value.**
   `active_display_space` converts the display ID to a UUID (`Result`) and then
   uses `SLSManagedDisplayGetCurrentSpace`'s return directly, so a failed read
   arrives as `Ok(0)`. This is the same shape as the three-valued collapse, one
   step worse: there is not even a check to mis-collapse.
3. **The `unwrap_or_default` shape survives in a second place.**
   `find_existing_application_windows` swallows a supplementary inventory
   failure with `unwrap_or_default()` — the exact pattern deleted from
   `present_displays` — so an unreadable WindowServer inventory produces "no
   off-screen windows". It is at least logged at debug, which
   `present_displays` was not.

A fourth, weaker item: `request_window_notifications` returns `Ok(())` without
requesting anything on macOS < 15. That is a deliberate version short-circuit
rather than a failure, but nothing in the interface says so, so a caller cannot
tell "subscribed" from "not applicable here".

**Decision needed before changing any of this.** For (1), the two candidate
semantics are: refuse every Space operation whose kind is not `User`
(recommended — it makes "unreadable or unusual" fail closed and matches how the
precondition is already used), or model system Spaces explicitly, which means a
third `SpaceKind` variant and therefore a client-visible change in
`spool-shared-types`, `client.rs`'s string mapping and `SpaceState.kind` on the
wire.

### The two adapters disagree about that error mode

`space_window_list_for_connection` (`src/manager.rs:1046`) returns
`Err(Error::NotFound)` when the platform reports zero windows:

```rust
let count = window_list_ref.count();
if count == 0 {
    return Err(Error::NotFound(format!("{}: zero windows returned", function_name!())));
}
```

The test adapter returns `Ok(vec![])` for the same condition
(`mocks.rs:532` `query_workspace_windows`). So the production and test adapters
satisfy one interface with **different observable behaviour**, and the test
suite cannot detect the production error mode.

Callers have patched around the ambiguity inconsistently:

- `src/bar/state.rs:80` special-cases it, with the rule written out longhand:
  `// SkyLight reports an empty Space as NotFound.`
- `src/ecs/reconcile.rs:784` special-cases it a second time:
  `Err(Error::NotFound(_)) => Some(HashSet::new())`.
- `src/ecs/topology.rs:118` does **not** special-case it —
  `for window_id in manager.windows_in_workspace(space)?` propagates `Err`, so
  one empty Space aborts the entire `observe_memberships` scan and
  `is_complete()` fails.
- `src/ecs/workspace.rs:1153`, `src/ecs/focus.rs:667`, `src/commands.rs:329`
  collapse both meanings with `.ok()`.

That is one under-specified error mode, re-derived four different ways, in four
modules, with a latent bug in the least defensive of them. This is the concrete
cost of a shallow seam.

### Composition protocol lives in callers

Ordering and gating knowledge that belongs behind the seam is re-implemented at
each call site:

- **Capability gating.** `config.space_control_enabled()` is checked, then
  `perform_native_space_intent` is called, then the error is mapped to a
  rejection code — at `native_space.rs:231`, `:615`, and `:635`.
- **Acceptance is not completion.** ADR
  [0007](../adr/0007-explicit-intent-ownership-and-realization.md) separates
  accepted state from native completion, but the interface offers only
  `Result<()>`. Callers re-derive verification: `overlay.rs:1070-1071` submits
  `move_owned_window_to_space` and then immediately re-reads
  `owned_window_is_in_space`, and `native_space.rs` tracks
  `PendingFollow`/`PendingMove` transactions to poll for settlement.
- **Cost knowledge buried in a call site.** `topology.rs:163` explains that
  reading one Space's membership rather than every Space's "is what a Bar click
  can afford to pay", while `mocks.rs:528` exposes
  `workspace_membership_query_count()` so tests can assert that cost. Cost is a
  documented interface property in prose and an assertion in the mock, but it is
  not in the interface.

### Two methods differ by an unnamed flag

`windows_in_workspace` and `presentation_windows_in_workspace` are the same
call with `also_minimized` set to `true` and `false`. The distinction that
matters — whether minimized windows are eligible — appears in neither name, is
documented in no `docs/` file, and is stated only obliquely in a doc comment
("excluding ordered-out retained surfaces"). A caller choosing between them
cannot infer the choice from the interface.

### Methods that are not window management

`setup_config_watcher`, `quit`, `perform_system_overview`, and `dim_windows`
are in the trait because it is the process's "talk to macOS" bag. Only
`dim_windows` is arguably window management. Every caller of the window and
Space logic pays for four methods it never uses, and every test double must
decide what to do about them.

> **Revised — this section's conclusion did not survive contact.** Two of the
> four are substituted by tests, which makes them a real seam rather than a
> pass-through. `dim_windows` has `expect_dim_windows` in
> `src/tests/exit_restore.rs`, and `perform_system_overview` is substituted by
> `system_overview_actions_reach_the_platform_once_per_request_even_after_failure`
> in `src/commands.rs` to check that each request reaches the platform exactly
> once even after a failure. Moving either one off the trait would put a real
> Mission Control launch into the test process and delete the only coverage of
> that dispatch rule. The other two would need `EventSender` to become a Bevy
> resource; a one-method reduction is not worth new event-channel plumbing. See
> [Migration](#migration) step 7.

### Deletion test

| Delete | Result | Verdict |
|---|---|---|
| `windows_in_workspace` | 20 call sites re-implement membership reads | earning its keep |
| `perform_native_space_intent` | 3 call sites re-implement gesture synthesis | earning its keep |
| `present_displays` | 1 caller inlines two lines | pass-through |
| `quit` | 1 caller sends `Event::Exit` itself | pass-through |
| `workspace_is_fullscreen` | 2 callers read a bit from the topology they already fetch | pass-through |
| `get_associated_windows` | 2 callers, no error semantics to preserve | thin |
| `native_space_capabilities` | 1 caller | thin, but the fact is real |

## Proposed interface

The deepening is **not** to split the 22-method bag into three smaller bags.
Partitioning is not depth: it redistributes the same knowledge. Depth comes from
moving *interpretation* behind the seam — the empty-versus-unavailable
distinction, the accept-versus-complete protocol, the ordering, and the cost.

> Revised after the first two steps landed: `Observation<T>` below is a sketch
> of the target shape, not a commitment to add it at every seam.
> `DisplayObservation` already carries per-source evidence for displays, and
> `Result` already distinguishes an empty membership list from an unreadable
> one. See [Migration](#migration) for what is actually being introduced and
> where.

One value type carries unavailability, so it cannot be forgotten:

```rust
/// The outcome of one native read. `Observed` may be empty; `Unavailable` is
/// never empty. Callers cannot construct one from the other.
pub enum Observation<T> {
    /// A native source answered. An empty collection here is a real empty.
    Observed(T),
    /// No native source answered. Distinct from an empty answer, always.
    Unavailable(ReadFailure),
}

impl<T> Observation<T> {
    pub fn observed(self) -> Option<T>;
    pub fn or_unavailable(self, fallback: T) -> T;  // explicit, logged, visible
}
```

The read port replaces 10 methods with 5 entry points:

```rust
/// Everything a caller must know to read native macOS state.
pub trait NativeState {
    /// Runtime capabilities. Cheap; no native read.
    fn capabilities(&self) -> NativeSpaceCapabilities;

    /// One display and Space topology sample.
    fn topology(&self) -> Observation<Topology>;

    /// Windows macOS lists in one Space, without scanning the others.
    /// `scope` selects whether minimized windows are eligible.
    fn membership(&self, space: WorkspaceId, scope: Scope) -> Observation<Membership>;

    /// Session-wide window order and owners, front to back.
    fn session_windows(&self) -> Observation<SessionWindows>;

    /// Pointer position and the window under it.
    fn pointer(&self) -> Observation<PointerState>;
}

/// Which windows a membership question is asking about.
pub enum Scope {
    /// Minimized windows are eligible: this asks what the Space holds.
    Held,
    /// Only ordered-in surfaces: this asks what the Space shows.
    Presented,
}
```

The control port replaces acceptance-as-completion with an explicit two-step:

```rust
/// Submitting is acceptance. Completion is a separate, explicit question.
pub trait NativeSpaceControl {
    fn submit(&self, intent: NativeSpaceIntent) -> Submission;
    /// Verify a claim against current native evidence.
    fn verify(&self, claim: SpaceClaim) -> ClaimResult;
}

pub enum Submission {
    /// macOS accepted the operation. Not a statement that it happened.
    Accepted,
    /// Capability or configuration refuses this operation.
    Refused(Refusal),
    /// Accepted, then failed.
    Failed(Error),
}
```

`Scope::Held` / `Scope::Presented` names the flag that
`windows_in_workspace` / `presentation_windows_in_workspace` leave implicit.
`SpaceClaim` and `ClaimResult` reuse the vocabulary already in
`topology.rs:54` rather than inventing a parallel one.

**The proposed interface is smaller where it matters**: a caller that only reads
state learns 5 entry points and one result type, instead of 22 methods and 5
error encodings. Total surface across the two ports is 8, down from 22, and no
method is a 1:1 platform wrapper.

## What hides behind the seam

- Which platform calls answer a question, and in what order.
- That `Err(NotFound)` from the Space window query means *empty*, and that
  distinguishing *empty* from *gone* requires correlating with the topology
  sample — a fact no single platform call carries, and the reason this module
  is deep rather than a rename.
- That a failed inventory is never an empty inventory.
- Capability probing, and which intents are refusable without a platform call.
- The cost model: one full membership scan is per-Space reads; one Space's
  membership is one read. `membership` exposes the cheap question directly
  instead of making callers reason about an `also_minimized` boolean.
- Gesture and shortcut synthesis for Space focus.

## Dependency strategy

Category **4, true external (mock)**. macOS `SkyLight`/AX/CoreGraphics is a
third-party system we do not control and cannot run under test. So the deepened
module takes the platform dependency as an injected port and tests supply a mock
adapter — which is already the shape in use. Two consequences follow.

**The mock gets much smaller.** `mocks.rs` is ~1800 lines because the seam sits
at the FFI-verb altitude, so the mock must emulate `SkyLight` quirks
(`ordered_out`, `withdrawn_surfaces`, presentation inventory availability,
scripted membership queues). At the question altitude the mock only needs to
hold a small in-memory desktop and answer questions about it truthfully. The
mock's job becomes "be a small honest macOS", not "reimplement a private
framework".

**The seam becomes one adapter deep, twice.** The production adapter keeps the
platform knowledge; the mock adapter keeps the fixture knowledge; the
interpretation lives in the module and is tested once, through the same
interface callers use.

Internal seams (the per-source collectors behind `topology()` and
`session_windows()`) stay private to the implementation and are exercised by the
module's own tests. They are not part of the interface, even though tests use
them.

## Test surface

The interface is the test surface — callers and tests cross the same seam.

Tests should assert on:

- `Observation::Unavailable` is produced when a source fails, and
  `Observation::Observed(vec![])` is produced for a genuinely empty Space, with
  **both adapters agreeing**.
- `is_complete()`-style gating is driven by `Unavailable`, not by emptiness.
- `Submission::Accepted` does not read as completion; `verify` returns
  `Confirmed`/`Refused`/`Unavailable` against changed native evidence.
- A `Scope::Held` / `Scope::Presented` difference for a minimized window.
- Read cost, through `membership` being asked for one Space rather than a full
  topology scan.

Tests should **not** assert which platform verb was called. Assertions like
`workspace_membership_query_count()` describe implementation; the equivalent
question-level assertion is "one `membership` call, not a full scan". Any test
that must change when the platform adapter changes is testing past the
interface.

## Migration

Replace, don't layer. The current 22 methods must not survive as a public
superset.

Status: steps 1, 2a, 2b, 2c and the capability half of step 6 have landed
(`47d239c`, `5399477`, `4d8f98a`, `5aa9579`, `b16da18`, `680fe51`), plus one
soundness defect the diagnosis surfaced (`63e32d9`). The plan is revised from
its first draft each time the investigation contradicts it; the revisions are
recorded rather than silently applied. Two of the original steps were rejected
outright once the evidence was in — step 5 and step 7 — and those rejections
are part of the result, not gaps in it. The interface is 21 methods, down from
22; the target shape sketched under [Proposed interface](#proposed-interface)
was not reached, and the reasons are on the record rather than left as an
unexplained shortfall.

1. **Fix the adapter divergence.** Landed. Both adapters answer an empty Space
   with `Ok(vec![])`, the contract is stated on both membership methods, and
   the now-unreachable `NotFound` special cases are gone.
2. **Retire the evidence-collapsing display pass-through.** Landed. This
   replaces the original step 2, which called for introducing `Observation<T>`
   here. `DisplayObservation` already *is* the source-preserving carrier — it
   pairs a `Display` with its own `Result<Vec<WorkspaceId>>` — so a generic
   `Observation<T>` at this seam would have been a second encoding of the same
   idea layered on the first, adding surface without adding depth. The
   invariant violation was in `present_displays`, and deleting it was the
   deepening.
3. **Cover the preconditions before moving them.** Landed as step 2b. The
   `native_precondition_failed` rejection for a Space move to an unknown target
   had no test; both branches now have one, mutation-checked by neutering the
   precondition.
4. **Let the observation epoch own the per-Space membership fallback.**
   Landed. `SpaceMemberships` answers a Space at a time from one scan, and
   reads a Space alone when the scan could not be taken. The Bar's inline
   fallback and its `WindowManager` system param are gone, and
   `DefaultGeometry::viewport`, which hand-rolled the same scan plus its
   uniqueness and failure rules, now asks the epoch.
5. **Deliberately *not* migrating the single-Space callers.** This is the
   revision step 2c produced. `windows_in_workspace` still has callers in
   `workspace.rs`, `focus.rs`, `systems.rs`, `commands.rs`, `triggers.rs`,
   `exit_restore.rs` and `reconcile.rs`, and each asks about exactly one Space.
   For them the epoch's eager scan is *more* expensive than the one read they
   need — `focus.rs` caches a per-Space read precisely to avoid paying for
   Spaces it never asks about, and `systems.rs` runs from `finish_setup`, which
   has no topology to consult, so the epoch would fall back to those same reads
   while adding a resource dependency. Their
   failure policies also differ on purpose (skip, propagate, no filter), so a
   shared wrapper could only unify them by lying. Routing them through the
   epoch would trade measured platform cost for indirection, which is the
   opposite of the goal. The epoch is for callers that need many Spaces; the
   cheap read is for callers that need one.
6. **Introduce `NativeSpaceControl`.** Landed as `SpaceControl`, but only the
   capability half, and the rest is deliberately not built. `SpaceControl`
   computes the effective capability where configuration permission and the
   platform's own answer meet; the gates and the inspection report now ask the
   same value instead of disagreeing, and the intent-to-capability mapping lives
   with it. The proposed `Submission` enum is *not* warranted: once the gate
   refuses on capability, the only platform refusal left that would map to
   `Refused` is unreachable, leaving `Submission::Failed` as its only real
   variant. An enum whose distinguishing variant no producer can produce is
   surface, not depth.
7. **Do not move the four non-window-management methods out.** The rationale
   was that the trait is the process's "talk to macOS" bag and callers pay for
   four methods they never use. Two of the four turn out to be substituted by
   tests — `dim_windows` by `src/tests/exit_restore.rs`, `perform_system_overview`
   by a dispatch test in `src/commands.rs` — which makes them a real seam, and
   moving the latter would run an actual Mission Control launch inside the test
   process while deleting that dispatch rule's only coverage. The other two
   (`quit`, `setup_config_watcher`) need `EventSender` to become a Bevy
   resource, which is new event-channel plumbing bought for a one-method
   reduction. Recorded as rejected rather than left undone.

## Trade-offs

**Where the leverage is high.** The empty-versus-unavailable distinction is the
single highest-value thing to move behind the seam: it is currently re-derived
in four modules, one of which gets it wrong, and the mock cannot see the
production behaviour. The `Scope` parameter removes a silent wrong-choice
footgun. The effective-capability answer stops the gates and the inspection
report from disagreeing about what the platform can do.

**Where it is thin.** `pointer()` bundles `cursor_position`, `warp_mouse`, and
`find_window_at_point`, which are three different concerns; a read-only port
holding a mutating `warp_mouse` is not obviously right, and `warp_mouse` has 5
production call sites that mostly want "put the pointer here". Consider
splitting the mutating half out. `session_windows()` is used mostly by
inspection and may belong with the CLI's independent native observation rather
than the daemon resource.

**What this does not fix.** The `#[automock]` derive is not the problem and
removing it is not part of this. The `NonSend` main-thread discipline on the
`WindowManager` resource is orthogonal — the deepened interface still must not
be called off the main thread, and the Lua worker rule in
[AGENTS.md](../../AGENTS.md) still applies. `display_space_list`,
`active_display_uuid`, and `connection_for_process` are already private helpers,
so they are internal seams today and stay that way.

**The honest risk.** This is a large refactor of the most-called interface in
the project, and `windows_in_workspace` alone has 20 production call sites. Landing it in
the five steps above keeps each step reviewable, but step 4 is where a
half-migrated seam would be worse than either endpoint. If the full split is not
warranted now, step 1 is the piece that pays for itself immediately and should
land regardless.

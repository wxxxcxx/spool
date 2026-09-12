# Bar click → window switch latency

Date: 2026-09-12
Status: diagnosed; findings 1–6 fixed
Scope: clicking a window icon in the Bar to switch windows in the same
application.

## Fixes applied

- **Finding 1** (three native membership scans per extraction): one
  `WindowMemberships` now serves the state document, the window set and the
  Bar's own per-Space membership. A failed scan still degrades each Space
  individually, as before. Pinned by
  `one_bar_extraction_reads_native_membership_once_per_space`, which reads three
  scans per Space on the pre-fix code and one now.
- **Finding 2** (`Changed<Window>` re-dirtied the Bar every frame): the run
  condition is now the named `bar_projection_dirty`, built from what the Bar
  actually draws — strips, Spaces, the focused/floating/hidden/unavailable
  states of the windows it lists, the app identity behind an icon, displays and
  config — plus a pending focus request. A window's cached frame and title are
  carried but never drawn, so they no longer appear. Pinned by
  `incidental_window_mutation_does_not_dirty_the_bar_projection`.
- **Finding 3** (frames of 70–180 ms while idle): a consequence of 1 and 2;
  see *Measured after* below.
- **Finding 4** (the indicator waited for accessibility to confirm the focus):
  the Bar draws a pending focus request as its focused window, scoped to the
  Space that holds it so another display's Bar keeps its confirmed focus, and
  without relaxing the visibility/unavailable gates. Pinned by
  `a_requested_focus_moves_the_indicator_before_accessibility_confirms`.
- **Finding 5** (a Bar click fed the focus path and cleared the confirmed
  focus): `BarManager::pointer_is_on_chrome` plus a guard at the top of
  `mouse_down_trigger`. A panel only takes mouse events while the pointer is on
  the Bar's own chrome, which is the same test AppKit uses to decide who
  receives the click.
- **Finding 6** (the bar-click path paid a topology sample before the focus
  request): `focus_window_command` now takes the Space the caller drew the
  window in. When that claim is confirmed it skips the scan of every *other*
  Space, reading only the claimed Space's membership; a refusal falls back to
  the full observation and an unreadable Space refuses. The one guarantee traded
  is the cross-Space membership ambiguity, which a caller that names its Space
  resolves for itself.

## Symptom

The clicked window takes focus immediately (its traffic lights light up), but
the Bar's own focus indicator and its motion arrive late — reported as roughly
0.3–0.5 s, "half a beat behind" the window. Keyboard focus switching was
reported as not slow.

## Method

Temporary tagged probes (`[DEBUG-a4f2]`, target `spool::latency`) were added
along the whole path and removed afterwards (tree is clean):

| Probe | Site |
| --- | --- |
| `bar_dispatch` | `BarView::dispatch` (`src/bar/appkit.rs`) |
| `mouse_down … tracked=` | `mouse_down_trigger` (`src/ecs/mouse.rs`) |
| `focus_window_command enter/observed` | `focus_window_command` (`src/ecs/native_space.rs`) |
| `focus_request` / `focus_requested` | `focus_window_trigger` (`src/ecs/focus.rs`) |
| `focus_cleared untracked/unresolved/invalidated` | `FocusCoordinator::apply_signal` |
| `untracked_focus_observation` | `queue_untracked_focus_observation` (`src/ecs/triggers.rs`) |
| `ax_focus_resolved` | `front_switched_trigger` (`src/ecs/triggers.rs`) |
| `focus_observation_confirmed` | `window_focused_trigger` (`src/ecs/triggers.rs`) |
| `project_confirmed_focus` | `project_confirmed_focus` (`src/ecs/focus.rs`) |
| `bar_update … extract_ms= update_ms=` | `update_bar` (`src/bar/mod.rs`) |
| `bar_dirty [terms]` | the `bar_dirty` run condition (`src/ecs.rs`) |
| `bar_motion_start` | `BarMotion::retarget` (`src/bar/motion.rs`) |
| `frame_gap_ms` | `pump_events` (`src/ecs/systems.rs`) |
| `window_changed` | one-frame probe system over `Changed<Window>` |

Raw captures: `/tmp/spool-latency*.log` (ephemeral). Reproduce by rebuilding
with the probes and running
`RUST_LOG=info,spool::latency=debug ./target/debug/spool launch`.

## Measured timeline (one bar click)

Two windows of the same application, ids 44 and 20070.

```
  0.0 ms  bar_dispatch FocusWindow { window_id: 20070 }     (Cocoa mouseUp)
  5.9     focus_window_command enter                        (event channel hop)
 17.5     focus_window_command observed                     observe_visible_window_space
 17.9     focus_request                                     focus.request()
 25.8     focus_requested                                   SLS + AX raise returned
 89.4     ax_focus_resolved  source=AccessibilityWindow     AX kAXFocusedWindowChanged
 91.2     focus_observation_confirmed  confirms_request=true
146.8     project_confirmed_focus target=Some(192v0)        FocusedMarker inserted
190.1     bar_motion_start focused_window=Some(20070)       indicator motion begins
```

Four consecutive clicks measured **190 / 671 / 245 / 241 ms** from dispatch to
the indicator *starting* to move. The shared 240 ms ease-out
(`motion::DURATION`) is then added on top, so the indicator lands roughly
**0.45–0.9 s** after the click.

Separately, every bar click is preceded by:

```
mouse_down point={ x: 760, y: -1064 } window=19067 tracked=false   (Bar band)
focus_cleared untracked pid=None window=Some(19067)
project_confirmed_focus target=None
bar_motion_start focused_window=None focus_items=0                 indicator animates OFF
```

## Measured after

The same two probes (`bar_update extract_ms`, `frame_gap_ms` over 60 ms) on the
same idle machine, before and after findings 1 and 2:

| metric (idle run) | before | after |
| --- | --- | --- |
| `extract_ms` median | 34.4 ms | **14.2 ms** |
| `extract_ms` mean | 33.7 ms | **13.7 ms** |
| `extract_ms` max | 54.1 ms | **24.1 ms** |
| `bar_update` calls | 129 in 42.8 s (3.0/s) | 82 in 38.6 s (2.1/s) |
| `bar_update` interval median | 229 ms | 425 ms |
| `frame_gap_ms` median | 105 ms | **78 ms** |

Bar extraction work per second: 3.0 × 34.4 ≈ **104 ms/s → 2.1 × 14.2 ≈ 30 ms/s**,
about 3.5× less. What is left of the frame period is not the Bar: `frame_gap_ms`
still medians 78 ms (≈ 50 ms of idle pump wait plus other per-frame work), so the
overlay/geometry pipeline is the next thing to measure, not this.

## Findings

1. **`update_bar` runs on nearly every frame, and its extraction costs ~34 ms.**
   `BarStateParams::extract()` measured **mean 33.7 ms, median 34.4 ms, max
   54.1 ms** in an *idle* run (129 calls in 42.8 s). It performs three
   separate full native-membership scans: `QueryStateParams::extract()` and
   `extract_window_set()` each call `floating_by_space()` →
   `NativeTopology::observe_memberships()` (an SLS `windows_in_workspace` call
   per Space), and `BarStateParams::extract()` then loops
   `window_manager.windows_in_workspace()` per Space again.

2. **The bar is re-dirtied every frame by `Changed<Window>`.**
   In the `bar_dirty` term vector, term 5 (`Changed<Window>`) was the only
   true term in 55 of 60 sampled frames. An idle one-frame probe saw 212
   `Changed<Window>` hits across 7 windows in ~22 s — roughly one window
   re-marked per frame, so the term never goes quiet.

3. **The main loop idles at ~45 fps, but stalls in bursts.** The first
   measurement logged only gaps over 60 ms, which made the *median of the slow
   frames* (105 ms) look like the frame period. Measuring every frame instead:
   ~2 700 frames in 50 s (≈45 fps) with a median frame of 6.4 ms, of which the
   idle pump wait is ~3 ms, PreUpdate 1.2 ms, Update 1.4 ms and PostUpdate
   0.7 ms. The tail is the problem: frames over 60 ms recur ~3/s, and PostUpdate
   and Update each showed a ~1.3 s outlier (the Update one is startup
   discovery). Every hand-off in the click path can therefore be quantised to a
   whole frame: AX confirmation → `FocusedMarker` measured 55–370 ms and
   `FocusedMarker` → `bar_motion_start` 43–131 ms, both in the runs where those
   stalls were present.

4. **The indicator is driven by *confirmed* focus only.** `BarWindow.focused`
   comes from `Has<FocusedMarker>`, which `project_confirmed_focus` sets from
   `FocusSnapshot::confirmed_entity()`. The `requested` focus — already set by
   `focus.request(entity)` and already treated as authoritative by the layout
   path (`command_move_focus` calls `ensure_visible` for exactly this reason)
   — is not used by the Bar. So the AX round-trip is on the critical path.

5. **A click anywhere in the Bar band feeds the focus path.** The global HID
   event tap emits `MouseDown` for Bar clicks, and
   `WindowManager::find_window_at_point` resolves them to window `19067` in
   the Bar band, which is untracked. `mouse_down_trigger` therefore records
   `FocusSignal::Untracked`, which clears `observed` and `requested` and makes
   the indicator animate *away* before it animates back to the new window.

6. **The bar-click path pays a topology sample the keyboard path does not.**
   `focus_window_command` calls `observe_visible_window_space` →
   `refresh_for_command` → `sample()`, measured at **12–22 ms** before the
   focus request is even issued. `command_move_focus` goes straight to
   `focus_entity`.

Why keyboard focus feels different: it shares findings 1, 3 and 4 (which are
the bulk of the latency) but skips 5 and 6. Measured keyboard
request → `bar_motion_start` was ~155 ms against ~190–210 ms for bar clicks,
so the difference is real but modest; the larger part of the perceived delay
is shared and was probably attributed to the click because the Bar is what is
being watched.

## What is left: the reconciliation audit

With findings 1–6 fixed, the remaining periodic cost is not the overlay or the
window-frame pipeline (PostUpdate medians 0.7 ms, p99 22 ms, max 80 ms) but
`reconcile_windows` in `Update`:

| measurement (50 s idle) | value |
| --- | --- |
| `reconcile_windows` calls over 8 ms | 58 |
| their total | 4.46 s (**~8 % of wall time**) |
| each | median 74 ms, up to ~300 ms |
| of that, `audit_lifecycle` | 4.27 s (97 %) |

Within the audit:

| call | hits > 5 ms | total | max |
| --- | --- | --- | --- |
| `window_owners_in_session` (session window list) | 53 | 415 ms | 15 ms |
| per-app `window_inventory` (AX) | 174 | 1 396 ms | 17 ms |

The per-app AX inventories are spread across applications that cannot own a
user window — Notification Center (56 hits), `spool` itself (28), loginwindow
(8), Wallpaper (6) — and the remainder of the audit's 4.27 s is in the rest of
its per-application loop. The heartbeat is one second
(`WindowStateSync`), so a full audit of every application runs once per second
and blocks the main thread for ~75–95 ms doing it; a click that lands inside one
waits that long before the frame that handles it even starts.

The audit's scope is every application the daemon tracks, and a heartbeat
listing showed **59 of them**, of which only a handful can own a user window.
The rest are system agents (`Dock`, `Control Center`, `Notification Center`,
`Wallpaper`, `Spotlight`, `Siri`, `loginwindow`, `TextInputSwitcher`,
`SystemUIServer`, …) and helper processes (`WeChatAppEx Helper (Renderer)`
five times over, `Google Chrome Helper`, `Dock Extra` twice). Admission
already refuses processes whose activation policy is `Prohibited`
(`policy_can_own_windows`), so the 59 are the ones macOS reports as able to own
windows — regular *and* accessory — and the audit has no per-application time
budget: `WindowStateSync`'s heartbeat does all of them in one go.

### What each automatic trigger costs, measured

`automatic_reconcile` (new option, `options = { automatic_reconcile = false }`)
drops only the full sweeps — the heartbeat and the mouse-up — and keeps the
per-application notifications and explicit requests. Two 30 s runs with
otherwise identical configuration:

| | sweeps | slow audits | total audit time | worst audit |
| --- | --- | --- | --- | --- |
| `automatic_reconcile = true` (default) | 27 | 28 | 1 768 ms | 78.8 ms |
| `automatic_reconcile = false` | 1 | 28 | **411 ms** | **35.6 ms** |

So the sweeps were ~4.3× the whole audit's cost (≈59 ms/s → ≈14 ms/s of
main-thread time), and the worst single block drops with them. What is left is
27 *scoped* audits — one application each, driven by focus and activation
notifications — which now dominate at ~15 ms each and up to 35.6 ms for a
single application. Those are what a worker thread would still remove.

Fixed while measuring: **Spool audited its own process.** Process admission
only filtered `pid != 0`, so the daemon was tracked as an application like any
other and had its own accessibility inventory read once a second.
`crate::ecs::is_own_process` now refuses it at both admission sites, and the
heartbeat listing no longer contains `spool`. Nothing about Spool's own windows
— its Bar and overlay panels — is actionable, so nothing is lost.

## Fix status

| # | Fix | State |
| --- | --- | --- |
| 1 | One membership scan per extraction instead of three | done |
| 2 | `bar_dirty` watches what the Bar draws, not `Changed<Window>` | done |
| 3 | The Bar draws a pending focus request | done |
| 4 | Bar-band mouse-downs filtered out of the focus path | done |
| 5 | `focus_window_command` takes the caller's Space | done |

The order they were done in is not the order they were found in. Fixes 4 and 5
were 12–22 ms of a 190–670 ms path, and were done first only because they were
the ones asked for; fixes 1 and 2 shrink every hop, and fix 3 removes the wait
for the accessibility round trip outright.

## Limits of this diagnosis

- Numbers come from one debug build on one machine over short runs; the probes
  themselves add log I/O and inflate frame times somewhat. `extract_ms` is
  measured around the extraction only and is the most trustworthy figure; the
  frame-gap figures are indicative, not exact. The before/after comparison uses
  the same probes, so the ratio is meaningful even where the absolute values
  are not.
- The click-to-indicator timings after the fixes were not re-measured with the
  full probe set; the improvements are established per stage (one scan instead
  of three, no per-frame dirt, request drawn immediately) rather than
  end-to-end.
- Which system re-marks one `Window` per frame was never identified. Fix 2 does
  not depend on knowing: the condition now names the inputs the Bar reads, so
  an incidental `Mut<Window>` touch cannot dirty it whatever its source. The
  test simulates that touch directly.
- No regression seam exists for the latency itself; it is a live-desktop
  frame-budget property. Each fix is pinned by the projection contract it
  changed instead: one scan per Space, an incidental `Window` touch is not a
  Bar change, a requested focus is drawn, and an unrelated Space's unreadable
  membership cannot refuse a confirmed claim.
- The overlay and window-geometry pipeline was measured and is *not* the
  remaining cost (PostUpdate: median 0.7 ms, p99 22 ms, max 80 ms). The
  remaining cost is the per-second reconciliation audit above, which has not
  been fixed — it needs a decision about reconciliation semantics, not a
  projection fix.
- The audit's 4.27 s is not fully attributed: `window_owners_in_session` and
  the per-app AX inventories account for 1.81 s of it, and the rest is in the
  audit's remaining per-application work (liveness checks, window
  representation refreshes, suspension bookkeeping).

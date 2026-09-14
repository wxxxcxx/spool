# The Bar's decisions are separated from its AppKit effects by a main-thread surface seam

Status: Accepted and implemented, 2026-09-14; desktop acceptance passed.

## Context

`src/bar/appkit.rs` is the repository's hottest file and the only place in
`src/bar/` that touches AppKit. It currently holds both the Bar's coordination
logic — the `ViewState` interaction machine, the per-display panel table, the
`update`/`animate`/`toggle_collapse` loop — and the native effects: `NSPanel`
and `NSView` construction, CoreGraphics drawing, `CALayer` hover animation,
`NSButton` toolbars, drag-preview panels, and the ObjC event selectors. The
input half of the Bar is already deep: `model.rs` produces a world-free
`BarSnapshot`, and `layout.rs` / `motion.rs` / `drag.rs` / `placement.rs` /
`preferences.rs` compute a `Presentation` with no FFI. The coordination between
`BarSnapshot` and `Presentation`, however, cannot be tested without a live
window server, because it is welded to the concrete AppKit types.

## Decision

Draw one **seam** between what the Bar decides and what it asks AppKit to do.
A new deep `Bar` module in `src/bar/runtime.rs` owns `ViewState`, the
per-display panel table, and the `update` / `animate` / `toggle_collapse`
coordination. It depends only on `BarSnapshot`, `BarScreenMetrics`,
`BarPreferences`, and a `BarSurface` port.

- **Data in, effects out.** Environment reads (screen geometry, menu-bar
  height, notch, measured label widths, pointer and button state) are taken at
  the adapter edge and passed in as plain data. The pure module never queries
  the environment mid-decision, and icon images never cross the seam: `present`
  resolves bundle identities inside the adapter.
- **One `present` call owns drawing.** The adapter consumes the pure
  `Presentation` and performs all CoreGraphics work; the pure side decides only
  what, where, and with what emphasis.
- **One input entry.** ObjC selectors translate to a plain `BarInput`; the
  adapter queues them, and the outer layer drains them into
  `Bar::handle(input) -> BarOutcome`. Actions leave as returned values, not via
  an injected `EventSender`.
- **One imperative `BarSurface`** whose methods carry idempotent/reconciling
  semantics (ensure/remove panel, present, set interactivity, sync toolbar,
  set button highlight, show/hide drag preview).
- **The port is main-thread-only.** `BarSurface` does not require
  `Send + Sync`, matching the existing `NonSend` `BarManager` resource; AppKit
  must run on the main thread regardless.
- `BarManager` stays the `NonSend` resource but becomes a thin holder of `Bar`
  and `Box<dyn BarSurface>`; `appkit.rs` becomes the `AppKitSurface` adapter.

## Considered options

- **Only extract `ViewState` into a pure interaction module**, leaving
  `BarManager`'s panel lifecycle concrete. Rejected: the panel reconcile loop
  is the part that changed most often and would stay untested.
- **Emit a `Vec<DrawCommand>` vocabulary** instead of passing `Presentation` to
  `present`. Rejected: it encodes the same geometry twice and adds a second,
  parallel description of what to draw.
- **Expose drawing primitives on the port** (`fill_rect`, `stroke_rounded`,
  …). Rejected: a very wide, shallow interface that hands the pure side the
  pixel work it does not own.
- **Require `Send + Sync` on the port** to match `WindowManagerApi`. Rejected:
  it is false portability, forcing AppKit types behind `RefCell` or locks for
  no benefit.

## Consequences

The Bar's coordination and interaction become testable through an in-memory
recording adapter, and `appkit.rs` shrinks to the untestable CoreGraphics and
AppKit surface. This is a large refactor of a 2,900-line file and must be landed
as a tracer bullet — one end-to-end path (`update` + `present` + collapse)
behind the seam first, then `animate`/hover, then the input selectors, then the
drag preview and toolbar — with no step leaving two sources of Bar state alive.

Behaviour must not change. `hit_testing` and label measurement move to the side
that owns them but keep their current decisions; independent behaviour fixes are
separate work. The subjective visual and interactive parts still require desktop
acceptance on top of the seam's tests.

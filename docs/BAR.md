# Built-in Bar

Spool renders one native AppKit Bar per physical display from the same Bevy
world that owns window layout. It is part of the `spool` process; there is no
`spool-bar` executable, socket client, or separate LaunchAgent.

## Presentation

The Bar *is* the menu bar band on each display: exactly as wide and as tall as
the menu bar, flush with the screen top, with no inset and no rounding. It draws
its content on a menu-material blur backdrop, so it looks like the menu bar it
covers, and it deliberately covers the system menu bar while expanded.
The fixed Mission Control / Show Desktop buttons and the Spaces are one group,
and the group gathers toward the middle: a display without a notch
centres it, and a notched display puts it either side of the notch with both
halves hugging it, so the leftover whitespace falls at the outer ends and need
not be equal. On a notched display the group is split by count, with the fixed
buttons counting as one item, so the notch takes the first `ceil((n + 1) / 2)`
items on its left and the rest on its right; `notch_side = "left"` or
`"right"` keeps every Space on one side instead. Spaces remain in native order
and none is ever split across the notch. Creation, deletion or native
reordering recomputes the split; window counts and focus never do. Clipped
content has no hit or drop targets.
This is still a nonactivating panel, not an
`NSStatusItem`: macOS does not reserve horizontal space for it among application
menus and status items, which is why the Bar takes that space instead.

- Every native Space on the display is listed in its current macOS order.
- Each Space is clickable over its full height, including the blank padding
  above and below icons. Its background leaves 3pt gaps at the top and bottom,
  exposing the menu bar through the default transparent panel. Active Spaces use
  a subtle blue tint; others use neutral gray. Window icons retain their focus/drag behavior. The toolbar, inter-Space
  gaps, and the notch lanes are not Space-switch targets.
- The visible Space is expanded. Tiled windows are grouped by layout column;
  columns run left to right. Multi-window columns use vertically overlapping
  icons inside a single fixed-height lane, so stacks never increase Bar height.
  Lower members paint behind upper members by default; the focused member
  paints last, above its siblings, without changing layout/window order.
- Floating windows appear after the tiled columns with an overlapping-window
  symbol badge.
- Other Spaces show up to four application icons as a compact deck, with at
  least 5px exposed edges, without added backing plates or icon outlines. The first
  window in layout order is the front card; rendering proceeds back to front.
  All collapsed icons use the normal icon size and share one horizontal lane,
  including members of Stack and Tabs columns. Collapse/expansion interpolates
  between this uniform deck and the expanded column geometry. Single and floating
  icons retain their size and Y position; icons beyond the four-card limit fade
  without moving vertically, including when a transition is interrupted.
- Empty Spaces use a full-size outlined placeholder in the content region,
  never a synthetic application icon. Native fullscreen status is indicated only
  by a small bottom-right expand-arrows badge on window icons, in both expanded
  and collapsed views and with labels hidden. Space labels have no fullscreen
  symbol or extra reserved width. Fullscreen status never adds a Space outline or selection
  color; the existing active-Space and focused-window accents remain independent.
- The exact focused window has a moving accent fill, outline and bottom
  indicator. The indicator paints above icon decks so backing plates cannot
  hide selection.
- Space labels are centered horizontally and vertically and always use the
  one-based numeric Space ordinal. The old `[workspace_labels]` name mapping
  is no longer supported; existing entries are ignored rather than producing
  a mixture of letters and numbers.
- Expansion, icon movement and focus transitions share a 240ms ease-out policy
  in `src/bar/motion.rs`. The collapse morph has its own `MORPH` curve — a
  320ms spring with a four-percent overshoot by default, which is the small
  settle an `AppKit` panel has — because it is the transition the eye reads as
  the Bar folding away. Interrupted transitions begin at the last presented
  frame, and repeated snapshots do not restart motion. Content is clipped to
  its Space and to the Bar's own chrome, so it wipes away with the shape as the
  Bar collapses.
- Hover breathes. The toolbar button under the pointer and the collapsed Bar
  get a repeating 1.8s ease-in-out pulse — a translucent highlight under the
  button's symbol (0.07..0.17), and a halo around the collapsed shape made of a
  hairline rim plus the bloom its shadow casts from the same path, breathing
  between 0.30 and 0.75 layer opacity while the bloom's radius moves between
  0.60 and 1.35 times its resting 6pt (plain) or 9pt (notched). Both are Core Animation
  layers rather than per-frame drawing: pulsing them from the frame loop would
  mean running the whole ECS at refresh rate, which costs about 45% of a core
  to animate a highlight. The halo carries a rim as well as a shadow so the
  pulse is never at the mercy of a shadow alone painting nothing.
- The pulse's mechanism is one constant, `BREATH`: `Pulse` lifts the halo
  vertically as well as brightening it, `Bloom` only brightens it. How far a
  halo may stretch comes from the room the shape has left inside the band, so a
  six-point tab breathes and a capsule that already fills the menu bar only
  brightens. The halo layer spans the whole panel and draws the shape's own
  path, because a path lives in its layer's coordinates: a layer the size of the
  shape would land the halo an origin away from the shape it belongs to.

### Tuning the feel

`examples/bar_polish_prototype` (kept on the throwaway `prototype/bar-polish`
branch, with a copy in the working tree) is the primary source for the shape
and motion exploration; it renders every candidate and carries its settings in
the URL. Its vocabulary maps onto the code like this:

| Prototype | Code |
| --- | --- |
| `sn=sfillet` | `placement::CAPSULE_TOP_RADIUS` > 0 with `ChromeShape::overhang`, i.e. an overhanging top edge |
| `sn=scoop` | `SHOULDER = Shoulder::Scoop`, the concave alternative |
| `sn=bottom` / `sn=shoulder` | `CAPSULE_TOP_RADIUS` = 0, with `CAPSULE_BOTTOM_RADIUS` raised |
| `sn=capsule` | a gap under the shape, i.e. `collapsed_rect` returning a `y > 0` |
| `sp=r3` / `r5` / `capsule` | the plain tab's radii in `chrome_path`; `capsule` rounds both ends |
| `br=bloom` / `pulse` | `BREATH` |
| `br=heartbeat` | the pulse's two halves given different curves |
| `period`, `amp` | `BREATH_PERIOD`, and the range passed to `breathe` |
| `cv=ease` / `spring` / `smooth` | `MORPH`, with the curve in `motion::ease_out` / `spring` / `smooth` |
- Every Space keeps a slot: the strip never scrolls, and no Space is dropped to
  make room. The focused Space takes whatever the others leave, up to its own
  content width; the rest keep the narrowest slot that still reads as that Space
  (a collapsed Space keeps its whole deck, an expanded one its label and one
  icon). When even those minimums do not fit, every Space takes an equal share.
- A Space whose icons do not fit its slot scrolls inside the slot: the icons
  slide under the label, the label and the slot stay where they are, and
  scrolling one Space never moves another. Scrolling is bound to the Space under
  the pointer and clamped to that Space's real overflow, so an unscrollable
  Space ignores the wheel. Icons scrolled out of the slot are clipped, and hit
  testing follows: nothing can be clicked through a neighbouring slot.

## Interaction

  state directory as `state.json`, keyed by `CGDirectDisplayID`, and applied on
  the next run. Only the axes a gesture touched are stored, so resizing the width
  leaves the automatic position alone. A saved rect is clamped to its display, so
  geometry recorded against a larger or differently arranged display cannot strand
  the Bar off-screen. Deleting the file, or double-clicking the handle, restores
  automatic placement.
- Two fixed icon buttons lead the group and open Mission Control and toggle Show
  Desktop. Native buttons provide hover tooltips and click feedback without
  taking keyboard focus. The Space strip scrolls and clips independently to
  their right; neither button is a window-drag target, and both move with the
  group they lead. They also work in observe-only Space mode.
- Both buttons dispatch the shared `Action::MissionControl` / `Action::ShowDesktop`
  through the same command handler and WindowManager API as CLI and keybindings.
  The platform submits the system app bundle to Launch Services with `/usr/bin/open -n`
  and the corresponding mode; it never directly executes the AMFI-restricted
  bundle binary. No shell or synthesized keyboard shortcuts are used. The launch
  request is reaped off the UI thread and failures are logged. Request acceptance
  does not establish completion: Dock notifications remain authoritative for
  overview entry/exit. Mode arguments are macOS implementation details, not a
  public API contract.
- Click a window icon to focus that exact window.
- Click a collapsed Space to focus it when native Space control is available.
- Drag any tiled icon to move its complete column. Dropping between columns
  reorders the column; dropping on another user Space moves the whole column
  while preserving stack/tab structure and member order.
- Drag a floating icon within the current Space to reorder the floating icon
  lane. Drop it on another user Space to move that window alone.
- After a 4px drag threshold, a translucent application-icon preview follows
  the pointer with its original grab offset. Tiled columns keep every member's
  overlapping geometry and paint order; floating windows preview individually.
  The preview uses a nonactivating, mouse-transparent panel and stays inside
  the gesture's owning display, including when the pointer leaves the Bar.
- Valid insertion targets animate neighboring icons out of the reserved slot.
  Hovering another user Space temporarily expands its Bar presentation and
  previews the append operation without changing native Space focus or layout.
  The opened slot and its activation region remain stable during animation.
- Preview and release use one drop target. Leaving the Bar or losing a valid
  source/target clears the insertion preview; invalid drops issue no command.
  Mouse release, cancellation and lost-button cleanup remove the drag panel.
- During animation, hit testing follows presented geometry but uses current
  target identities. Departed, fading-out windows cannot receive actions.

Space focus and window moves follow the existing
`experimental_space_control` capability. The Bar still renders in observe-only
mode when those private operations are unavailable.

The same overview actions are available outside the Bar:

```sh
spool action mission-control
spool action show-desktop
spool action bar toggle-collapse
```

Lua configuration and client scripts can use `spool.action.mission_control()`,
`spool.action.show_desktop()` and `spool.action.bar.toggle_collapse()`. They can
also be passed as functions to `spool.bind`; `spool.bind("alt+shift+space",
spool.action.bar.toggle_collapse)` is the usual way to reach the collapsed Bar
from the keyboard.

## Collapse

The Bar covers the system menu bar while expanded, so collapsing is how the
menu bar is handed back. Two chevron handles sit in the 24pt zone at each end
of the Bar — where the system menu bar's own edge items were — and appear as
soon as the pointer is anywhere on the Bar, so collapsing is discoverable
without hunting for an edge. The handle under the pointer brightens; a hidden
handle ignores clicks. Collapsing is runtime-only presentation state: it is
never written to disk and every Bar starts expanded.

- Collapse a notched display and the Bar's chrome becomes a black capsule
  merged with the notch: the notch's own width plus 24pt on each side,
  menu-bar height, opaque. It is shaped like the notch: its top edge
  **overhangs** the body by `CAPSULE_TOP_RADIUS` (12pt) on each side, so the
  black is widest along the screen edge, and a **convex fillet** of the same
  radius carries each end back down to a vertical wall. The bottom corners are
  the ordinary convex fillet too, deliberately tighter
  (`CAPSULE_BOTTOM_RADIUS`, 8pt). The alternative — a *scooped*, concave
  shoulder that carves the corner out — is kept behind `SHOULDER`, because this
  choice flipped once: the scoop leaves a small notch where it meets the screen
  edge, and the prototype's render beside a photograph of the hardware is what
  settled it. A small expand handle sits inside each end.
- The collapsed shapes paint their own black. The Bar's configured background
  colour defaults to fully transparent, because expanded its look *is* the blur
  behind it; the blur is faded out as the Bar collapses, so the chrome fills
  opaque black in proportion to how much of the blur has gone and lays the
  configured colour over that.
- Collapse a display without a notch and it becomes a 120x6pt tab flush with
  the screen top, horizontally centred, a pill: both ends round, so it reads as
  the same rounded shape the capsule is, minus the notch. Hovering grows it to
  9pt as the click affordance, and a click anywhere on it expands the Bar
  again.
- The **window never moves**. The panel is the menu-bar band for its whole
  life, and only the chrome drawn inside it morphs — that is what keeps the
  transition smooth, because moving and resizing a blurred window every frame
  makes the window server re-blur it every frame. An interrupted morph restarts
  from the frame on screen.
- While collapsed the panel still covers the band, so it ignores mouse events
  everywhere except the tab: the Apple menu, application menus and status items
  underneath keep working, and only the few points the tab occupies are ours.
- On a plain display the tab's hover is judged against the 9pt box it grows
  into, never against its current height: otherwise a tab growing under the
  pointer would drop the pointer out of hover and flip every frame. The shape
  hangs from the top edge either way, so hover only ever grows it downwards.
- Everything the Bar does not cover while collapsed is the normal macOS menu
  bar, so the Apple menu, application menus and status items work as usual.
- `spool.action.bar.toggle_collapse` acts on the display that owns the active
  Space, leaving other displays alone.

## Window Identity

The Bar has a read-only ECS projection separate from the operational window
query. Inactive Spaces retain tracked application icons when macOS temporarily
withdraws their accessibility surfaces. Tiled membership comes from each
Space's retained `LayoutStrip`; floating membership is observed for every
native Space and filtered through tracked ECS identities. Cached application
identity supplies icons without accessing withdrawn AX windows.

This does not relax focus or movement availability checks. Confirmed window
destruction removes the tracked identity and its icon; the Bar does not retain
a separate last-known snapshot of closed windows.

Cold-start discovery can leave a native surface without a tracked AX identity
after the bounded startup scan. Such surfaces remain discovery candidates, not
Bar icons. Native membership, a known application bundle, and visible geometry
alone do not establish an operable window. The Bar does not discover independent
read-only icons or activate an application as a substitute for window focus.

Once normal AX discovery admits the window, its tracked identity supplies both
the Bar icon and its operational layout role. A Space with no tracked windows
uses the empty placeholder, including before fullscreen-window discovery.
Already tracked hidden, minimized, or temporarily withdrawn windows retain their
existing lifecycle and restoration policy. A retained identity confirmed no
longer presented loses its icon and tile slot through the shared lifecycle state.
The renderer performs no extra AX scans.

## Configuration

The Bar uses the `bar` section in the active `init.lua`. Merge it into the same
`spool.setup` call as window-manager settings; a second setup call replaces the
whole configuration. The [default Lua configuration](../config/default.lua)
contains every Bar setting:

```lua
-- Spool configuration. Changes are hot-reloaded on save.
spool.setup {
  options = {},
  bar = {
    notch_side = "balanced", -- "balanced", "left", or "right" on notched screens.
    icon_size = 0, -- 0 fits the available height, up to 20pt.
    vertical_padding = 3,
    horizontal_padding = 6,
    workspace_spacing = 5,
    window_spacing = 3,
    background_color = "#00000000",
    border_color = "#FFFFFF29",
    border_width = 0,
    corner_radius = 0,
    show_shadow = false,
    selection_color = "#0A84FFFF",
    active_workspace_color = "#0A84FF33",
    inactive_workspace_color = "#80808026",
    workspace_corner_radius = 4,
    show_focus_ring = true,
    focus_ring_width = 2,
    show_workspace_labels = true,
    label_font_size = 11,
    foreground_color = "auto", -- System label color, or an RGB/RGBA hex color.
    show_mission_control = true,
    show_desktop = true,
  },
}
```

Set `bar.show_workspace_labels = false` to hide numeric Space labels. Changes
reload with the rest of Lua configuration; invalid edits preserve the last
working settings. Removing the `bar` section restores defaults. There is no
separate Bar config file or file polling in the renderer.


| Option | Default | Behavior |
| --- | --- | --- |
| `notch_side` | `"balanced"` | How the Space group is placed around the notch. `"balanced"` splits it by count, `"left"` / `"right"` keep it on that side. |
| `icon_size` | `0` | Auto fit up to 20pt, or an explicit point size. Smaller icons stay vertically centered without reducing Bar height. |
| `label_font_size` | `11` | Numeric label size (8-20pt); text is also bounded by its actual icon lane. |
| `foreground_color` | `"auto"` | System appearance's label color, or an RGB/RGBA hex color for labels and toolbar symbols. |
| `inactive_workspace_color` | `"#80808026"` | Neutral background for inactive Space groups. |
| `workspace_corner_radius` | `4` | Space-background corner radius (0-8pt). |
| `show_mission_control` | `true` | Show the Mission Control button. |
| `show_desktop` | `true` | Show the Show Desktop button. Hidden buttons release their width. |

The Bar is the menu bar band: it is exactly as wide and as tall as the menu bar
on each display, so its geometry is not configurable. `embed_in_menu_bar`,
`height`, `top_offset`, `max_width` and `screen_padding` are therefore retired;
an `init.lua` that still sets them keeps loading and the keys are ignored.

Existing spacing, background/border, shadow, selection color, focus ring and
label-visibility settings remain available. All lengths use logical points, not
physical Retina pixels. Non-finite numeric values reject the reload.

## Validation Boundary

Pure tests cover top-edge placement for 22/24/37pt menu bars, display offsets,
notch bounds, explicit-height clamping, icon centering, toolbar width reclamation,
and atomic clip changes when controls or display height change. They also cover fixed-height stack geometry, focus promotion, numeric labels,
deck paint order and exposed regions, label-width removal,
empty-content geometry, interrupted motion, per-frame clipping and animated
hit targets. ECS projection tests also cover inactive and fullscreen Spaces,
withdrawn accessibility surfaces, inactive floating windows, Space switching,
and confirmed-close cleanup while preserving operation guards.
Drag tests cover pointer/grab-offset geometry, display bounds, animated slot
stability, whole-column and floating previews, command agreement, and the real
mouse-event state path through release and cancellation. They do not create
live native panels or operate the user's windows.
Additional tests cover borderless, uniformly sized and aligned collapsed icons,
interrupted transitions between collapsed and expanded stack geometry, unchanged
single-window vertical geometry, exclusion of unresolved native surfaces until
AX discovery, floating icon/focus agreement after identity recovery, inventory
failures, and preservation of tracked identities on inactive Spaces.
Live Space switching, drag gestures, multi-display behavior and
the subjective animation/style review still require desktop acceptance; unit
tests are not a substitute for that inspection.

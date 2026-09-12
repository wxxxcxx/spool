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
not be equal. What they hug is the notch plus the handle's reach past it
(`BarSurface::keep_out`): the collapsed Bar draws a collar there, so a lane that
kept clear of the notch alone would put its label and icons under the collar and
its clicks would go to the handle instead of the Space. On a notched display the group is split by count, with the fixed
buttons counting as one item, so the notch takes the first `ceil((n + 1) / 2)`
items on its left and the rest on its right; `notch_side = "left"` or
`"right"` keeps every Space on one side instead. Spaces remain in native order
and none is ever split across the notch. Creation, deletion or native
reordering recomputes the split; window counts and focus never do. Clipped
content has no hit or drop targets.
This is still a nonactivating panel, not an
`NSStatusItem`: macOS does not reserve horizontal space for it among application
menus and status items, which is why the Bar takes that space instead.

One **Bar Handle** sits at the display's centre, and it is the Bar's only control
of its own: a small tab whose square top edge is glued to the Bar's bottom edge,
so while expanded it hangs `bar.handle_height` below the band, over the desktop
below. It is 64pt wide and 5pt tall with 2.5pt bottom corners by default on a
display without a notch; on a notched display it is the Notch's width plus the
handle's height a side, and the same below it,
because the middle of the band there is the camera housing and a tab centred
inside it would be drawn on pixels that do not exist. It is filled like the Bar
itself — menu-material glass plus `background_color`, with `border_color` and
`border_width` around it — and as the Bar leaves, black ramps in on the same
formula the Bar's own shape uses, so the resting handle is opaque black and the
Notch's ears merge with the Notch. Hovering grows it by a few points and never
changes its colour (see below).

The panel is `Stationary` to the window server: Mission Control, Exposé and Show
Desktop must not move or scale it. Without that flag the window server scales the
panel to a point while either mode is up — measurably, the panel's WindowServer
bounds collapse to 1x1 — and the Bar disappears until the mode ends. Apple's
guide makes the choice exclusive: the `Transient` flag the Bar used to set means
"float in Spaces and be hidden in Exposé", which is exactly the wrong half, and
`CanJoinAllSpaces` already asks for the other half.

The panel window is therefore the menu-bar band **plus** that handle's overhang:
the handle at rest and 30% of its height more for the room it grows into when
hovered — 5pt and 1.5pt at the default height, so 6.5pt taller than the menu bar
on every display, even while expanded. That
headroom is not optional — the view clips its own drawing, so a window sized to
the resting handle alone would slice the rounded bottom off the moment the handle
grew. Its frame never moves.

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
- By default every Space shows its columns at the width those columns need,
  whether or not macOS is showing it: the Space you are on is where focus and the
  newest layout live, but the others stay just as readable — none of their icons
  are traded away for a narrower slot — and a window in one can be clicked or
  dragged like any other (see Interaction). Set
  `bar.collapse_inactive_spaces = true` to shrink the Spaces you are not on into
  a compact deck instead.
- A collapsed Space shows up to four application icons as a compact deck, with at
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
- Expansion, icon movement, the collapse, the handle's hover growth and focus
  transitions all share one 240ms ease-out policy in `src/bar/motion.rs`
  (`motion::DURATION` and `motion::ease_out`). Nothing in the Bar overshoots: the Bar leaves through the
  screen's top edge, so a curve that passed its target would push the handle past
  the edge it is coming to rest on. Interrupted transitions begin at the last
  presented frame, and repeated snapshots do not restart motion. Content is
  clipped to its Space and to the band it rides on, so it leaves with the Bar.
- The Bar Handle answers the pointer by growing, never by changing colour: it
  gains `HandleMetrics::HOVER_GROWTH` — an eighth of its own width and a third of
  its own height, so 8pt and 1.5pt at the shipped 64x5pt shape — over the same
  240ms ease-out as every other transition, centred on itself and growing *away*
  from the edge it is glued to — the Bar's bottom edge while expanded, the
  screen's top edge once collapsed — so the join never opens and the handle never
  lifts off the Bar. A notched display's collar takes the height's share only, on
  each side: its width is mostly the Notch, so scaling that would slide its ears
  over the neighbouring Spaces, while a thickness always stays inside the gap the
  lanes keep clear. The grown rect contains the resting one, so a pointer that
  is inside the small shape is still inside the large one: growing can never drop
  the pointer out of hover and flip the handle back and forth every frame. It is
  a frame-loop transition, not a repeating pulse, so it costs nothing once it has
  settled.
- The toolbar button under the pointer keeps the other hover language: a
  repeating 1.8s ease-in-out wash of translucent white over the button's own
  shape, between 0.07 and 0.17 layer opacity. It is a Core Animation layer rather
  than per-frame drawing, because pulsing it from the frame loop would mean
  running the whole ECS at refresh rate, which costs about 45% of a core to
  animate a highlight.

### Tuning the feel

`examples/bar_handbar_prototype` (throwaway, with a copy in the working tree)
carries the current shape and motion exploration: both display kinds, both
states, a scrubber that freezes the slide at any progress, and the retired
alternatives it was picked over. `examples/bar_polish_prototype` and
`examples/bar_collapse_prototype` (kept on the throwaway `prototype/bar-polish`
and `prototype/bar-collapse` branches) explored the capsule and tab shapes this
design replaced; their settings no longer map onto any constant.

The shape the prototype settled on lives in `src/bar/placement.rs`: a
`HandleMetrics` resolved from `bar.handle_height` (default 5pt, clamped 4-24pt;
also how far the collar reaches past the Notch and how much taller than the band
the panel is) and `bar.handle_radius` (default 2.5pt, clamped to the handle's own
height). The width is not configurable: 64pt is the width the prototype picked,
and a notched display derives its collar from the Notch instead.
- Every Space keeps a slot: the strip never scrolls, and no Space is dropped to
  make room. The focused Space takes whatever the others leave, up to its own
  content width; the rest keep the width what they show needs — a collapsed Space
  its whole deck, a Space drawing its columns every column's width — so not
  collapsing a Space never costs it an icon. The Space macOS is showing is the one
  with a floor of a label and one icon, because it is the one that can scroll
  without hiding something the user asked to see. When even those widths do not
  fit, every Space takes an equal share.
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
  Mission Control submits the system app bundle to Launch Services with
  `/usr/bin/open -n`. That docklet reads its mode from `atoi(argv[1])` and falls
  back to Mission Control when it has no argument, and Launch Services does not
  deliver `--args` to it, so Show Desktop is requested by posting the system's
  Show Desktop shortcut (`F11` with the secondary-fn flag) at the HID event tap:
  the bundle cannot be asked for that mode. The launch request is reaped off the
  UI thread and failures are logged. Request acceptance does not establish
  completion: Dock notifications remain authoritative for overview entry/exit.
  The docklet's mode argument and the shortcut binding are macOS implementation
  The platform submits the system app bundle to Launch Services with `/usr/bin/open -n`
  and the corresponding mode; it never directly executes the AMFI-restricted
  bundle binary. No shell or synthesized keyboard shortcuts are used. The launch
  request is reaped off the UI thread and failures are logged. Request acceptance
  does not establish completion: Dock notifications remain authoritative for
  overview entry/exit. Mode arguments are macOS implementation details, not a
  public API contract.
- Click a window icon to focus that exact window. A window in a Space macOS is
  not showing is focused by going to it: the Space switch is submitted first and
  the window is focused once its Space is up, because focusing a window whose
  Space is not on screen does not stick. Both halves are one command,
  `window focus-in-space <window-id> <space-id>`, so a keybinding or script can
  ask for the same thing.
- A request for a window that has since moved to another Space is refused rather
  than obeyed: an unasked-for Space switch is worse than nothing happening.
- Click a collapsed Space to focus it when native Space control is available: a
  deck shows what is in a Space, it is not a set of separate targets.
- Drag any tiled icon to move its complete column, from any Space that draws its
  windows. Dropping between columns reorders the column; dropping on another user
  Space moves the whole column while preserving stack/tab structure and member
  order.
- Drag a floating icon within its Space to reorder the floating icon lane. Drop
  it on another user Space to move that window alone.
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
spool mission-control
spool show-desktop
spool bar toggle-collapse
```

Lua configuration and client scripts can use `spool.action.mission_control()`,
`spool.action.show_desktop()` and `spool.action.bar.toggle_collapse()`. They can
also be passed as functions to `spool.bind`; `spool.bind("alt+shift+space",
spool.action.bar.toggle_collapse)` is the usual way to reach the collapsed Bar
from the keyboard.

## Collapse

The Bar covers the system menu bar while expanded, so collapsing is how the
menu bar is handed back. The **Bar Handle** is the Bar's only collapse control,
in both directions and in one click: it hangs below the band while expanded, so
there is nothing to reveal first and no chevron to hunt for at the display's
edges. Collapsing is runtime-only presentation state: it is never written to
disk and every Bar starts expanded.

- Collapsing slides the whole Bar — band, content and glass — up out of the
  screen through the display's top edge, over the shared 240ms ease-out. The
  handle is what stays behind.
- On a display **without a notch** the handle rides up with the band and comes
  to rest flush with the screen's top edge: 64pt wide, `bar.handle_height` tall
  (5pt by default), square where it was glued to the Bar and rounded at the
  bottom corners by `bar.handle_radius`. It hangs the same distance below the
  menu bar before the collapse and 0pt after, and it is the same size throughout:
  the shape never grows on hover or at rest.
- On a **notched** display the handle is a collar around the Notch — the Notch's
  width plus the handle's height a side, and the same below it — and it does not
  move at all. The
  middle of the band there is the camera housing, so the pixels a centred tab
  would occupy do not exist; the collar's two ears and its chin, outside the
  housing, are what is left on screen, and the collapsed Bar reads as a slightly
  larger Notch.
- The handle's fill follows the Bar's: menu-material glass plus the configured
  `background_color`, with the configured border around it. The Bar's own
  configured background colour defaults to fully transparent, because expanded
  its look *is* the blur behind it; as the Bar leaves, opaque black ramps in
  where that blur has gone and the configured colour is laid over it, so the
  resting handle is solid black — which is also what merges the Notch's ears
  with the Notch.
- The **window never moves**. The panel is the menu-bar band plus the handle's
  window overhang (`placement::window_overhang`) for its whole life, and only what
  is drawn inside it slides:
  that is what keeps the transition smooth, because moving and resizing a
  blurred window every frame makes the window server re-blur it every frame. The
  blur is masked to the moving chrome rather than faded, so the glass leaves with
  the Bar instead of lingering over the desktop below it. An interrupted slide
  restarts from the frame on screen.
- The panel is ours only where the pointer is on the Bar's own chrome — the band
  while it is there, the handle either way — and ignores mouse events everywhere
  else. So the Apple menu, application menus and status items under a collapsed
  Bar keep working, and so does the strip beside the handle: pointing at it
  hands the click to whatever is underneath before it can be swallowed.
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

Application discovery merges `AXWindows`, `AXFocusedWindow` and `AXMainWindow`,
deduplicating identical AX endpoints. Some apps publish an empty `AXWindows`
list while their fullscreen Space is inactive but still publish its focused or
main window. All three sources pass the same identity and admission checks;
startup, lifecycle reconciliation and ownership checks use this same inventory.
A failed `AXWindows` read remains unknown, not a complete empty inventory.

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
    handle_height = 5, -- pt the handle hangs below the Bar (4-24); also the
                       -- collapsed collar's reach past a notch.
    handle_radius = 2.5, -- handle's bottom corners (0 to handle_height).
    collapse_inactive_spaces = false, -- false keeps Spaces you are not on at the
                                      -- width their columns need (one icon per
                                      -- window); true shrinks them into a compact
                                      -- deck of up to four icons instead.
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
| `handle_height` | `5` | Point size the Bar Handle hangs below the Bar (4-24pt). On a notched display it is also how far the collapsed collar reaches past the Notch, and it always sets how much taller than the band the panel window is. |
| `handle_radius` | `2.5` | The handle's bottom corners, 0 to `handle_height`. Above half the height the sides vanish and the bottom reads as a pill. Its top edge never rounds. |
| `collapse_inactive_spaces` | `false` | `true` shrinks the Spaces macOS is not showing into a compact deck of up to four icons each. `false` draws every Space's columns, each at the width it needs, so no icon of an inactive Space is traded away for a narrower slot and a window in any of them can be clicked or dragged. |

The Bar is the menu bar band: it is exactly as wide and as tall as the menu bar
on each display, so the band's own geometry is not configurable. The handle is
the one exception: `bar.handle_height` (4-24pt, default 5) and
`bar.handle_radius` (0 to the handle's height, default 2.5) are the only Bar
geometry keys there are — the width stays 64pt, and a notched display's collar
follows from the Notch. `bar.corner_radius` still rounds the band's own bottom
corners, and `bar.height` is still the retired band key: it is ignored, not the
handle. `embed_in_menu_bar`, `height`,
`top_offset`, `max_width` and `screen_padding` are retired; an `init.lua` that
still sets them keeps loading and the keys are ignored.

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

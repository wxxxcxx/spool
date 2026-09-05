# Built-in Bar

Spool renders one native AppKit Bar per physical display from the same Bevy
world that owns window layout. It is part of the `spool` process; there is no
`spool-bar` executable, socket client, or separate LaunchAgent.

## Presentation

By default the Bar is flush with the screen top and fits within that display's
menu-bar height, with a transparent background, no outer border, and no shadow.
Notched displays default to a fixed transparent spacer over the camera cutout,
with 6pt clearance on each side. Spaces remain in native order: the first half
(rounded up) is on the left, the rest on the right. Creation, deletion, or native
reordering recomputes this split; window counts, focus, and collapse never do.
Each side scrolls independently under the pointer without changing Bar height.
A Space is never split across the notch, and clipped content has no hit/drop targets.
Compact groups sit near the spacer; ordinary displays remain centered.
This is still a nonactivating panel, not an
`NSStatusItem`: macOS does not reserve horizontal space for it among application
menus and status items. Use the width limit and notch side to avoid crowded areas.

- Every native Space on the display is listed in its current macOS order.
- Each Space is clickable over its full height, including the blank padding
  above and below icons. Its background leaves 3pt gaps at the top and bottom,
  exposing the menu bar through the default transparent panel. Active Spaces use
  a subtle blue tint; others use neutral gray. Window icons retain their focus/drag behavior. The toolbar, inter-Space
  gaps, and notch spacer are not Space-switch targets.
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
  Each icon retains its expanded size and Y position; collapse/expansion moves
  icons only horizontally. Icons beyond the four-card limit fade without
  moving vertically, including when a transition is interrupted.
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
- Expansion, collapse, icon movement and focus transitions share a 240ms
  ease-out policy in `src/bar/motion.rs`. Interrupted transitions begin at the
  last presented frame, and repeated snapshots do not restart motion. Content
  is clipped to its Space and the display-bound Bar viewport.
- When the complete Space strip is wider than the display, scrolling over the
  Bar moves the horizontal viewport.

## Interaction

- Two fixed icon buttons at the left open Mission Control and toggle Show Desktop.
  Native buttons provide hover tooltips and click feedback without taking keyboard
  focus. The Space strip scrolls and clips independently to their right; neither
  button is a window-drag target. They also work in observe-only Space mode.
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
```

Lua configuration and client scripts can use `spool.action.mission_control()` and
`spool.action.show_desktop()`. They can also be passed as functions to `spool.bind`.

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

Cold-start discovery can leave a native window without a tracked AX identity
after the bounded startup scan. The Bar shows a read-only application icon for
such a surface when its native ordered-window membership, ordinary nontransparent
WindowServer layer, and known application bundle identity are available. These
icons follow tracked windows in a stable process/window-ID order, including in
collapsed decks. They do not invent tiled columns or floating status and cannot
be focused or dragged. Clicking their Space still uses the normal Space action.

Read-only candidates use the Space-local ordered-window list (`0x2`), not the
broad discovery list (`0x7`). A closed application window can retain a full-size,
normal-layer, opaque WindowServer surface in the latter list without having any
AX windows, as Calendar does. Such ordered-out surfaces are not fallback icons.
This does not require a window to be on the current screen; inactive Spaces are
still queried individually. Tracked windows retain their existing broad membership
and visibility policy, including minimized windows.

AX discovery replaces a read-only icon with the tracked window's real layout
role without duplicating it. Surfaces already identified by AX, including ignored
or retired windows, are excluded from fallback while their native IDs survive.
Fallback is rebuilt from fresh native observations on Bar updates; it disappears
on close or failed inventory/membership reads instead of retaining stale icons.
The renderer performs no extra AX scans. Missing bundle identity or uncertain
native metadata still means no fallback icon; read-only icons are not included
in operational queries or Lua window sets.

## Configuration

The Bar uses the `bar` section in the active `init.lua`. Merge it into the same
`spool.setup` call as window-manager settings; a second setup call replaces the
whole configuration. The [default Lua configuration](config/default.lua)
contains every Bar setting:

```lua
-- Spool configuration. Changes are hot-reloaded on save.
spool.setup {
  options = {},
  bar = {
    embed_in_menu_bar = true,
    height = 0, -- 0 follows each screen's menu-bar height.
    top_offset = 0,
    max_width = 0, -- 0 uses the available screen region.
    screen_padding = 12,
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
| `embed_in_menu_bar` | `true` | Constrain height and top offset to the menu-bar band. Set false for a floating Bar. |
| `height` | `0` | Auto per-display menu height; in floating mode auto means 34pt. Explicit heights are clamped to the available band (18-64pt). |
| `top_offset` | `0` | Gap below the screen top in points. Embedded mode clamps it so the Bar still fits inside the menu bar. |
| `max_width` | `0` | Auto available width, or a point limit. Small values are raised enough to retain toolbar controls; screen bounds always win. |
| `screen_padding` | `12` | Horizontal inset from the available screen/notch region. |
| `notch_side` | `"balanced"` | Count-balanced groups on both sides, or explicit `"left"` / `"right"` safe region. In balanced mode `max_width` includes the spacer and is raised when necessary to leave usable lanes. |
| `icon_size` | `0` | Auto fit up to 20pt, or an explicit point size. Smaller icons stay vertically centered without reducing Bar height. |
| `label_font_size` | `11` | Numeric label size (8-20pt); text is also bounded by its actual icon lane. |
| `foreground_color` | `"auto"` | System appearance's label color, or an RGB/RGBA hex color for labels and toolbar symbols. |
| `inactive_workspace_color` | `"#80808026"` | Neutral background for inactive Space groups. |
| `workspace_corner_radius` | `4` | Space-background corner radius (0-8pt). |
| `show_mission_control` | `true` | Show the Mission Control button. |
| `show_desktop` | `true` | Show the Show Desktop button. Hidden buttons release their width. |

Existing spacing, outer background/border, shadow, selection color, focus ring,
and label-visibility settings remain available. All lengths use logical points,
not physical Retina pixels. Non-finite numeric values reject the reload.

For a floating style, merge these values into the same `bar` table:

```lua
embed_in_menu_bar = false,
height = 34,
top_offset = 4,
background_color = "#151517E0",
border_width = 1,
corner_radius = 8,
show_shadow = true,
```

Existing `init.lua` values are not overwritten. To adopt the new embedded defaults,
remove old Bar overrides or update them from `config/default.lua`.

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
Additional tests cover borderless collapsed icons, unchanged vertical geometry
through interrupted transitions, cold-start read-only icons and their handoff
to AX discovery, inventory failures, closed/ignored-surface cleanup, and exclusion
from focus, drag and operational queries.
Live Space switching, drag gestures, multi-display behavior and
the subjective animation/style review still require desktop acceptance; unit
tests are not a substitute for that inspection.

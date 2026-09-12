# Configuration Guide

Spool uses one Lua configuration for the window manager and built-in Bar.
Configuration discovery checks these locations in order:

1. `$SPOOL_LUA` (an existing file)
2. `$HOME/.spool.lua`
3. `$XDG_CONFIG_HOME/spool/init.lua` (normally `~/.config/spool/init.lua`)

If none exists, Spool creates the [default init.lua](../config/default.lua) in the
XDG config directory without overwriting an existing script. TOML configuration
(`spool.toml`, `bar.toml`, `~/.spool`, and `SPOOL_CONFIG`) is no longer supported.
Existing TOML files are left untouched but are not read, created, or watched.

Save the active script to reload settings, Bar appearance, and keybindings.
A failed reload keeps the last working configuration. Omitted settings use
built-in defaults; removing `bar` or `spool.setup` also restores their defaults.
Builds without the `lua` feature use built-in defaults only.

Numeric settings must be finite. NaN and positive or negative infinity fail
`spool.setup` before the candidate configuration is published; a failed reload
keeps the working configuration. Finite values retain the documented ranges
and existing clamping behavior.

All sections below belong inside one `spool.setup { ... }` call.
Examples show standalone calls; merge their sections when combining them.

---

## 1. Global Options (`options`)

General behavior settings for the window manager.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `focus_follows_mouse` | Boolean | `true` | If enabled, the window under the mouse cursor will automatically gain focus. |
| `mouse_follows_focus` | Boolean | `true` | If enabled, the mouse cursor will warp to the center of the focused window when focus changes via keyboard. |
| `horizontal_mouse_warp` | Integer ``(-1, 1)`` | Off | If enabled, the mouse will warp to another screen above or below, when touching the left or right edge. The direction depends on the direction - a negative value will cause the left edge to warp to a screen above and the right edge to a screen below. This allows having horizontal positioning of displays while having them aligned in a virtual layout in macOS settings. The cursor lands at the *opposite* edge of the target display (preserving cursor flow), with the source's relative Y position. Carries pre-warp horizontal velocity to avoid a "standing start", and skips the warp when the equivalent Y has no position on the target — matching macOS's native side-by-side behavior for displays of unequal height. (inspired by https://github.com/mogenson/WarpMouse.spoon) |
| `horizontal_mouse_warp_offset` | Integer (px) | `0` | Vertical pixel offset applied to the `horizontal_mouse_warp` landing position, signed by warp direction. Positive values shift the cursor lower when warping to a display *below* (in macOS arrangement) and higher when warping to one *above*. Use to compensate for physical desk arrangement differing from the macOS arrangement (e.g. portrait monitor sitting physically higher or lower than the laptop). |
| `preset_column_widths` | Array (Float) | `[0.25, 0.33, 0.5, 0.66, 0.75, 1.0, 1.5, 2.0]` | Ratios of the screen width used by tiled `window grow width` / `window shrink width` actions and the menu bar width picker. Values above `1.0` create a horizontally scrollable oversized window. |
| `animation_speed` | Float | *None* | Speed of window animations. Comfortable range is from 8 to 20. Unset or set to a very high value to effectively disable animations. |
| `auto_center` | Boolean | `false` | Automatically center the focused window on the screen when switching focus. |
| `sliver_height` | Float (0.1–1.0) | `1.0` | Vertical ratio of off-screen windows kept visible to prevent macOS from relocating them. |
| `sliver_width` | Integer (px) | `5` | Horizontal width of off-screen windows kept visible. |
| `menubar_height` | Integer (px) | *Auto* | Manually override the detected macOS menubar height. |
| `window_hidden_ratio` | Float (0.0–1.0) | `0.0` | How much of a window can be hidden before it's forced into view on focus change. `0.0` = eager, `1.0` = lazy. |
| `window_resize_cycle` | Boolean | `true` | If disabled, tiled width grow/shrink stops at the largest/smallest preset instead of cycling back. |
| `floating_window_move_step` | Integer (px) | `20` | Distance a floating window moves for each directional `window move` action. |
| `floating_window_resize_step` | Integer (px) | `40` | Width or height change applied to a floating window for each grow/shrink action. |
| `mouse_resize_modifier` | String | *None* | If enabled allows window resizing using mouse movement. For example `cmd + shift` will allow resizing of the window when holding those keys. Proximity of the pointer to left or right window edge determines which side will be adjusted. |
| `experimental_space_control` | Boolean | `false` | Enables capability-probed private Space control. The current backend may focus a Space on the active display or move windows to a user Space; create/delete remain unavailable. This never injects into Dock and does not require disabling SIP. |
| `automatic_reconcile` | Boolean | `true` | Enables full application inventory sweeps on the one-second heartbeat and mouse-up. Disabling skips only these sweeps: frame/focus heartbeats, Space/display changes, wake, Mission Control exit, scoped notifications and explicit `reconcile-windows` requests still reconcile. |
| `space_switch_animation` | Boolean | `true` | Uses the native Mission Control animation when focusing a Space. Requires macOS's “Move left/right a space” shortcuts to be enabled. When disabled, Spool uses the instant high-velocity gesture path. |

Turning off `experimental_space_control` cancels pending follow-up Space/window
focus, including follows waiting for membership confirmation. It does not undo
native movement already submitted; Spool still observes and reconciles the
result. Reenabling control does not resume canceled follows. Platform focus
events that were already issued cannot be recalled.

Hiding or minimizing a moved window also cancels its pending follow-up focus;
showing it later does not resume that follow. Moving an already hidden or
minimized window retains that state and updates its remembered tiled destination.

An accepted explicit window selection, successful Space selection, or newer
follow supersedes older pending follows. Automatic focus restoration does not.
Canceling a follow never rolls back a native movement already submitted.

Direct window-by-ID focus, including Bar window clicks, requires a currently
observed visible owning Space. Floating or hidden windows do not bypass that
check, and enabling Space control does not change it. Bound script `focus`
retains its explicit native-activation request behavior.

---

Numeric shortcuts include both user and fullscreen Spaces in the order returned
by `spool.query_spaces()`. The same order drives bounded adjacent-Space bindings such as
`Option+[` / `Option+]` or `Ctrl+Option+Left/Right`; reaching the first or last
numbered Space is a no-op.

The example Lua configuration binds `Shift+Option+1...7` to move the focused
window, switch to the destination Space, and focus it after macOS confirms the
move. `Shift+Control+Option+1...7` keeps the original move-without-following
behavior.

---

## 2. Padding (`padding`)

Sets the margins at the edges of the screen.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `top` | Integer (px) | `0` | Padding at the top of the screen. |
| `bottom` | Integer (px) | `0` | Padding at the bottom of the screen. |
| `left` | Integer (px) | `0` | Padding at the left edge. |
| `right` | Integer (px) | `0` | Padding at the right edge. |

---

## 3. Swipe & Gestures (`swipe`)

Configure trackpad gestures and scroll-wheel window sliding.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `sensitivity` | Float (0.1–2.0) | `0.35` | Multiplier for swipe distance. |
| `deceleration` | Float (1.0–10.0) | `4.0` | Rate at which inertia slows down after a swipe. |
| `continuous` | Boolean | `true` | If `true`, the windows are allowed to fully move across the desktop, potentially exposing the empty desktop space. If `false`, the window strip will not move further than the left or right most window. This also affects the windows during keyboard focus - if `false` the left or right most windows will snap to the edge of display. |

### `swipe.gesture`
| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `fingers_count` | Integer | *None* | Number of fingers for the swipe gesture. Set to 3 or more to enable. |
| `direction` | String | `"Natural"` | Direction of movement: `"Natural"` or `"Reversed"`. |
| `vertical` | Boolean | `true` | Let vertical gestures scroll the current Space's horizontal layout strip. Disable it to leave vertical gestures to macOS. |

When `fingers_count` is omitted or set below 3, Spool does not intercept native macOS gestures. If macOS uses three-finger horizontal swipes for Spaces, prefer `swipe.scroll` with a modifier or configure a different finger count.

### `swipe.scroll`
| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `modifier` | String | `"alt"` | Modifier key(s) required to slide windows with the scroll wheel: `"alt"`, `"rcmd"`, `"ralt+cmd"`, `"lctrl+lalt+cmd"`, etc. |

---

## 4. Decorations (`decorations`)

Visual styling for workspaces, active and inactive windows.

### `decorations.inactive.dim` (Native macOS Dimming)

Spool supports native macOS window dimming. To use this mode, **only** set `opacity` (and optionally `opacity_night`). Do not set a `color`.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `opacity` | Float (-1.0 to 1.0) | `0.0` | Dimming intensity. `-1.0` is fully black, `1.0` is fully white. |
| `opacity_night` | Float (-1.0 to 1.0) | *opacity* | Dimming intensity used when macOS is in Dark Mode. |

**Example:**
```lua
spool.setup { decorations = { inactive = { dim = { opacity = -0.15, opacity_night = -0.25 } } } }
```

---

## 5. Keybindings (`spool.bind`)

Use `spool.bind(chord, spool.action.window.focus_west)` to map a chord to an action. The first argument can also be an array of chord strings.

Format: `"modifier+modifier+key"`. For example `alt+shift+j` and `cmd+j`.

Available modifiers are:
- `alt`, `lalt`, `ralt`
- `ctrl`, `lctrl`, `rctrl`
- `cmd`, `lcmd`, `rcmd`
- `shift`, `lshift`, `rshift`
- `fn`

For key names, action functions, and callback examples, see the
[Lua Scripting Guide](SCRIPTING.md#keybindings).

```lua
spool.bind("cmd+h", spool.action.window.focus_west)
spool.bind("alt+n", spool.action.window.focus_next)
spool.bind("alt+p", spool.action.window.focus_previous)
spool.bind("alt+shift+h", spool.action.window.move_west)
spool.bind("alt+minus", spool.action.window.shrink_width)
spool.bind("alt+equal", spool.action.window.grow_width)
spool.bind("alt+s", spool.action.window.toggle_stack)
spool.bind("alt+v", spool.action.window.toggle_tiled_visibility)
```

`toggle_tiled_visibility` parks the current Space's tiled windows at the screen
edge using the existing off-screen sliver projection. It does not minimize
windows or hide their applications. Columns to the left/right of the focused
tile park on the corresponding edge. The focused column (including its stacks
and tabs) parks at the nearer edge, choosing left on a tie. With floating focus,
the most recently focused eligible tile in this Space is the reference; with
no tiled focus history, each window uses its nearer edge. Directions are captured
when hiding and do not change with subsequent focus changes.
A second invocation restores the windows
parked by this action; columns, stacks and tab order remain in the layout.
Floating windows, other Spaces, and windows already hidden or minimized are
excluded. Windows opened afterward remain visible. Parked tiles are excluded
from navigation and border/dim overlays. Closing or changing a parked window's
layout ownership releases its parking state.

The equivalent CLI action is `spool space layout toggle tiled-visibility`.

### Spaces

Spool owns one `LayoutStrip` per macOS Space. macOS remains the source
of truth for Space lifecycle, order, visibility, and fullscreen Spaces. Use
Mission Control or system gestures to navigate by default. Experimental
commands are documented in
[QUERY_AND_SUBSCRIBE_FORMAT.md](QUERY_AND_SUBSCRIBE_FORMAT.md).

See [QUERY_AND_SUBSCRIBE_FORMAT.md](QUERY_AND_SUBSCRIBE_FORMAT.md) for the
resource `list`/`inspect` responses and `spool session watch` event stream.

### Displays

`window move-to-display next --follow`, `window move-to-display next --stay` and `mouse next-display` cycle
through display positions ordered by X, then Y, with display ID breaking ties.
Window commands start from the focused display; mouse commands start from the
screen containing the cursor. Up/down focus and tiled movement first use the
local layout, then select a screen in that direction, preferring horizontal
overlap and the nearest edge. Directional navigation does not wrap at the end.

An unknown/reconfiguring destination or ambiguous cursor ownership defers the
command. Mouse navigation selects a visible window on the destination when
possible; otherwise it lands at the center of that display's usable area.
These commands do not require experimental native Space control.

---

## 6. Window Rules (`windows`)

Define initial window behavior using optional AND-combined matchers. Rules sort
by descending `priority`, then ascending name. Each field uses the first explicit
value, including `false`; passthrough keys accumulate. See [Window policy](WINDOW_POLICY.md)
for admission, retries, editable defaults, and migration limits.

| Option | Type | Description |
| :--- | :--- | :--- |
| `title` | Regex | Optional window-title pattern. Omission imposes no title requirement. |
| `bundle_id` | String | Optional Bundle ID to match (e.g., `com.apple.Terminal`). |
| `role` | String | Optional exact AX role, e.g. `AXWindow`. |
| `subrole` | String | Optional exact AX subrole, e.g. `AXDialog`. |
| `priority` | Integer | Higher wins; default `0`. Shipped preferences use `-100`. |
| `floating` | Boolean | Start the tracked window outside the tiling layout. |
| `track` | Boolean | `false` excludes new candidates; `true` permits nonstandard independent AXWindow subroles and may expand process observation. Cannot force controls, menus, or attached windows into independent tracking. |
| `index` | Integer | Preferred position in the strip when spawned. |
| `dont_focus` | Boolean | Prevent the window from taking focus when spawned. |
| `width` | Positive Float | Initial width ratio for the window. Values above `1.0` create an oversized, horizontally scrollable window. |
| `grid` | String | placement for floating windows: `"cols:rows:x:y:w:h"`. Requires exactly six finite numbers, positive column/row counts, and finite resulting ratios. Malformed strings are ignored, never partially parsed. |
| `horizontal_padding` | Integer | Gaps to the left/right of this window. |
| `vertical_padding` | Integer | Gaps to the top/bottom of this window. |
| `bindings_passthrough`| Array (String)| Keys that should bypass Spool and go directly to the app. |

**Example:**
```lua
spool.setup { windows = {
  terminal = {
    title = ".*",
    bundle_id = "com.apple.Terminal",
    horizontal_padding = 5,
    bindings_passthrough = { "ctrl+h", "ctrl+l" },
  },
} }
```

### Tracking LSUIElement or non-standard windows

Regular and Accessory applications, including menu-bar utilities commonly using
`LSUIElement`, are observed by default. Independent standard, floating, dialog,
and system-dialog windows do not need `track=true`. Purpose does not inherently
force float: the editable template supplies low-priority preferences, and existing
scripts are not rewritten or auto-merged.

Use `track=true` for verified independent windows with nonstandard subroles, not
to promote AXTable/AXTextField controls. Admission applies to new candidates:
reloading `track=false` does not revoke already tracked identities. Initial layout
rules do not reclassify every existing window on reload; pending defaults use new
rules, and dynamic settings keep their existing update behavior.

```lua
spool.setup { windows = {
  btt_main = {
    bundle_id = "com.hegenberg.BetterTouchTool",
    title = "BetterTouchTool",
    track = true,
  },
  btt_screenshot = {
    bundle_id = "com.hegenberg.BetterTouchTool",
    title = "Screenshot.*",
    floating = true,
  },
} }
```

### Session Restore

Spool saves its tracked window layout and can restore it the next time it
starts. Restore is a startup-only phase: Spool loads the saved session, applies
it after initial window discovery, keeps matching open for a short grace period,
then stops consulting the saved state until the next Spool process start.

The saved session includes:

- native workspace ids
- Space ID, order hint, kind, and one layout strip per Space
- layout structure: singles, stacks, tabs, and fullscreen strips
- display/screen association
- window identity for matching across restarts

Matched eligible startup windows use the saved tiled layout before initial
`index`, `floating`, `width`, and `grid` preferences. Saved state cannot bypass
admission or known movement/resize limitations. Unmatched startup windows,
and all windows created after the restore
grace period ends, keep normal `windows` behavior.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `true` | Enables session restore on startup. |
| `startup_grace_ms` | Integer (ms) | `2000` | How long Spool keeps restore matching active after startup. This gives apps a chance to create windows shortly after Spool starts. |
| `missing_windows` | String | `"ignore"` | Behavior when a saved window is not present during restore. Currently only `"ignore"` is supported, which drops the missing window and compacts the restored layout. |

**Example:**
```lua
spool.setup { restore = {
  enabled = true,
  startup_grace_ms = 2000,
  missing_windows = "ignore",
} }
```

Restore matches windows first by stable startup identity:

- window id
- process id
- bundle id

If an application has restarted and those ids changed, Spool can use a
conservative fallback match:

- bundle id
- non-empty window title
- window identifier
- accessibility role
- accessibility subrole

Fallback matching is only used when it is unambiguous. If multiple current
windows could match the same saved window, Spool skips that saved window rather
than moving the wrong one.

If a saved app or window is missing, the default `"ignore"` policy simply drops
it from the restored layout. Empty stacks, tab groups, columns, and virtual rows
are removed. If the previously selected virtual row is removed because all of
its windows are missing, Spool selects the nearest remaining restored row for
that native workspace. If no restored row remains, the normal startup workspace
selection is kept.

For screens, Spool prefers the current macOS workspace-to-display mapping when
the workspace is already present on a display. Otherwise it restores to the
saved display id when that screen is still connected, then falls back to the
current active display, then the first available display by id. Spool does not
create placeholder displays or off-screen state for disconnected monitors.

---

## 7. Experimental Features

> [!WARNING]
> These features rely on undocumented macOS window-server APIs and have known issues. For example, overlay windows (like YouTube Picture-in-Picture) may be partially shaded, and layer ordering can behave unexpectedly. Both features are **disabled by default**. 
>
> Disabling **System Integrity Protection (SIP)** is **not required**, but without it Spool has limited control over window layering, which is the root cause of most visual edge-cases. Enable these only if you are comfortable with occasional glitches.

### Inactive Window Overlay Dimming
Another dimming option that draws a translucent overlay on every inactive window to visually emphasize the focused one. 

**Activation:** This mode is enabled by setting **both** `opacity` and `color` under `decorations.inactive.dim`. In this mode, `opacity` ranges from `0.0` to `1.0`.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `opacity` | Float (0.0 to 1.0) | `0.0` | Opacity of the dim overlay. `0.0` is transparent, `1.0` is opaque. |
| `color` | String (Hex) | `"#000000"` | Hex color for the dim overlay (default: black). |

**Example:**
```lua
spool.setup { decorations = { inactive = { dim = { opacity = 0.3, color = "#000000" } } } }
```

### Active Window Border
Draws a colored border around the currently focused window.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `false` | Enable the active window border. |
| `color` | String (Hex) | `"#FFFFFF"` | Hex color for the active window border. |
| `opacity` | Float (0.0–1.0) | `1.0` | Opacity of the active window border. |
| `width` | Float (px) | `2.0` | Width of the border in pixels. |
| `radius` | Number/String | `"auto"` | Corner radius in pixels or `"auto"` to match system. |

**Example:**
```lua
spool.setup { decorations = { active = { border = {
  enabled = true,
  color = "#89b4fa",
  width = 2.0,
  radius = 12.0,
} } } }
```

> **Tip:** You can override the `border_radius` for specific applications in the `windows` section. See [Window Rules](#6-window-rules).

## 8. Lua Scripting

The same `init.lua` can register callbacks and run actions alongside its static configuration.

All options, padding, gesture settings, and window rules documented in sections 1–7 above are available under identical names via `spool.setup{...}`. Keybindings are declared separately with `spool.bind` so they can refer directly to action functions.

In addition to static configuration, Lua scripting allows:
- **Event Hooks (`spool.on`)**: React to window creation (`window_spawned`), focus changes, or space switches with optional filter specs or regex matchers.
- **Keybindings (`spool.bind`)**: Map hotkeys to `spool.action` functions or custom Lua callbacks.
- **State Queries (`spool.query_*`)**: Read real-time window, workspace, and display layout state without round-trip shell executions.
- **Persistent State (`spool.state`)**: Store and mutate data across reloads and daemon restarts.
- **Programmatic Layout Transformations (`ws`)**: Pure layout operations (`ws:focus`, `ws:swap`, `ws:float`, `ws:shift`, `ws:view`, etc.) for custom workflows like named scratchpads.

For complete documentation, event specifications, API reference, and examples, see the **[Lua Scripting Guide](SCRIPTING.md)**.

## 9. Bar (`bar`)

Bar styling is part of `spool.setup`, not a separate file. See [Bar Configuration](BAR.md#configuration)
for all defaults and [config/default.lua](../config/default.lua) for the generated startup script.

```lua
spool.setup {
  options = {},
  bar = { show_workspace_labels = false },
}
```

# Configuration Guide

Spool is configured via a TOML file *or* a Lua script — never both. By
default, it looks for the TOML configuration in the following locations (in
order):

1.  `$SPOOL_CONFIG` (environment variable)
2.  `$HOME/.spool`
3.  `$HOME/.spool.toml`
4.  `$XDG_CONFIG_HOME/spool/spool.toml`

The configuration is automatically reloaded when the file is saved.

If an `init.lua` exists (see [Lua Scripting Guide](./SCRIPTING.md)), it takes over
completely and none of these TOML paths are read.

---

## 1. Global Options (`[options]`)

General behavior settings for the window manager.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `focus_follows_mouse` | Boolean | `true` | If enabled, the window under the mouse cursor will automatically gain focus. |
| `mouse_follows_focus` | Boolean | `true` | If enabled, the mouse cursor will warp to the center of the focused window when focus changes via keyboard. |
| `horizontal_mouse_warp` | Integer ``(-1, 1)`` | Off | If enabled, the mouse will warp to another screen above or below, when touching the left or right edge. The direction depends on the direction - a negative value will cause the left edge to warp to a screen above and the right edge to a screen below. This allows having horizontal positioning of displays while having them aligned in a virtual layout in macOS settings. The cursor lands at the *opposite* edge of the target display (preserving cursor flow), with the source's relative Y position. Carries pre-warp horizontal velocity to avoid a "standing start", and skips the warp when the equivalent Y has no position on the target — matching macOS's native side-by-side behavior for displays of unequal height. (inspired by https://github.com/mogenson/WarpMouse.spoon) |
| `horizontal_mouse_warp_offset` | Integer (px) | `0` | Vertical pixel offset applied to the `horizontal_mouse_warp` landing position, signed by warp direction. Positive values shift the cursor lower when warping to a display *below* (in macOS arrangement) and higher when warping to one *above*. Use to compensate for physical desk arrangement differing from the macOS arrangement (e.g. portrait monitor sitting physically higher or lower than the laptop). |
| `preset_column_widths` | Array (Float) | `[0.25, 0.33, 0.5, 0.66, 0.75, 1.0, 1.5, 2.0]` | Ratios of the screen width used by the `window_resize` command and the menu bar width picker. Values above `1.0` create a horizontally scrollable oversized window. |
| `animation_speed` | Float | *None* | Speed of window animations. Comfortable range is from 8 to 20. Unset or set to a very high value to effectively disable animations. |
| `auto_center` | Boolean | `false` | Automatically center the focused window on the screen when switching focus. |
| `sliver_height` | Float (0.1–1.0) | `1.0` | Vertical ratio of off-screen windows kept visible to prevent macOS from relocating them. |
| `sliver_width` | Integer (px) | `5` | Horizontal width of off-screen windows kept visible. |
| `menubar_height` | Integer (px) | *Auto* | Manually override the detected macOS menubar height. |
| `window_hidden_ratio` | Float (0.0–1.0) | `0.0` | How much of a window can be hidden before it's forced into view on focus change. `0.0` = eager, `1.0` = lazy. |
| `window_resize_cycle` | Boolean | `true` | If disabled, `window_resize` and `window_shrink` stop at the largest/smallest preset instead of cycling back. |
| `mouse_resize_modifier` | String | *None* | If enabled allows window resizing using mouse movement. For example `cmd + shift` will allow resizing of the window when holding those keys. Proximity of the pointer to left or right window edge determines which side will be adjusted. |
| `disable_native_tabs` | Boolean | `false` | If enabled, Spool will not auto-merge a newly-spawned window into a tab group with an existing same-app sibling that shares its frame. Use this if you find unrelated windows being grouped together. |
| `experimental_space_control` | Boolean | `false` | Enables capability-probed private Space control. The current backend may focus a Space on the active display or move windows to a user Space; create/delete remain unavailable. This never injects into Dock and does not require disabling SIP. |
| `space_switch_animation` | Boolean | `true` | Uses the native Mission Control animation when focusing a Space. Requires macOS's “Move left/right a space” shortcuts to be enabled. When disabled, Spool uses the instant high-velocity gesture path. |

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

## 2. Padding (`[padding]`)

Sets the margins at the edges of the screen.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `top` | Integer (px) | `0` | Padding at the top of the screen. |
| `bottom` | Integer (px) | `0` | Padding at the bottom of the screen. |
| `left` | Integer (px) | `0` | Padding at the left edge. |
| `right` | Integer (px) | `0` | Padding at the right edge. |

---

## 3. Swipe & Gestures (`[swipe]`)

Configure trackpad gestures and scroll-wheel window sliding.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `sensitivity` | Float (0.1–2.0) | `0.35` | Multiplier for swipe distance. |
| `deceleration` | Float (1.0–10.0) | `4.0` | Rate at which inertia slows down after a swipe. |
| `continuous` | Boolean | `true` | If `true`, the windows are allowed to fully move across the desktop, potentially exposing the empty desktop space. If `false`, the window strip will not move further than the left or right most window. This also affects the windows during keyboard focus - if `false` the left or right most windows will snap to the edge of display. |

### `[swipe.gesture]`
| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `fingers_count` | Integer | *None* | Number of fingers for the swipe gesture. Set to 3 or more to enable. |
| `direction` | String | `"Natural"` | Direction of movement: `"Natural"` or `"Reversed"`. |
| `vertical` | Boolean | `true` | Let vertical gestures scroll the current Space's horizontal layout strip. Disable it to leave vertical gestures to macOS. |

When `fingers_count` is omitted or set below 3, Spool does not intercept native macOS gestures. If macOS uses three-finger horizontal swipes for Spaces, prefer `[swipe.scroll]` with a modifier or configure a different finger count.

### `[swipe.scroll]`
| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `modifier` | String | `"alt"` | Modifier key(s) required to slide windows with the scroll wheel: `"alt"`, `"rcmd"`, `"ralt + cmd"`, `"lctrl + lalt + cmd"`, etc. |

---

## 4. Decorations (`[decorations]`)

Visual styling for workspaces, active and inactive windows.

### `[decorations.inactive.dim] (Native macOS Dimming)`

Spool supports native macOS window dimming. To use this mode, **only** set `opacity` (and optionally `opacity_night`). Do not set a `color`.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `opacity` | Float (-1.0 to 1.0) | `0.0` | Dimming intensity. `-1.0` is fully black, `1.0` is fully white. |
| `opacity_night` | Float (-1.0 to 1.0) | *opacity* | Dimming intensity used when macOS is in Dark Mode. |

**Example:**
```toml
[decorations.inactive.dim]
opacity = -0.15
opacity_night = -0.25
```

---

## 5. Keybindings (`[bindings]`)

Bindings map a key combination to an action. A binding can be a single string or an array of strings.

Format: `"[modifiers-]key"`. For example `alt + cmd - j` and `cmd - j`.

Available modifiers are:
- `alt`, `lalt`, `ralt`
- `ctrl`, `lctrl`, `rctrl`
- `cmd`, `lcmd`, `rcmd`
- `shift`, `lshift`, `rshift`
- `fn`

For a full list of parseable keys (i.e. `leftarrow`) check the source:
https://github.com/karinushka/paneru/blob/3790b01f8d65df5d9000142db7cf25f9270dcccc/src/config.rs#L1466-L1601


### Window commands

| Action | Description |
| :--- | :--- |
| `window_focus_west` / `_east` | Focus window to the left/right. |
| `window_focus_north` / `_south` | Focus window above/below. If no window exists, switches focus to the display in that direction. |
| `window_focus_first` / `_last` | Jump to the start/end of the strip. |
| `window_focus_tiled` | Switch to a previously focused tiled window on this Space. |
| `window_focus_floating` | Switch to a previously focused floating window on this Space. |
| `window_swap_west` / `_east` | Swap current window with neighbor. |
| `window_swap_north` / `_south` | Swap current window above/below. If no window exists, moves the window to the display in that direction. |
| `window_swap_first` / `_last` | Move current window to start/end of strip. |
| `window_center` | Center the current window in the viewport. |
| `window_resize` | Cycle through preset widths (Grow). |
| `window_grow` | Alias for `window_resize`. |
| `window_shrink` | Cycle through preset widths (Shrink). |
| `window_fullwidth` | Toggle full-width mode. |
| `window_togglefloating` | Toggle between tiled and floating state. |
| `window_stack` | Stack the current window into the column on the left. |
| `window_unstack` | Pull a window out of a stack into its own column. |
| `window_equalize` | Make all windows in a stack equal height. |
| `window_balance` | Make all columns in the strip the same width as the focused window. |
| `window_nextdisplay` | Move focused window to the next monitor and follow it. |
| `window_nextdisplaysend` | Move focused window to the next monitor but stay on current. |
| `mouse_nextdisplay` | Warp mouse cursor to the next monitor. |
| `window_snap` | Snap an overflowing window into the viewport. |
| `window_raise_floating` | Make the floating windows layer visible on the current workspace. |
| `window_togglefloatlayer` | Selectively move the floating windows in front or behind of the workspace windows. |
| `quit` | Exit Spool. |
| `restart` | Restart the Spool service (`spool restart`). |

**Example:**
```toml
[bindings]
window_focus_west = "cmd - h"
window_resize = ["alt - r", "ctrl - r"]
```

### Spaces

Spool owns one `LayoutStrip` per macOS Space. macOS remains the source
of truth for Space lifecycle, order, visibility, and fullscreen Spaces. Use
Mission Control or system gestures to navigate by default. Experimental
commands are documented in
[QUERY_AND_SUBSCRIBE_FORMAT.md](QUERY_AND_SUBSCRIBE_FORMAT.md).

See [QUERY_AND_SUBSCRIBE_FORMAT.md](QUERY_AND_SUBSCRIBE_FORMAT.md) for the
structured `spool query` responses and `spool subscribe` event stream.

---

## 6. Window Rules (`[windows]`)

Define specific behaviors for applications based on their Title or Bundle ID.

| Option | Type | Description |
| :--- | :--- | :--- |
| `title` | Regex | **(Required)** Regex pattern to match the window title. |
| `bundle_id` | String | Optional Bundle ID to match (e.g., `com.apple.Terminal`). |
| `floating` | Boolean | Start the tracked window outside the tiling layout. |
| `track` | Boolean | Force Spool to track this app/window even if macOS reports the app as unobservable or the window has a non-standard role/subrole. |
| `index` | Integer | Preferred position in the strip when spawned. |
| `dont_focus` | Boolean | Prevent the window from taking focus when spawned. |
| `width` | Positive Float | Initial width ratio for the window. Values above `1.0` create an oversized, horizontally scrollable window. |
| `grid` | String | placement for floating windows: `"cols:rows:x:y:w:h"`. |
| `horizontal_padding` | Integer | Gaps to the left/right of this window. |
| `vertical_padding` | Integer | Gaps to the top/bottom of this window. |
| `bindings_passthrough`| Array (String)| Keys that should bypass Spool and go directly to the app. |

**Example:**
```toml
[windows.terminal]
title = ".*"
bundle_id = "com.apple.Terminal"
horizontal_padding = 5
bindings_passthrough = ["ctrl-h", "ctrl-l"]
```

### Tracking LSUIElement or non-standard windows

Some applications (e.g., BetterTouchTool, ProtonVPN) are flagged as background apps
(`LSUIElement`) or expose windows with unusual accessibility roles such as `AXTable`
or `AXTextField`. Spool normally ignores these processes and windows. Use `track = true`
to opt in and forcibly track the matching windows.

```toml
[windows.btt_main]
bundle_id = "com.hegenberg.BetterTouchTool"
title = "BetterTouchTool"
track = true

[windows.btt_screenshot]
bundle_id = "com.hegenberg.BetterTouchTool"
title = "Screenshot.*"
floating = true
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

Matched startup windows use the saved session before static `[windows]` rules.
That means saved layout, Space, display, and tiled/floating state
win over configured `index`, `floating`, `width`, and `grid` rules during
restore. Unmatched startup windows, and all windows created after the restore
grace period ends, keep normal `[windows]` behavior.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `true` | Enables session restore on startup. |
| `startup_grace_ms` | Integer (ms) | `2000` | How long Spool keeps restore matching active after startup. This gives apps a chance to create windows shortly after Spool starts. |
| `missing_windows` | String | `"ignore"` | Behavior when a saved window is not present during restore. Currently only `"ignore"` is supported, which drops the missing window and compacts the restored layout. |

**Example:**
```toml
[restore]
enabled = true
startup_grace_ms = 2000
missing_windows = "ignore"
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

**Activation:** This mode is enabled by setting **both** `opacity` and `color` under `[decorations.inactive.dim]`. In this mode, `opacity` ranges from `0.0` to `1.0`.

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `opacity` | Float (0.0 to 1.0) | `0.0` | Opacity of the dim overlay. `0.0` is transparent, `1.0` is opaque. |
| `color` | String (Hex) | `"#000000"` | Hex color for the dim overlay (default: black). |

**Example:**
```toml
[decorations.inactive.dim]
opacity = 0.3
color = "#000000"
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
```toml
[decorations.active.border]
enabled = true
color = "#89b4fa"
width = 2.0
radius = 12.0
```

> **Tip:** You can override the `border_radius` for specific applications in the `[windows]` section. See [Window Rules](#6-window-rules).

## 8. Lua Scripting

Spool embeds a Lua runtime that allows full configuration via `init.lua`, replacing `spool.toml` entirely. When a Lua configuration or script exists (`$SPOOL_LUA`, `$HOME/.spool.lua`, or `$XDG_CONFIG_HOME/spool/init.lua`), it takes over completely and no TOML config is read.

All options, padding, gesture settings, window rules, and keybindings documented in sections 1–7 above are available under identical names via `spool.setup{...}`.

In addition to static configuration, Lua scripting allows:
- **Event Hooks (`spool.on`)**: React to window creation (`window_spawned`), focus changes, or space switches with optional filter specs or regex matchers.
- **Keybinding Callbacks (`spool.bind`)**: Map hotkeys to custom Lua callbacks or commands.
- **State Queries (`spool.query_*`)**: Read real-time window, workspace, and display layout state without round-trip shell executions.
- **Persistent State (`spool.state`)**: Store and mutate data across reloads and daemon restarts.
- **Programmatic Layout Transformations (`ws`)**: Pure layout operations (`ws:focus`, `ws:swap`, `ws:float`, `ws:shift`, `ws:view`, etc.) for custom workflows like named scratchpads.

For complete documentation, event specifications, API reference, and examples, see the **[Lua Scripting Guide](./SCRIPTING.md)**.

# Lua Scripting Guide

Spool embeds a Lua runtime, letting a script declare the entire configuration via `spool.setup{...}`, hook into window-manager events (`spool.on`), bind keys directly to action functions or Lua callbacks (`spool.bind`), query state, persist data across reloads, and programmatically manipulate window sets.

This guide distinguishes two kinds of script:

- A **configuration script** is the long-lived `init.lua` described below. It
  declares configuration and registers callbacks inside the daemon's Lua
  worker.
- A **client script** is an on-demand program run with `spool script`. It gets
  the socket-backed query, state, command, window-set, and subscription
  interfaces, but not `setup`, `bind`, or `on`. Each invocation uses a fresh
  Lua runtime in the CLI process, so it cannot block or mutate the
  configuration runtime. The command is available in Lua-enabled builds,
  including the default build.

```shell
spool script task.lua -- first-argument second-argument
spool script -e 'print(spool.query_active().focused_window_title)'
printf 'print(spool.state.get("mode"))' | spool script -
```

Arguments after `--` are available as `arg[1]`, `arg[2]`, and so on; `arg[0]`
is the file name, `-e`, or `-`. Scripts control stdout themselves with
`print`. The global `spool` and `require("spool")` return the same client
module.

---

## 1. Getting Started

### Script Locations

By default, Spool looks for a Lua script in the following locations (in order):

1. `$SPOOL_LUA` (environment variable)
2. `$HOME/.spool.lua`
3. `$XDG_CONFIG_HOME/spool/init.lua`

### One Configuration

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
Keypresses captured under a retired configuration are discarded if they reach
the worker after a successful reload; they cannot invoke a replacement binding.
Builds without the `lua` feature use built-in defaults only.

```lua
spool.on("window_focused", function(event, ws)
  spool.run("window balance")
end)

spool.bind("alt+j", spool.action.window.focus_east)
```

---

## 2. Configuration from Lua (`spool.setup`)

`spool.setup{...}` declares the whole configuration: `options`, `bar`, `padding`, `swipe`, `decorations`, `restore`, and `windows`. Merge all sections into one call; a later call replaces the earlier table.

```lua
spool.setup {
  options = {
    focus_follows_mouse = true,
    sliver_width = 5,
    animation_speed = 12.0,   -- write floats with a decimal point
  },
  bar = { show_workspace_labels = true, height = 0 },
  padding = { top = 10, bottom = 10, left = 8, right = 8 },
  swipe = { sensitivity = 0.4, scroll = { modifier = "alt" } },
  decorations = {
    active = { border = { enabled = true, color = "#89b4fa", width = 2.0 } },
  },
  restore = { enabled = true, startup_grace_ms = 2000 },
  windows = {
    -- keys are just rule names; `title` is a required regex
    term = { title = "kitty", floating = true, bindings_passthrough = { "ctrl+alt+h" } },
  },
}
```

### Keybindings

`spool.bind(chord, handler)` accepts plus-separated chords such as `alt+h`. Put the chord first and pass either a function from `spool.action` or a custom Lua callback. A chord array binds every listed chord to the same handler:

```lua
spool.bind("alt+h", spool.action.window.focus_west)
spool.bind({ "alt+k", "alt+pageup" }, spool.action.window.focus_north)
spool.bind("alt+shift+minus", spool.action.window.shrink_height)

spool.bind("alt+3", function(ws)
  return ws:view(spool.query_spaces()[3].space_id)
end)
```

Action functions live under the singular `spool.action` namespace. The main groups are `spool.action.window`, `spool.action.space`, and `spool.action.mouse`; lifecycle actions such as `spool.action.quit` and `spool.action.restart` live directly on it. Command strings remain available through the explicit `spool.run(...)` escape hatch, not as bind handlers. `spool.setup.bindings` is intentionally unsupported.

### Reloading

Saving `init.lua` reloads the whole configuration, including Bar styling, menubar
height, and passthrough rules. Failed reloads preserve the last working runtime
and configuration. Missing sections use defaults; no TOML fallback exists.

**Notes:**
- Float-valued options (`animation_speed`, border `width`/`opacity`, window `width`, …) should be written with a decimal point (`12.0`, not `12`).
- A successful reload that removes a previous `spool.setup` call restores built-in defaults. Removing only `bar` restores default Bar styling.
- With Nix modules, set `services.spool.config` to this `init.lua` (Lua source or a path). See [`docs/NIX.md`](NIX.md).

---

## 3. Event Handling (`spool.on`)

`spool.on` registers callback functions that execute when window-manager events occur.

### Registration Syntax

`spool.on` accepts 2 or 3 arguments:

```lua
spool.on(event_name, [filter,] handler)
```

- `event_name` (string): The event to listen for.
- `filter` (optional table or function): Filter criteria. Only events matching the filter will trigger the handler. Non-matching events avoid cross-thread state queries and Lua execution.
- `handler` (function): The callback function, receiving `(event, workspace)`.

```lua
-- Unfiltered handler:
spool.on("window_focused", function(event, ws)
  spool.log("Focused window: " .. tostring(event.window_id))
end)

-- Filtered handler with table spec:
spool.on("window_spawned", { bundle = "libreoffice" }, function(event, ws)
  if event.frame.width < 400 or event.frame.height < 400 then
    return ws:float(event.window_id)
  end
end)

-- Filtered handler with spool.match:
spool.on("window_spawned", spool.match{ app = "Ghostty" }, function(event, ws)
  spool.log("Spawned Ghostty window")
end)
```

### Supported Events

| Event Name | Description | Event Payload Fields |
| --- | --- | --- |
| `window_spawned` | A window was fully spawned and initialized in Spool | `type`, `window_id`, `pid`, `app_name`, `bundle_id`, `title`, `frame` (`{x, y, width, height}`), `floating` |
| `window_focused` | A window gained focus | `type`, `window_id` |
| `window_destroyed` | A window was closed / destroyed | `type`, `window_id` |
| `window_moved` | A window was moved | `type`, `window_id` |
| `window_resized` | A window was resized | `type`, `window_id` |
| `window_minimized` | A window was minimized | `type`, `window_id` |
| `window_deminimized` | A window was un-minimized | `type`, `window_id` |
| `window_title_changed` | A window's title changed | `type`, `window_id` |
| `application_activated` | An application was activated | `type`, `pid` |
| `application_deactivated` | An application was deactivated | `type`, `pid` |
| `application_visible` | An application became visible | `type`, `pid` |
| `application_hidden` | An application became hidden | `type`, `pid` |
| `mouse_down` / `mouse_up` / `mouse_dragged` / `mouse_moved` | Mouse actions | `type`, `x`, `y`, `modifiers` |
| `space_changed` | Active workspace changed | `type` |
| `space_created` / `space_destroyed` | Native macOS Space added or removed | `type`, `space_id` |

---

## 4. Querying State

Inside a `spool.on` handler or a `spool.bind` callback, the script can read the same state documents `spool query …` returns — no round trip, no `io.popen`:

```lua
spool.on("window_focused", function(event, ws)
  for _, window in ipairs(spool.query_on_screen()) do  -- actually visible
    spool.log(window.app_name .. ": " .. window.title)
  end

  local active = spool.query_active()
  spool.flash("Space " .. tostring(active.space_id))
end)
```

| Function | Returns |
| --- | --- |
| `spool.query(kind)` | the raw JSON string, `kind` defaulting to `"state"` |
| `spool.query_json(kind)` | the same document, decoded into a table |
| `spool.query_state()` | the complete state document |
| `spool.query_active()` | the active display, workspace and focused window |
| `spool.query_spaces()` | native macOS Spaces and their tracked windows |
| `spool.query_on_screen()` | the windows currently visible |

These are spelled exactly as in the loadable client module (`require("spool")`, see [`crates/lua`](../crates/lua)), so a helper that reads state works unchanged in either host. The payloads are documented in [`docs/QUERY_AND_SUBSCRIBE_FORMAT.md`](QUERY_AND_SUBSCRIBE_FORMAT.md).

State is gathered on demand and at most once per callback, so handlers that never query cost nothing extra. Outside a callback there is no window-manager state to read, so calling one of these at script top level raises an error; call them inside a handler or keybinding callback.

Handlers from the same input batch share an extraction. A later input batch
gets its own snapshot even if an earlier callback is still waiting on a query,
script-state write, or external command. A resumed callback keeps its original
snapshot; a long-running callback does not freeze state for later inputs.

After a successful reload, callbacks already in flight may finish their queued
actions, but only the installed runtime can update event-handler availability.

`spool.flash(message[, seconds])` defaults to two seconds. Its duration must be
finite, non-negative, and representable by the host timer; invalid values raise
a Lua error before any message is queued. Zero is accepted as an immediate expiry.

---

## 5. Persistent State (`spool.state`)

A handler that wants to remember something — which window is the scratchpad, what was focused a moment ago, how many times something has happened — cannot keep it in a Lua global. Saving `init.lua` rebuilds the interpreter, and every global goes with it. `spool.state` is the store that survives reloads and daemon restarts.

```lua
spool.state.set("pads.term", 4213)     -- any JSON-shaped value
spool.state.get("pads.term")           -- 4213, or nil
spool.state.set("pads.term", nil)      -- nil removes the key

spool.state.mutate("count", function(n) return (n or 0) + 1 end)
```

| Function | Description |
| --- | --- |
| `spool.state.get(key)` | Returns the stored value, or `nil` |
| `spool.state.set(key, value)` | Stores a value; passing `nil` removes the key |
| `spool.state.mutate(key, fn)` | Passes the current value to `fn` and atomically stores what it returns |

Reach for `mutate` whenever the new value depends on the old one. It reads, runs your function, and stores the result only if the value is still what it read; if something else modified it first, `mutate` re-runs your function against the new value.

Keys are plain strings; values can be strings, numbers, booleans, or JSON-shaped tables. The store is saved in `$XDG_STATE_HOME/spool/script-state.json`. A client script reads and writes the same store:

```shell
spool script -e 'print(spool.state.get("pads.term"))'
spool script -e 'spool.state.set("mode", "compact")'
```

Keys must be nonempty and at most 512 bytes. Numbers must be finite. Values may
contain at most 62 nested list/map containers, including empty containers, and
the store's compact JSON encoding may not exceed 1 MiB. Writes that violate
these limits fail without changing the store. Loading saved state applies the
same constraints so an accepted value remains readable after restart.

---

## 6. Programmatic Window Management

Handlers are given a **window set** (`ws`): the whole layout — displays, native Spaces, columns, and the windows in them — as a value you can transform. It is modeled on xmonad's `StackSet`, and it is *pure*: every operation returns a **new** window set rather than changing the one you were given, and nothing touches a real window until you **return** it.

```lua
spool.bind("alt+h",       function(ws) return ws:focus(ws:west(ws:focused())) end)
spool.bind("alt+shift+h", function(ws) return ws:swap(ws:focused(), ws:west(ws:focused())) end)
spool.bind("alt+3", function(ws)
  return ws:view(spool.query_spaces()[3].space_id)
end)
```

Because the window set is pure:

```lua
spool.bind("alt+b", function(ws)
  local tidied = ws:width(ws:focused(), 0.6)   -- computed, not applied
  if #ws:columns() < 3 then
    return                                     -- returning nothing changes nothing
  end
  return tidied                                -- only what you return commits
end)
```

A handler that raises partway through changes nothing either, because it never returned anything. You can branch, compute candidate layouts, and return the chosen one.

`spool.windows(fn)` is the same contract for use partway through a handler: it hands `fn` the window set and commits what it gives back.

### Reading Layout State

| Method | Returns |
| --- | --- |
| `ws:focused()` | ID of the focused window, or `nil` |
| `ws:windows()` | Every window, as records |
| `ws:window(id)` | One window record |
| `ws:find(pred)` / `ws:filter(pred)` | The first / all windows matching a predicate |
| `ws:current()` | Stable ID of the focused native Space, or `nil` when unknown or ambiguous |
| `ws:spaces()` | Stable IDs of all known native Spaces |
| `ws:space_windows(space_id)` | The windows on a Space |
| `ws:columns([space_id])` | The columns of a Space, each a list of window IDs |
| `ws:column_of(id)` / `ws:space_of(id)` | The column index / Space ID a window is on |
| `ws:display_of(id)` | The display a window is on: `{ id, active, x, y, width, height }` |
| `ws:east(id)` / `ws:west(id)` | The window one column over |
| `ws:next(id)` / `ws:prev(id)` | The next/previous window, wrapping |

A window record contains `id`, `app_name`, `bundle_id`, `title`, `frame`, `floating`, `visible` and `focused`.

Each display can have a visible Space, but only one display holds global focus.
`ws:current()` requires exactly one active display and one observed visible Space
on that display. It does not fall back to the first display when the observation
is unavailable. In that case, `ws:columns()` returns an empty list; an explicit
Space ID can still address its retained layout.

`spool.match{ app = …, bundle = …, title = …, floating = … }` builds a compiled predicate; `app`, `bundle` and `title` are regular expressions.

### Transforming Layout State

Each method returns a new window set:
- `ws:focus(id)`
- `ws:swap(a, b)`
- `ws:shift(id, space_id[, follow])`
- `ws:view(space_id)`
- `ws:float(id[, rect])`
- `ws:sink(id)`
- `ws:width(id, ratio)`
- `ws:stack(id, onto)`
- `ws:tab(id, onto)`
- `ws:unstack(id)`

`ws:stack(id, onto)` targets the named column in the same native Space; it does
not mean "stack onto the column to the left". It moves the named stack item,
keeping an existing native tab group together. Missing, floating, self, and
same-column targets do not rearrange the strip. `ws:unstack(id)` splits a stack
item next to its source column without changing native Space membership.

`ws:swap(a, b)` exchanges named entries within the same native Space, including
entries in one stack. Unrelated stacked siblings stay in place. During replay,
existing native tab groups move as one item, and incoming items receive the
destination column's width. A swap does not change focus or move windows between
native Spaces; missing, floating, and fullscreen endpoints are skipped.

Snapshots preserve native tab groups even inside vertical stacks. Chained
`swap`, `stack`, and `unstack` predictions move those groups as whole entries,
matching replay. `ws:columns()` still returns flat lists of window IDs in layout
order. Binary clients and the daemon must both use local IPC protocol version 4;
versions 2 (flat snapshots) and 3 (unbound operations) are rejected explicitly.

Returned operations retain the original snapshot's daemon-session, ECS-entity,
and native-incarnation bindings. A closed/replaced window is not targeted just
because a new window has the same numeric ID. Both named endpoints and affected
native tab/column members are checked at replay. Each stale operation is skipped
independently; later operations on unchanged windows can still run. Operations
for IDs that were not tracked at capture are recorded, but cannot bind to a
window that appears later. A new daemon session invalidates the whole old plan,
including Space-only `view` requests.

Cross-Space operations keep their bindings while queued and check again before
the native command is submitted. A move is skipped when an associated window
is replaced or was not tracked in the original snapshot. These checks prevent
identity reuse; they do not lock the layout while a script runs or guarantee
that a predicted tree still matches the current layout.

For a known target, `focus` predicts its owning display and Space as active and
selects that member inside its column without reordering native tabs. `view`
predicts the target display and Space as active while preserving the visible
Spaces on other displays. A view retains focus only when the known focused
window already belongs to the target; otherwise `focused()` becomes `nil`
because the native Space request does not identify its eventual focused window.

Bound `focus` keeps its explicit native-activation behavior. It is distinct from
ordinary window-by-ID focus, which only accepts a currently observed visible
owning Space. The session/entity/incarnation check still applies to script focus;
an accepted request is not proof that macOS completed the focus or Space change.

`shift(id, space_id, true)` predicts following and focusing the moved window,
even when the window is already on that Space; the same-Space case does not
reinsert it. Without following, moving the focused window clears the predicted
focus without guessing a replacement. Missing targets or ambiguous Space IDs
leave the tree unchanged while recording the original intent. These updates do
not add synthetic focus/view operations or change the original snapshot bindings.
Hiding a Space clears its window visibility; showing one does not invent
visibility, and a moved window cannot retain its old visibility observation.

These are requested predictions, not native confirmations. Native membership
movement and Space focus are asynchronous; operations following them in a
returned plan are not guaranteed to wait for completion. Associated native-tab
movement can also differ from the single-window `shift` prediction. Take a fresh
snapshot after native confirmation when a later decision depends on the result.

Known release blocker: `ws:tab(id, onto)` still replays as vertical stacking
instead of creating a native tab group. Preserving existing groups does not
implement native tab creation, and its prediction must not be treated as a
confirmed native result.

`ws:width(id, ratio)` requests a width for the complete tiled column, using its
owner display's usable width after Dock and edge padding. It does not use the
focused display as a fallback. Nonpositive or nonfinite ratios, rounded widths
below one pixel, unrepresentable frames, and overflowing strip offsets are
rejected before changing sizes or maximize restoration state. Ordinary columns
wider than a display remain supported. Actual geometry still follows the
application's constraints and the normal frame-commit pipeline.

`ws:float(id)` takes a window out of the tiling layout and leaves it where it is. `ws:float(id, rect)` places it relative to display fractions:

```lua
ws:float(id, { x = 0.1, y = 0.05, width = 0.8, height = 0.5 })
```

Mode predictions keep the window in its original native Space and display,
including when that Space is inactive or no Space is marked active. Repeating
the current float/tile mode leaves predicted order and column structure intact.
`shift` preserves floating classification; requesting the current Space does
not extract and reinsert the window. Intent is still recorded for best-effort
replay. Configuration-dependent retile placement and final geometry remain
decisions of the live layout pipeline.

Layout mutation replay is also best-effort during native reassignment. A
retained source layout is not a writable placement: `swap`, `stack`, `unstack`,
width and frame operations are skipped if their affected members are still
owned by a move or membership-recovery barrier. Column-wide changes include
the current column's other members. Independent operations in the same plan
still run; rejected ones are not replayed automatically after confirmation.
Focus and float/tile classification retain their own admission rules.

When native ownership is known, snapshots include floating windows on inactive
Spaces and hidden floating windows. A failed complete membership scan or a
window reported in multiple Spaces leaves that floating record out of the
snapshot until ownership is known again; this does not close or untrack the
window. The `visible` flag requires current native Space visibility as well as
on-display geometry and a non-hidden, non-minimized window state.

Relative-coordinate addition saturates instead of wrapping. The frame-request
pipeline rejects a rectangle whose normalized positive dimensions overflow its
integer endpoint coordinates before changing position or size. Negative global
origins remain valid; zero or negative requested dimensions still normalize to
one pixel.

Cross-Space transforms record native Space commands. Check
`spool.query_state().capabilities` first. Private Space control is disabled by
default. When enabled, the backend supports requesting Space focus on the
currently active display and may support moving windows to a user Space.
A following move waits for membership confirmation before requesting Space
focus, then waits for target visibility before focusing the moved window.
Cross-display Space focus can be rejected even if the pure prediction selects
that display. Create/delete and the old hidden-workspace scratchpad pattern are
not provided by the observe-only core.

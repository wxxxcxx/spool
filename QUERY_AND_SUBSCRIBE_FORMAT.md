# Query and Subscribe Format

Spool exposes structured state over the same IPC channel used by `send-cmd`:
a Mach service named `com.wxxxcxx.spool`. The CLI commands below
require a running Spool daemon.

**The JSON below is what the CLI prints, not what crosses between processes.**
Requests and responses travel as typed values in a compact binary encoding
(`postcard`); `spool query` and `spool subscribe` render them as JSON because
a terminal — and `jq`, and a status bar's shell script — needs text. Anything
consuming these commands' output sees exactly the shapes documented here.

A client written in Rust can skip the JSON entirely by using the
`spool-shared-types` crate: its `wire::Request` and `wire::Response` are the
protocol, and `spool-mach-ipc` is the transport.

All query responses are a single JSON document. `subscribe` emits
line-delimited JSON, with one complete event object per line.

## Query Commands

```shell
spool query state --json
spool query virtual-workspaces --json
spool query active --json
```

`--json` is accepted for clarity and is the only output format, so it may be
omitted; callers should include it anyway, in case another format is ever added.

### `spool query state --json`

Returns the complete state document.

```json
{
  "version": 2,
  "timestamp": 1777740000,
  "active": {
    "display_id": 1,
    "native_workspace_id": 4,
    "virtual_workspace_number": 3,
    "focused_window_id": 321,
    "focused_bundle_id": "com.apple.Terminal",
    "focused_app_name": "Terminal",
    "focused_window_title": "spool"
  },
  "displays": [
    {
      "display_id": 1,
      "active": true,
      "native_workspace_id": 4,
      "virtual_workspace_number": 3
    }
  ],
  "virtual_workspaces": [
    {
      "number": 1,
      "native_workspace_id": 4,
      "display_id": 1,
      "selected": false,
      "active": false,
      "windows": []
    },
    {
      "number": 2,
      "native_workspace_id": 4,
      "display_id": 1,
      "selected": false,
      "active": false,
      "windows": []
    },
    {
      "number": 3,
      "native_workspace_id": 4,
      "display_id": 1,
      "selected": true,
      "active": true,
      "windows": [
        {
          "window_id": 321,
          "bundle_id": "com.apple.Terminal",
          "app_name": "Terminal",
          "title": "spool",
          "focused": true,
          "floating": false
        }
      ]
    }
  ]
}
```

### `spool query virtual-workspaces --json`

Returns only the `virtual_workspaces` array from the complete state document.

```json
[
  {
    "number": 1,
    "native_workspace_id": 4,
    "display_id": 1,
    "selected": false,
    "active": false,
    "windows": []
  },
  {
    "number": 2,
    "native_workspace_id": 4,
    "display_id": 1,
    "selected": false,
    "active": false,
    "windows": []
  },
  {
    "number": 3,
    "native_workspace_id": 4,
    "display_id": 1,
    "selected": true,
    "active": true,
    "windows": [
      {
        "window_id": 321,
        "bundle_id": "com.apple.Terminal",
        "app_name": "Terminal",
        "title": "spool",
        "focused": true,
        "floating": false
      }
    ]
  }
]
```

### `spool query active --json`

Returns only the active display, workspace, and focused-window state.

```json
{
  "display_id": 1,
  "native_workspace_id": 4,
  "virtual_workspace_number": 3,
  "focused_window_id": 321,
  "focused_bundle_id": "com.apple.Terminal",
  "focused_app_name": "Terminal",
  "focused_window_title": "spool"
}
```

## Fields

| Field | Type | Description |
| :--- | :--- | :--- |
| `version` | number | State document format version. Currently `2`. |
| `timestamp` | number | Unix timestamp in seconds when the response was built. |
| `active` | object | Current active display/native workspace/virtual workspace/focused window. |
| `displays` | array | Current native Space and selected Spool row for every physical display. |
| `display_id` | number or null | CoreGraphics display id for the active display, when known. |
| `native_workspace_id` | number or null | macOS Space id for the active native workspace, when known. |
| `virtual_workspace_number` | number or null | One-based Spool virtual workspace number, when known. |
| `focused_window_id` | number or null | Focused window id, when known. |
| `focused_bundle_id` | string or null | Bundle id of the focused window's app, when known. |
| `focused_app_name` | string or null | Display name of the focused window's app, when known. |
| `focused_window_title` | string or null | Title of the focused window, when known. |
| `virtual_workspaces` | array | Virtual workspace rows known to Spool. |
| `number` | number | One-based virtual workspace number. |
| `display_id` | number or null | Physical display that owns this virtual workspace row. |
| `selected` | boolean | Whether this is the remembered Spool row for its native Space. |
| `active` | boolean | Whether this row is globally active on Spool's active display. |
| `windows` | array | Managed windows in this virtual workspace row. |
| `window_id` | number | Window id. |
| `bundle_id` | string | Bundle id for the owning application, or an empty string if unknown. |
| `app_name` | string | Display name for the owning application, or an empty string if unknown. |
| `title` | string | Window title, or an empty string if unknown. |
| `focused` | boolean | Whether this window is focused. |
| `floating` | boolean | Whether this window is unmanaged/floating. |

Spool may include empty `windows` arrays for missing virtual workspace numbers
inside a native workspace so integrations can render stable numbered slots.

## Subscribe Command

```shell
spool subscribe --json
```

`subscribe` keeps its channel open and writes one JSON event per line. The stream
is intended for integrations such as SketchyBar, so it emits changes that are
useful for keeping a bar in sync: focus changes, native or virtual workspace
changes, managed window-list changes, window title changes, and display changes.
Spool coalesces duplicate internal events from the same ECS tick and skips
events whose relevant state has not changed since the last emitted event.
Consumers should parse each line independently and then call
`spool query state --json` when they need a full refresh.

### Event Types

```json
{"event":"virtual_workspace_changed","active":{"display_id":1,"native_workspace_id":4,"virtual_workspace_number":3,"focused_window_id":321,"focused_bundle_id":"com.apple.Terminal","focused_app_name":"Terminal","focused_window_title":"spool"}}
```

Emitted after native Space changes and Spool virtual workspace switches. Spool
derives this from both incoming workspace events and ECS active-workspace marker
changes, so integrations receive the event when the visible workspace state
changes.

```json
{"event":"windows_changed","virtual_workspace_number":3,"active":{"display_id":1,"native_workspace_id":4,"virtual_workspace_number":3,"focused_window_id":321,"focused_bundle_id":"com.apple.Terminal","focused_app_name":"Terminal","focused_window_title":"spool"}}
```

Emitted after managed window creation/destruction/minimize/deminimize events and
after Spool moves or sends a window between virtual workspaces. The event is
emitted only when Spool's virtual workspace/window state differs from the last
emitted `windows_changed` event.

```json
{"event":"window_focused","window_id":321,"bundle_id":"com.apple.Terminal","title":"spool","virtual_workspace_number":3}
```

Emitted when focus changes. Spool derives this from both incoming focus events
and ECS focused-window marker changes, so internally handled focus transitions
are visible to subscribers. The `window_id`, `bundle_id`, `title`, and
`virtual_workspace_number` fields are taken from the final active state for the
tick, so stale lower-level focus notifications are not forwarded with mismatched
window metadata.

```json
{"event":"window_title_changed","window_id":321,"title":"spool"}
```

Emitted when a window title changes.

```json
{"event":"display_changed","display_id":1}
```

Emitted when display configuration changes. `display_id` can be `null` when the
event is a global display-change notification and Spool cannot resolve an
active display id.

## Virtual Workspace Commands

Absolute virtual workspace selection is addressed as a window command:

```shell
spool send-cmd window virtualnum 3
spool send-cmd window virtualmovenum 3
spool send-cmd window virtualsendnum 3
```

The matching config binding names are:

```toml
[bindings]
window_virtualnum_3 = "cmd + alt - 3"
window_virtualmovenum_3 = "cmd + alt + ctrl - 3"
window_virtualsendnum_3 = "cmd + alt + shift - 3"
```

External UI clients can address a window or display without changing Spool's
keyboard-command semantics:

```shell
spool send-cmd window focusid 321
spool send-cmd workspace select 1 3
spool send-cmd window move-to-workspace 321 1 3 follow
```

# Query and Subscribe Format (v3)

Spool exposes its macOS Space model as JSON over its Mach service. A
Space's `space_id` is the identity used by commands and integrations;
`ordinal` is only its current zero-based order on one display.

```shell
spool query state --json
spool query spaces --json
spool query active --json
spool query on-screen --json
spool subscribe --json
```

## Complete state

`spool query state --json` returns:

```json
{
  "version": 3,
  "timestamp": 1787820000,
  "active": {
    "display_id": 1,
    "space_id": 42,
    "focused_window_id": 321,
    "focused_bundle_id": "com.apple.Terminal",
    "focused_app_name": "Terminal",
    "focused_window_title": "spool"
  },
  "capabilities": {
    "move_windows": false,
    "focus": false,
    "create": false,
    "delete": false
  },
  "displays": [
    {"display_id": 1, "active": true, "visible_space_id": 42}
  ],
  "spaces": [
    {
      "space_id": 42,
      "display_id": 1,
      "ordinal": 0,
      "kind": "user",
      "visible": true,
      "focused": true,
      "windows": []
    }
  ]
}
```

`kind` is `user` or `fullscreen`. Every connected display may have one visible
Space; `focused` identifies the Space on Spool's globally active display.

The capability fields are authoritative for the current process and OS. They
remain false unless `[options].experimental_space_control = true` and
the runtime private API probe succeeds. Spool never injects into Dock and does
not require SIP to be disabled.

## Partial queries

- `spaces` returns the `spaces` array.
- `active` returns the `active` object.
- `on-screen` returns visible windows, ordered left-to-right per display.

A window contains `window_id`, `bundle_id`, `app_name`, `title`, `focused`,
`floating`, `display_id`, `frame`, and `visible`.

## Events

`subscribe` prints one JSON object per line. Event names and payloads are:

```json
{"event":"space_changed","active":{"display_id":1,"space_id":42}}
{"event":"windows_changed","space_id":42,"active":{"display_id":1,"space_id":42}}
{"event":"window_focused","window_id":321,"bundle_id":"com.apple.Terminal","title":"spool","space_id":42}
{"event":"on_screen_changed","windows":[],"active":{"display_id":1,"space_id":42}}
{"event":"window_title_changed","window_id":321,"title":"new title"}
{"event":"display_changed","display_id":1}
```

Optional fields may be `null`. Consumers should ignore unknown fields and
events so minor additions remain forward compatible.

## Space commands

Use IDs obtained from `query spaces`, never ordinal positions:

```shell
spool send-cmd space focus SPACE_ID
spool send-cmd window move-to-space WINDOW_ID SPACE_ID stay
spool send-cmd window move-to-space WINDOW_ID SPACE_ID follow
spool send-cmd space create DISPLAY_ID
spool send-cmd space delete SPACE_ID
```

The CLI checks capabilities before sending these commands. In the current
backend, focus and both `move-to-space` modes can become available. Create and
delete remain unavailable. Window moves target
user Spaces only, include associated windows, and are considered complete only
after Spool reads the new membership back from macOS.

## v2 break

v3 removes `virtual_workspaces`, `virtual_workspace_number`, and the
`virtual_workspace_changed` event. Integrations must switch to `spaces`,
`space_id`, and `space_changed`. SpoolBar can read a v2 snapshot during its
upgrade transition, but the daemon emits v3 only.

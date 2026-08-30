# Focus acquisition and control in yabai and Rift

Research date: 2026-08-29

This note compares focus acquisition, loss, discovery, and control in:

- yabai at commit [`dd845723416f5fe92af49fad5ebab00369e07edd`](https://github.com/asmvik/yabai/tree/dd845723416f5fe92af49fad5ebab00369e07edd)
- Rift at commit [`74ce00dd8cb6cf15cba3ef7459af70ddff51a562`](https://github.com/acsandmann/rift/tree/74ce00dd8cb6cf15cba3ef7459af70ddff51a562)

The goal is not to copy either implementation. It is to identify the behavioral
boundary Spool should preserve when macOS focus temporarily cannot be mapped to
a managed window.

## Summary

Both projects separate observing focus from commanding focus. Neither project
uses a failed or unresolved focus observation as a reason to focus an arbitrary
managed window.

| Question | yabai | Rift |
| --- | --- | --- |
| Primary observation | AX focused-window notifications and AX queries | WindowServer key focus, Carbon activation, and AX main-window observations |
| Unknown window | Queue/discover it; do not choose another window | Request discovery; do not emit a layout-focus event |
| No resolvable window | Return/clear the cached observation | Publish no focus event or an explicit `None` |
| Focus control | Explicit request performs process activation, make-key, and AX raise | Explicit raise/focus request performs WindowServer make-key and AX raise |
| "First window" | Explicit command selector | Explicit, scoped command behavior |

## yabai

### Focus acquisition

yabai subscribes to `kAXFocusedWindowChangedNotification` and posts a
`WINDOW_FOCUSED` event containing the AX window ID. Its direct lookup reads
`kAXFocusedWindowAttribute` and returns zero if AX supplies no window:

- [`application.c`, AX notification forwarding](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/application.c#L6-L13)
- [`application.c`, focused-window query](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/application.c#L91-L100)

The live focused-window lookup requires a front application, an AX focused
window ID, and a matching tracked window. If any part is unavailable it returns
`NULL`; startup records focus only when that lookup succeeds:

- [`window_manager.c`, live focused-window lookup](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L1352-L1361)
- [`window_manager.c`, startup focus initialization](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L2758-L2764)

### Unknown and missing focus

On application activation, an AX window ID of zero clears yabai's cached focus
and returns. An unknown non-zero ID is queued for later discovery and also
returns. A focus notification for an unknown window follows the same lost-event
path:

- [`event_loop.c`, application activation](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L349-L423)
- [`event_loop.c`, focused-window event](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/event_loop.c#L636-L673)
- [`window_manager.c`, replay after discovery](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L1438-L1471)

The important policy is negative: unresolved observation does not select a
different managed window.

### Focus control

An explicit focus operation activates the process, makes the selected window
key, and performs the AX raise action:

- [`window_manager.c`, focus control](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/window_manager.c#L1293-L1335)

`first` exists as an explicit selector in command parsing, not as a periodic
fallback after observation fails:

- [`message.c`, explicit `first` selector](https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/message.c#L1039-L1045)

## Rift

### Focus acquisition

Rift coalesces WindowServer focus notifications, queries the active Space and
key-focused native window, and publishes a resolved event only when the query
returns a window. A `None` result produces no layout-focus event:

- [`window_notify.rs`, coalesced WindowServer resolution](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/window_notify.rs#L298-L334)
- [`window_server.rs`, key-focus lookup](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/sys/window_server.rs#L789-L829)

Rift explicitly models global frontmost application, WindowServer key focus,
and per-application AX main window as separate observations. WindowServer focus
becomes authoritative after it yields a result; failure to find an exact match
returns `None` rather than an arbitrary application window:

- [`main_window.rs`, focus source resolution](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/reactor/main_window.rs#L5-L116)
- [`reactor tests`, no arbitrary window on activation](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/reactor/tests.rs#L2625-L2650)

### Unknown and missing focus

If WindowServer identifies a window that Rift has not tracked, the reactor asks
the application actor to discover visible windows and returns without changing
layout focus. AX main-window changes similarly resolve the exact window and may
trigger discovery. During application activation, failure to resolve an AX main
window clears the previous value and sends `None`, avoiding stale reuse:

- [`reactor.rs`, unknown WindowServer ID](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/reactor.rs#L1149-L1167)
- [`app.rs`, AX main-window resolution](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/app.rs#L1341-L1397)
- [`app.rs`, activation without a main window](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/app.rs#L1440-L1464)

### Focus control

Rift's explicit raise path combines WindowServer make-key behavior with AX
raise. It also avoids reasserting an already focused managed window, because
doing so could pull focus back from transient system UI:

- [`app.rs`, explicit focus/raise path](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/app.rs#L1215-L1335)

Selecting a first or chosen window is scoped to an explicit command, such as
focusing a display; it is not global recovery policy:

- [`command.rs`, explicit focus commands](https://github.com/acsandmann/rift/blob/74ce00dd8cb6cf15cba3ef7459af70ddff51a562/src/actor/reactor/events/command.rs#L312-L365)

## Evidence boundary

The cited code does not contain a dedicated rule named for macOS save panels or
`AXSheet`. The modal-sheet conclusion is therefore an architectural inference:
when a transient UI element is absent from the managed-window model, both
projects preserve uncertainty or trigger discovery instead of converting that
absence into permission to focus another window.

## Implications for Spool

Spool should preserve three different concepts:

1. **Observed focus:** what macOS has confirmed, including tracked, untracked,
   unresolved, and external-application states.
2. **Requested focus:** an explicit in-flight command and its target.
3. **Navigation anchor:** the last managed window used for directional commands
   and per-Space history.

Passive focus observations may update the first concept and discovery work, but
must never issue AX focus or raise effects. A timeout should end in an unresolved
observation, not in "focus the first managed window." Only explicit commands or
a narrowly specified lifecycle policy may create a focus request.

This boundary also allows rapid directional commands to chain through the
requested target or navigation anchor without falsely publishing that target as
macOS-confirmed focus.

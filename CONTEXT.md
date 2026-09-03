# Spool Window Management

Spool tracks ordinary macOS windows and arranges them within native Spaces while leaving macOS responsible for Space topology and membership.

## Language

**Tracked Window**:
An ordinary application window represented in Spool, regardless of whether it is tiled or floating.
_Avoid_: Managed window

**Ignored Window**:
A transient or system window that Spool deliberately does not track, such as a menu or system panel.
_Avoid_: Unmanaged window

**Tiled Window**:
A tracked window that occupies a position in a Space's layout strip.
_Avoid_: Managed window

**Floating Window**:
A tracked window that belongs to a native Space but does not occupy a position in its layout strip.
_Avoid_: Unmanaged window

**Window Visibility**:
Whether a tracked window is visible, minimized, or hidden. Visibility is independent of whether the window is tiled or floating.

**Layout State**:
The Spool-owned arrangement of tiled windows, including strip membership, column structure, ordering, and logical sizes.

**Desired Window Frame**:
The final window geometry implied by the current Layout State. It is unaffected by animation progress or an unsuccessful macOS write.
_Avoid_: Layout frame, target position

**Presented Window Frame**:
The window geometry Spool is currently asking macOS to display. It converges toward the Desired Window Frame and may be between two layouts.
_Avoid_: Current frame, actual frame

**Observed Window Frame**:
The latest window geometry successfully read back from macOS. It is the physical projection used by borders, queries, and drift detection.
_Avoid_: Cached frame, expected frame

**Window Frame Reconciliation**:
The bounded process that compares Desired and Observed Window Frames and retries when macOS has not converged, while preserving application size constraints and temporary failures.

**Geometry Gesture**:
A burst of externally initiated window move or resize observations treated as one state change after a quiet period.
_Avoid_: Resize event

**Launch Window Snapshot**:
The pre-Spool geometry and exact identity of a window already present when this Spool process starts. It is session-local and distinct from persisted layout state.
_Avoid_: Window state, restore state

**Exit-Restorable Window**:
A launch window whose fullscreen state was confirmed windowed at startup and remains confirmed windowed at graceful exit.

**Exit Restoration**:
The graceful-exit rollback of eligible launch window geometry. It does not alter Native Space membership, window visibility, focus, or z-order.
_Avoid_: Exit centering

**Native Space**:
A macOS-managed desktop or fullscreen Space. macOS owns its lifecycle, order, visibility, and window membership.
_Avoid_: Workspace, virtual workspace

**State Snapshot**:
Spool's current externally readable projection of displays, Native Spaces, Tracked Windows, focus, visibility, and observed geometry. It is a point-in-time value, not an event history.
_Avoid_: Event state, Lua state

**State Change Notification**:
A public, possibly coalesced notification that part of the State Snapshot changed. Consumers use it as an invalidation signal and obtain a fresh State Snapshot whenever exact current state matters.
_Avoid_: Raw event, event log

**Script State**:
A script-owned persistent key-value store. It remembers script choices across reloads and restarts but is not authoritative window or layout state.
_Avoid_: Window state, current state

**Configuration Script**:
The long-lived Lua program that declares Spool configuration and registers event handlers and keybindings.
_Avoid_: Client script

**Client Script**:
An on-demand Lua program that queries or controls a running Spool instance and ends when that invocation completes. It cannot register configuration handlers or keybindings.
_Avoid_: Configuration script

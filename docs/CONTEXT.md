# Spool Window Management

Spool tracks ordinary macOS windows and arranges them within native Spaces while leaving macOS responsible for Space topology and membership.

## Language

**Tracked Window**:
An ordinary application window represented in Spool, regardless of whether it is tiled or floating.
_Avoid_: Managed window

**Ignored Window**:
A transient or system window that Spool deliberately does not track, such as a menu or system panel.
_Avoid_: Unmanaged window

**Unresolved Surface**:
A native WindowServer surface without an admitted AX window identity. It remains
a discovery candidate, not a Bar icon, floating window, or focus target. Discovery
must establish the identity before presentation and operations can include it.

**Tiled Window**:
A tracked window that occupies a position in a Space's layout strip.
_Avoid_: Managed window

**Floating Window**:
A tracked window that belongs to a native Space but does not occupy a position in its layout strip.
_Avoid_: Unmanaged window

**Window Visibility**:
Whether a tracked window is visible, minimized, or hidden. Visibility is independent of whether the window is tiled or floating.

**Window Admission**:
The decision to track an independent application window, ignore a non-window or excluded surface, or defer while its identity evidence is incomplete. Fallback admission carries an initial floating preference, but does not determine layout capability or override explicit placement choices.

**Fallback Admission**:
Conservative admission of a nonstandard AX window using independent parent, valid geometry, visible normal/floating-layer surface, and close/minimize-button evidence. Its initial floating preference is generic, not a Quick Look or application-name rule; the evidence is not a liveness requirement for an already tracked identity.

**Layout Capability**:
The operations the current backend can perform on a window, distinguished as supported, unsupported, or unknown. Capability does not describe the window's purpose.

**Layout Preference**:
A default or user-specified choice of tiled or floating behavior. Explicit choices override the fallback-admission default. A preference cannot create missing capabilities.

**Deferred Tile Request**:
A retained request to tile after temporary capability or native-state uncertainty clears, distinct from a completed choice to float.

**Layout State**:
The Spool-owned arrangement of tiled windows, including strip membership, column structure, ordering, and logical sizes.

**Navigable Layout**:
A read-only projection of a Space's retained layout containing available, visible tiled identities, with stack and tab structure preserved. Directional, first/last/numeric, and next/previous tiled focus use this projection; retained unavailable identities are restoration data, not navigation targets. Projection never deletes or reorders the original layout.

**Retained Presentation Slot**:
A temporarily inaccessible identity may keep its layout structure for recovery without occupying a tile or Bar icon. Lifecycle reconciliation excludes that presentation slot when a complete AX inventory omits the identity and its uniquely known, visible user Space no longer presents it. Unknown queries, inactive Spaces, and native transitions do not establish that absence. Bar and tiling consume the same exclusion state; renewed AX availability restores the original slot.

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
A live, accessible tracked window confirmed windowed at graceful exit. An eligible launch snapshot supplies its preferred frame; otherwise its current frame is used.

**Exit Restoration**:
The graceful-exit restoration of eligible launch geometry, constrained to a currently available display. Windows without a launch snapshot also have their current geometry constrained. Oversized windows are resized to fit when the application permits; refused writes are logged and retried at most once to correct the origin. It does not alter Native Space membership, window visibility, focus, or z-order.
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

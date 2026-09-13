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

**Independent Window**:
An application-owned outer frame represented by one ordinary layout entity.
Native tab count, order, selection and internal layout belong to the application.
The backend may retain native chrome to resolve a changing AX control target;
reconciliation replaces that target on the same entity without adding a column.
A one-to-one publication withdrawal may bootstrap continuity only with matching
physical geometry, live process ownership and same-Space presentation evidence.
Geometry or onscreen visibility alone is insufficient.

**Legacy Tab Layout**:
`Column::Tabs` and `StackItem::Tabs` remain supported for existing layout state.
Their first eligible member represents the item; inactive members are not
independently moved. New native window discovery does not create these groups.

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

**Declarative Window Management**:
Window management in which explicit desired state expresses intent and native observations establish what has actually been realized. A valid desired arrangement remains meaningful while its Space is invisible or its realization is temporarily blocked.

**Window Management Intent**:
The latest accepted choice of arrangement or target, distinct from the outcome currently permitted by native constraints. A constrained outcome does not itself replace that choice.

**Effective Layout Target**:
The layout outcome derived from the latest Window Management Intent under evidenced, currently applicable constraints. It is a derived target, not an independently authored choice or an observed fact.

**Constrained Realization**:
A layout outcome that meets its Effective Layout Target while an applicable constraint prevents full satisfaction of the original Window Management Intent.

**Space Focus Preference**:
A Native Space's preferred focus target, covering tiled and floating windows. It is distinct from a request to activate that Space or window now.

**Logical Navigation Selection**:
The latest accepted navigation choice within a Native Space, which may precede native focus confirmation.

**Space Focus Indication**:
The visible indication of a Native Space's retained focus choice, independent of whether that Space is globally active. It does not assert that the selected window currently receives keyboard input.

**Confirmed Focus History**:
A Native Space's record of windows whose focus was confirmed by native observation. An accepted but unconfirmed navigation choice is not a confirmed visit.

**Activation Intent**:
The current request to activate a particular target in the Desktop Session. It is distinct from actual keyboard focus and from each Space's retained preference.

**Window Placement Intent**:
A tracked window's desired Native Space association, distinct from its observed native membership.

**External Adjustment**:
A stable external change accepted as a new Window Management Intent under the applicable domain policy. Acceptance does not establish whether a person or an application caused the change.

**External Focus Yield**:
The termination of an activation request after fresh evidence establishes stable conflicting focus under the focus policy. It preserves the unfulfilled request's outcome without treating the other window as successful completion of that request.

**Column Identity**:
A column's identity within one Spool run, independent of its current ordinal or member order. A destroyed column's identity is not reused.

**Column Width Intent**:
The original choice of a column's layout-slot width in logical points, as an absolute value, a proportion of its usable viewport, or an inherited configuration default. An ordinary new column without an explicit width rule adopts its admitted window's logical width once as an absolute value; later native observations and preset changes do not reseed it.

**Restore Candidate**:
A saved description of layout intent and possible object associations awaiting validated binding to the current run. Reading a candidate does not restore a window or establish current native topology. A candidate member with no recorded identity hint is an unresolved slot, not an absent association.

**Navigable Layout**:
A read-only projection of a Space's retained layout containing available, visible tiled identities, with stack and tab structure preserved. Directional, first/last/numeric, and next/previous tiled focus use this projection; retained unavailable identities are restoration data, not navigation targets. Projection never deletes or reorders the original layout.

**Column Ordinal**:
A column's one-based position within a Native Space's complete retained layout, including hidden or unavailable retained columns, at the time it is inspected or selected. It identifies a current position, not a persistent column identity, and filtering a view does not renumber it.

**Tiled Stacking Order**:
A derived bottom-to-top request order for the currently visible active Space.
Automatic repair only raises windows with overlapping observed rectangles on
that display, with the confirmed focus last when repair is needed. Disjoint
windows require no automatic AXRaise sweep; explicit layer raises still include
the whole eligible strip. Known pending geometry defers automatic repair.
Automatic repair also reads the presented `WindowServer` front-to-back order and
skips the sweep when it already matches the plan, because `AXRaise` is an
application action that also rewrites the target application's key and main
window. An unavailable or incomplete native order falls back to raising.
Eligible columns farther from confirmed tiled focus are raised first; ties use
layout order, and the focused window is raised last. Edge slivers participate.
Each native tab group contributes only its selected tab. Hidden, minimized,
unavailable, floating, fullscreen, and migrating windows do not participate.
Requests do not prove native z-order and never substitute for focus confirmation.

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

**Bar**:
Spool's own replacement for a display's menu bar: the panel that occupies that
display's menu-bar band and presents its Spaces, their windows, and the fixed
overview controls. It covers the system menu bar while expanded, so collapsing
it is how the menu bar is handed back.
_Avoid_: Status bar, toolbar, taskbar

**Bar Handle**:
The small tab at the centre of a display's Bar that collapses and expands it in
one click, and the only part of the Bar still on screen once the Bar has
collapsed. It is always visible, hanging below the Bar while expanded and
resting at the screen's top edge once the Bar has gone.
_Avoid_: Handbar, grabber, chevron, pull tab

**Notch Collar**:
The Bar Handle's form on a display with a Notch: a frame clearing the Notch on
every side the display actually shows, so a collapsed Bar reads as a slightly
larger Notch rather than as a tab hidden behind the camera housing.
_Avoid_: Notch capsule, notch spacer

**Notch**:
The camera housing that interrupts the top edge of a built-in display. Spool
never draws on it: it is a hole in the display, and the Bar works around it. The
measured value is the gap between the display's two auxiliary top areas, which
is what `notch_gap` returns and what `BarSurface::notch` carries; a display
without one reports no gap at all.
_Avoid_: Camera cutout, notched display (the display has a notch; the notch is
not the display)

**Notch Lane**:
One of the two runs of Spaces either side of the Notch on a notched display.
Both lanes hug the Notch, keeping clear of the Bar Handle's collar, so the
leftover space falls at the display's outer ends, and nothing is ever split
across it.
_Avoid_: Notch spacer, cutout lane

**Native Space**:
A macOS-managed desktop or fullscreen Space. macOS owns its lifecycle, order, visibility, and window membership.
_Avoid_: Workspace, virtual workspace

**State Snapshot**:
Spool's current externally readable projection of displays, Native Spaces, Tracked Windows, focus, visibility, and observed geometry. It is a point-in-time value, not an event history.
_Avoid_: Event state, Lua state

**Desktop Session**:
The current user's graphical desktop, encompassing applications, windows, displays, Native Spaces, and focus. Its lifetime and identity are independent of a particular Spool daemon run or saved layout.
_Avoid_: Daemon session, saved session

**State Change Notification**:
A public, possibly coalesced notification that part of the State Snapshot changed. Consumers use it as an invalidation signal and obtain a fresh State Snapshot whenever exact current state matters.
_Avoid_: Raw event, event log

**Native Observation**:
An on-demand, read-only collection of macOS window, display, Space, and focus evidence obtained independently of Spool's running daemon and tracked inventory. It records what the native sources could establish during collection, including uncertainty, rather than an atomic or infallible picture of the desktop.
_Avoid_: Real state, ground truth

**Native Evidence**:
A result attributed to a particular native source within a Native Observation, retaining its read outcome and any disagreement with other sources. Correlating evidence with a window does not replace the source results or discard evidence that cannot be correlated.

**Script State**:
A script-owned persistent key-value store. It remembers script choices across reloads and restarts but is not authoritative window or layout state.
_Avoid_: Window state, current state

**Configuration Script**:
The long-lived Lua program that declares Spool configuration and registers event handlers and keybindings.
_Avoid_: Client script

**Client Script**:
An on-demand Lua program that queries or controls a running Spool instance and ends when that invocation completes. It cannot register configuration handlers or keybindings.
_Avoid_: Configuration script

## Stack Height Intent

The positive relative share of vertical space desired by one stack item. A new item has share one. Shares belong to independent frame items, including a native-tab cohort as one item, and survive reordering or temporary absence. Equalizing gives each item the same share. Available space and layout limits determine effective heights without changing these original shares.

## Stack Item Identity

The continuing identity of one independently arranged frame item within a layout. Changing its position or selected native tab does not replace the item. A genuine split creates independent identities; a confirmed destruction ends the corresponding identity.

## Stack Projection Participant

A stack item that currently contributes to layout projection because at least one of its members is available and not ordered out. Only participants reserve vertical space, receive or donate an external height edit, and count toward the stack minimum. A retained item that is not a participant keeps its identity and raw share for a later reappearance.

## Unresolved Member Slot

A retained member position within a stack item whose window identity could not be cached when intent was saved. It preserves the item's member count and order for a later validated binding; it is not an identity, and it never authorizes one.

_Avoid_: Anonymous window, missing window, placeholder identity

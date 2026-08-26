# PaneruBar

PaneruBar is a native AppKit companion for Paneru. It renders one workspace bar
per display and reads only Paneru's structured state; it does not request
Accessibility access or enumerate windows itself.

The bar has one fixed workspace control followed by the selected workspace's
window icons: `1 A B C`. It is right-aligned in the free menu-bar gap between
the notch (or the screen midpoint) and the first system tray item. PaneruBar
reads menu-bar window bounds through Core Graphics, without Accessibility
permission, and clips the window strip to the measured gap.

Use the leading dotted grip to move the bar. Right-click anywhere on the bar to
lock or unlock it, open Settings, refresh, or quit. When unlocked, moving the
pointer onto an edge or corner reveals the appropriate resize cursor; drag that
edge directly to resize. PaneruBar stores the frame and lock state separately
for each display and restores them after relaunching.

The panel uses a native macOS material with a configurable tint, hairline border,
corner radius, and optional window shadow. The workspace rail and window strip
are separated visually without becoming nested capsules. The selected workspace
uses a quiet accent wash and short indicator; the focused window uses a subtle
background plus an accent underline. Interactive elements provide stable hover
feedback, and the unlocked move control uses a low-contrast two-column dotted
grip that strengthens on hover.

Workspace changes push the label and window strip in the navigation direction.
Window overflow arrows scroll icons as a strip, while focus changes glide the
indicator between visible icons and automatically reveal a newly focused hidden
window. Buttons use restrained hover and press feedback. All motion follows the
configured style and duration and is disabled when macOS Reduce Motion is active.

## Development

```sh
CLANG_MODULE_CACHE_PATH="$PWD/.build/module-cache" \
SWIFTPM_MODULECACHE_OVERRIDE="$PWD/.build/module-cache" \
swift test --disable-sandbox --scratch-path .build
```

Run the debug build with the matching Paneru CLI:

```sh
PANERU_CLI=/path/to/paneru .build/debug/PaneruBar
```

Right-click the bar and choose Settings to open the native settings bubble.
Changes are saved immediately to `~/.config/paneru-bar/config.toml`; comments,
unknown keys, and workspace labels in that file are preserved. PaneruBar creates
the file with defaults when it is missing and continues to reload external
changes automatically. Set `PANERU_BAR_CONFIG` to use a different path. See
`config.example.toml` for every supported option:

- bar height and workspace/window spacing;
- panel background, border, corner radius, and shadow;
- vertical padding with automatically derived icon sizing;
- focus-indicator color and thickness, workspace background, and floating-window icon
  treatment;
- editable per-workspace replacement labels;
- window spacing, focus-indicator visibility, and interaction animation.

Drag a window icon onto a workspace to move the exact Paneru window there and
follow it. Scroll the workspace control in the reverse scroll direction to
switch workspaces; clicking the label has no action. When every window cannot
fit, scroll the window strip to rotate which icons are visible. Trackpad momentum
is ignored to avoid continuing either operation after the gesture ends.

The settings header also provides a full reset to defaults. Legacy `icon_size`
entries are ignored because icon dimensions now follow bar height and vertical
padding.

## Paneru protocol

The initial state comes from `paneru query state --json`. Every line from
`paneru subscribe --json` invalidates the local snapshot and schedules a fresh
full query. UI actions use only the targeted CLI commands:

```sh
paneru send-cmd window focusid WINDOW_ID
paneru send-cmd workspace select DISPLAY_ID WORKSPACE_NUMBER
paneru send-cmd window move-to-workspace WINDOW_ID DISPLAY_ID WORKSPACE_NUMBER follow
```

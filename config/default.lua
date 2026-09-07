-- Spool configuration. Changes are hot-reloaded on save.
spool.setup {
  options = {},
  -- Editable preferences, not built-in window classification. Higher priority
  -- wins per field; equal priorities use rule-name order. Omit a matcher to
  -- match every value. Existing init.lua files are never rewritten on upgrade.
  windows = {
    default_dialog = { subrole = "AXDialog", floating = true, priority = -100 },
    default_system_dialog = { subrole = "AXSystemDialog", floating = true, priority = -100 },
    default_floating = { subrole = "AXFloatingWindow", floating = true, priority = -100 },
    default_system_settings = { bundle_id = "com.apple.systempreferences", floating = true, priority = -100 },
    default_keeping_you_awake = { bundle_id = "info.marcel-dierkes.KeepingYouAwake", floating = true, priority = -100 },
  },
  bar = {
    embed_in_menu_bar = true,
    height = 0, -- 0 follows each screen's menu-bar height.
    top_offset = 0,
    max_width = 0, -- 0 uses the available screen region.
    screen_padding = 12,
    notch_side = "balanced", -- "balanced", "left", or "right" on notched screens.
    icon_size = 0, -- 0 fits the available height, up to 20pt.
    vertical_padding = 3,
    horizontal_padding = 6,
    workspace_spacing = 5,
    window_spacing = 3,
    background_color = "#00000000",
    border_color = "#FFFFFF29",
    border_width = 0,
    corner_radius = 0,
    show_shadow = false,
    selection_color = "#0A84FFFF",
    active_workspace_color = "#0A84FF33",
    inactive_workspace_color = "#80808026",
    workspace_corner_radius = 4,
    show_focus_ring = true,
    focus_ring_width = 2,
    show_workspace_labels = true,
    label_font_size = 11,
    foreground_color = "auto", -- System label color, or an RGB/RGBA hex color.
    show_mission_control = true,
    show_desktop = true,
  },
}

-- spool.bind("alt+b", spool.action.window.balance)
-- spool.bind("alt+m", spool.action.mission_control)
-- spool.bind("alt+d", spool.action.show_desktop)
-- spool.on("window_focused", function(e) spool.log("focused " .. e.window_id) end)

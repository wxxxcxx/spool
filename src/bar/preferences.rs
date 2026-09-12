use serde::Deserialize;

use super::layout::BarMetrics;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotchSide {
    #[default]
    Balanced,
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Independent user-facing visibility/style switches.
pub struct BarPreferences {
    /// Resolved from the observed menu bar, never from configuration: the Bar
    /// takes over the menu bar, so the band decides how tall it is.
    #[serde(skip)]
    pub height: f64,
    pub notch_side: NotchSide,
    /// Zero fits the icon to the available height and vertical padding.
    pub icon_size: f64,
    pub vertical_padding: f64,
    pub workspace_spacing: f64,
    pub window_spacing: f64,
    pub horizontal_padding: f64,
    pub background_color: String,
    pub border_color: String,
    pub border_width: f64,
    pub corner_radius: f64,
    pub show_shadow: bool,
    pub selection_color: String,
    pub active_workspace_color: String,
    pub show_focus_ring: bool,
    pub focus_ring_width: f64,
    pub show_workspace_labels: bool,
    pub label_font_size: f64,
    pub foreground_color: String,
    pub inactive_workspace_color: String,
    pub workspace_corner_radius: f64,
    pub show_mission_control: bool,
    pub show_desktop: bool,
    /// How far the Bar Handle hangs below the Bar. Also how far the collapsed
    /// collar reaches past a notch, and how much taller than the band the panel
    /// window is.
    pub handle_height: f64,
    /// The Bar Handle's bottom corners. Its top edge never rounds.
    pub handle_radius: f64,
    /// Collapse the Spaces macOS is not showing into a compact deck. Off by
    /// default: every Space shows what it holds.
    pub collapse_inactive_spaces: bool,
}

impl Default for BarPreferences {
    fn default() -> Self {
        Self {
            height: 0.0,
            notch_side: NotchSide::Balanced,
            icon_size: 0.0,
            vertical_padding: 3.0,
            workspace_spacing: 5.0,
            window_spacing: 3.0,
            horizontal_padding: 6.0,
            background_color: "#00000000".to_owned(),
            border_color: "#FFFFFF29".to_owned(),
            border_width: 0.0,
            corner_radius: 0.0,
            show_shadow: false,
            selection_color: "#0A84FFFF".to_owned(),
            active_workspace_color: "#0A84FF33".to_owned(),
            show_focus_ring: true,
            focus_ring_width: 2.0,
            show_workspace_labels: true,
            label_font_size: 11.0,
            foreground_color: "auto".to_owned(),
            inactive_workspace_color: "#80808026".to_owned(),
            workspace_corner_radius: 4.0,
            show_mission_control: true,
            show_desktop: true,
            handle_height: super::placement::HandleMetrics::DEFAULT.height,
            handle_radius: super::placement::HandleMetrics::DEFAULT.radius,
            collapse_inactive_spaces: false,
        }
    }
}

impl BarPreferences {
    #[cfg(feature = "lua")]
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("icon_size", self.icon_size),
            ("vertical_padding", self.vertical_padding),
            ("horizontal_padding", self.horizontal_padding),
            ("workspace_spacing", self.workspace_spacing),
            ("window_spacing", self.window_spacing),
            ("border_width", self.border_width),
            ("corner_radius", self.corner_radius),
            ("focus_ring_width", self.focus_ring_width),
            ("label_font_size", self.label_font_size),
            ("workspace_corner_radius", self.workspace_corner_radius),
            ("handle_height", self.handle_height),
            ("handle_radius", self.handle_radius),
        ] {
            if !value.is_finite() {
                return Err(format!("bar.{name} must be finite"));
            }
        }
        Ok(())
    }

    /// Resolves the band height onto the preferences. The Bar is flush with the
    /// menu bar, so this is the only place its height comes from.
    #[must_use]
    pub fn for_menu_height(&self, menu_height: f64) -> Self {
        Self {
            height: menu_height.clamp(18.0, 64.0),
            ..self.clone()
        }
    }

    /// The Bar Handle's geometry, as the renderer needs it: the user's two
    /// numbers, clamped to what can actually be drawn.
    #[must_use]
    pub fn handle_metrics(&self) -> super::placement::HandleMetrics {
        super::placement::HandleMetrics::resolve(self.handle_height, self.handle_radius)
    }

    pub fn toolbar_width(&self) -> f64 {
        super::toolbar::width(self.show_mission_control, self.show_desktop)
    }

    #[must_use]
    pub fn metrics(&self) -> BarMetrics {
        let height = if self.height > 0.0 {
            self.height.clamp(18.0, 64.0)
        } else {
            24.0
        };
        let padding = self
            .vertical_padding
            .clamp(0.0, ((height - 12.0) / 2.0).max(0.0));
        let available_icon = (height - padding * 2.0).max(12.0);
        let icon_size = if self.icon_size > 0.0 {
            self.icon_size.clamp(12.0, available_icon)
        } else {
            available_icon.min(20.0)
        };
        BarMetrics {
            icon_size,
            icon_gap: self.window_spacing.clamp(0.0, 14.0),
            item_gap: self.workspace_spacing.clamp(2.0, 14.0),
            vertical_padding: (height - icon_size) / 2.0,
            horizontal_padding: self.horizontal_padding.clamp(4.0, 20.0),
            label_width: if self.show_workspace_labels {
                24.0
            } else {
                0.0
            },
            collapsed_width: 38.0,
            toolbar_width: self.toolbar_width(),
            collapse_inactive_spaces: self.collapse_inactive_spaces,
        }
    }

    #[must_use]
    pub fn workspace_label(ordinal: u32) -> String {
        (u64::from(ordinal) + 1).to_string()
    }

    #[must_use]
    pub fn workspace_label_width(&self, ordinal: u32) -> f64 {
        if !self.show_workspace_labels {
            return 0.0;
        }
        let characters = Self::workspace_label(ordinal).chars().count();
        (12.0
            + f64::from(u32::try_from(characters).unwrap_or(u32::MAX))
                * self.label_font_size.clamp(8.0, 20.0)
                * 0.64)
            .clamp(24.0, 120.0)
    }

    #[must_use]
    pub fn rgba(value: &str, fallback: [f64; 4]) -> [f64; 4] {
        parse_hex_color(value).unwrap_or(fallback)
    }
}

fn parse_hex_color(value: &str) -> Option<[f64; 4]> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let component = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some([
        f64::from(component(0)?) / 255.0,
        f64::from(component(2)?) / 255.0,
        f64::from(component(4)?) / 255.0,
        if hex.len() == 8 {
            f64::from(component(6)?) / 255.0
        } else {
            1.0
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_bar_decides_the_height_and_the_content_still_fits() {
        for menu_height in [22.0, 24.0, 34.0, 37.0, 64.0] {
            for requested in [0.0, 18.0, 200.0] {
                let preferences = BarPreferences {
                    height: requested,
                    icon_size: 100.0,
                    ..Default::default()
                }
                .for_menu_height(menu_height);
                let metrics = preferences.metrics();
                assert!(
                    (preferences.height - menu_height.clamp(18.0, 64.0)).abs() < f64::EPSILON,
                    "configuration cannot override the band"
                );
                assert!(
                    (metrics.icon_size + 2.0 * metrics.vertical_padding - preferences.height).abs()
                        < f64::EPSILON
                );
            }
        }
    }

    #[test]
    fn smaller_icons_are_centered_without_changing_bar_height() {
        let preferences = BarPreferences {
            icon_size: 12.0,
            ..Default::default()
        }
        .for_menu_height(37.0);
        let metrics = preferences.metrics();
        assert!((metrics.icon_size - 12.0).abs() < f64::EPSILON);
        assert!((metrics.vertical_padding - 12.5).abs() < f64::EPSILON);
        let automatic = BarPreferences::default().for_menu_height(37.0).metrics();
        assert!(automatic.icon_size <= 20.0);
        assert!(
            (automatic.icon_size + automatic.vertical_padding * 2.0 - 37.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn parses_rgb_and_rgba_colors() {
        assert_eq!(
            parse_hex_color("#FF8000"),
            Some([1.0, 128.0 / 255.0, 0.0, 1.0])
        );
        assert_eq!(
            parse_hex_color("00000080"),
            Some([0.0, 0.0, 0.0, 128.0 / 255.0])
        );
        assert_eq!(parse_hex_color("broken"), None);
    }

    #[test]
    fn workspace_labels_are_one_based_numeric_identifiers() {
        assert_eq!(BarPreferences::workspace_label(0), "1");
        assert_eq!(BarPreferences::workspace_label(9), "10");
        assert_eq!(BarPreferences::workspace_label(u32::MAX), "4294967296");
    }

    #[test]
    fn inactive_spaces_show_their_windows_unless_collapsing_is_asked_for() {
        let default = BarPreferences::default();
        assert!(
            !default.collapse_inactive_spaces,
            "the compact deck is opt-in"
        );
        assert!(!default.metrics().collapse_inactive_spaces);
        let collapsed: BarPreferences =
            serde_json::from_str(r#"{"collapse_inactive_spaces": true}"#).unwrap();
        assert!(collapsed.collapse_inactive_spaces);
        assert!(collapsed.metrics().collapse_inactive_spaces);
    }

    #[test]
    fn label_visibility_defaults_on_and_can_be_disabled() {
        assert!(
            serde_json::from_str::<BarPreferences>("{}")
                .unwrap()
                .show_workspace_labels
        );
        let prefs: BarPreferences =
            serde_json::from_str(r#"{"show_workspace_labels": false}"#).unwrap();
        assert!(!prefs.show_workspace_labels);
        assert!(prefs.workspace_label_width(0).abs() < f64::EPSILON);
        assert!(prefs.metrics().label_width.abs() < f64::EPSILON);
        assert_eq!(BarPreferences::workspace_label(0), "1");
    }

    #[test]
    fn legacy_custom_names_never_replace_numeric_space_identifiers() {
        let prefs: BarPreferences =
            serde_json::from_str(r#"{"workspace_labels": {"1": "A", "2": "B"}}"#).unwrap();
        assert_eq!(prefs, BarPreferences::default());
        assert_eq!(
            (0..4)
                .map(BarPreferences::workspace_label)
                .collect::<Vec<_>>(),
            vec!["1", "2", "3", "4"]
        );
    }
}

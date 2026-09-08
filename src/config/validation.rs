use super::{InnerConfig, MainOptions, decorations::BorderRadiusOption};

impl InnerConfig {
    pub(super) fn validate(&self) -> Result<(), String> {
        self.bar.validate()?;
        self.options.validate()?;
        if let Some(swipe) = &self.swipe {
            finite_fields(
                "swipe",
                &[
                    ("sensitivity", swipe.sensitivity),
                    ("deceleration", swipe.deceleration),
                ],
            )?;
        }
        if let Some(decorations) = &self.decorations {
            for (name, decoration) in [
                ("active", &decorations.active),
                ("inactive", &decorations.inactive),
            ] {
                if let Some(decoration) = decoration {
                    validate_decoration(&format!("decorations.{name}"), decoration)?;
                }
            }
        }
        if let Some(windows) = &self.windows {
            for (name, window) in windows {
                finite_fields(
                    &format!("windows.{name}"),
                    &[
                        ("width", window.width),
                        ("border_radius", window.border_radius),
                    ],
                )?;
            }
        }
        Ok(())
    }
}

impl MainOptions {
    fn validate(&self) -> Result<(), String> {
        finite_fields(
            "options",
            &[
                ("animation_speed", self.animation_speed),
                ("sliver_height", self.sliver_height),
                (
                    "dim_inactive_windows",
                    self.dim_inactive_windows.map(f64::from),
                ),
                ("border_opacity", self.border_opacity),
                ("border_width", self.border_width),
                ("border_radius", radius_value(self.border_radius.as_ref())),
                ("swipe_sensitivity", self.swipe_sensitivity),
                ("swipe_deceleration", self.swipe_deceleration),
                ("window_hidden_ratio", self.window_hidden_ratio),
            ],
        )?;
        for (index, ratio) in self.preset_column_widths.iter().copied().enumerate() {
            if !ratio.is_finite() {
                return Err(format!(
                    "spool.setup: options.preset_column_widths[{}] must be finite",
                    index + 1
                ));
            }
        }
        Ok(())
    }
}

fn validate_decoration(
    path: &str,
    decoration: &super::decorations::GeneralDecorationsOptions,
) -> Result<(), String> {
    if let Some(border) = &decoration.border {
        finite_fields(
            &format!("{path}.border"),
            &[
                ("opacity", border.opacity),
                ("width", border.width),
                ("radius", radius_value(border.radius.as_ref())),
            ],
        )?;
    }
    if let Some(dim) = &decoration.dim {
        finite_fields(
            &format!("{path}.dim"),
            &[
                ("opacity", dim.opacity.map(f64::from)),
                ("opacity_night", dim.opacity_night.map(f64::from)),
            ],
        )?;
    }
    Ok(())
}

fn radius_value(radius: Option<&BorderRadiusOption>) -> Option<f64> {
    match radius {
        Some(BorderRadiusOption::Value(value)) => Some(*value),
        _ => None,
    }
}

fn finite_fields(path: &str, fields: &[(&str, Option<f64>)]) -> Result<(), String> {
    for (name, value) in fields {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err(format!("spool.setup: {path}.{name} must be finite"));
        }
    }
    Ok(())
}

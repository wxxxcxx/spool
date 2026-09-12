use arc_swap::{ArcSwap, Guard};
use bevy::ecs::resource::Resource;
#[cfg(feature = "lua")]
use objc2_core_foundation::{CFData, CFString};
use regex::Regex;
use serde::{Deserialize, Deserializer, de};
use std::{collections::HashMap, sync::Arc, time::Duration};
#[cfg(feature = "lua")]
use std::{
    env,
    ffi::c_void,
    fs::{OpenOptions, create_dir_all},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::LazyLock,
};
use stdext::function_name;
#[cfg(feature = "lua")]
use tracing::{error, info, warn};

use self::decorations::BorderRadiusOption;
use self::swipe::SwipeGestureDirection;
#[cfg(test)]
use crate::commands::{Action, Operation, ResizeAxis, ResizeDirection};
use crate::errors::{Error, Result};
use crate::{
    manager::ProcessApi,
    platform::{Modifiers, macos_major_version},
};
#[cfg(feature = "lua")]
use crate::{
    platform::{CFStringRef, OSStatus},
    util::{AXUIWrapper, MacResult},
};

pub mod decorations;
pub mod padding;
pub mod swipe;
#[cfg(feature = "lua")]
mod validation;

/// The default Lua init script, written on first launch when no script exists so the
/// file watcher always has a concrete path to observe for hot reloading.
#[cfg(feature = "lua")]
pub(crate) const DEFAULT_LUA_SCRIPT: &str = include_str!("../config/default.lua");

#[cfg(feature = "lua")]
fn default_lua_file() -> std::io::Result<PathBuf> {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::NotFound,
                "neither XDG_CONFIG_HOME nor HOME is set",
            )
        })?;

    Ok(config_home.join("spool").join("init.lua"))
}

/// Finds the first existing Lua init script. Honors `$SPOOL_LUA` first.
#[cfg(feature = "lua")]
pub fn discover_lua_file() -> Option<PathBuf> {
    if let Ok(path_str) = env::var("SPOOL_LUA") {
        let path = PathBuf::from(path_str);
        if path.exists() {
            return Some(path);
        }
        warn!(
            "{}: $SPOOL_LUA is set to {}, but the file does not exist. Falling back to default locations.",
            function_name!(),
            path.display()
        );
    }

    let standard_paths = [env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".spool.lua"))];

    let xdg_dirs = xdg::BaseDirectories::with_prefix("spool");
    let xdg_paths = xdg_dirs.find_config_files("init.lua");

    standard_paths
        .into_iter()
        .flatten()
        .chain(xdg_paths)
        .find(|path| path.exists())
}

/// Returns the path to the Lua init script, creating a default one at the XDG
/// location if none exists.
#[cfg(feature = "lua")]
pub fn ensure_lua_file() -> std::io::Result<PathBuf> {
    if let Some(path) = discover_lua_file() {
        return std::path::absolute(path);
    }
    let path = default_lua_file()?;
    if create_lua_file_at(&path)? {
        info!("Created default Lua script at {}", path.display());
    }
    std::path::absolute(path)
}

#[cfg(feature = "lua")]
fn create_lua_file_at(path: &Path) -> std::io::Result<bool> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidInput, "Lua script path has no parent")
    })?;
    create_dir_all(parent)?;

    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(DEFAULT_LUA_SCRIPT.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Parses a command argument vector into a [`Action`] (e.g. `["window",
/// "focus", "east"]`), mapping the shared vocabulary crate's parse error into
/// this crate's configuration error.
#[cfg(test)]
pub(crate) fn parse_action(argv: &[&str]) -> Result<Action> {
    spool_shared_types::commands::parse_action(argv)
        .map_err(|err| Error::InvalidConfig(format!("{}: {err}", function_name!())))
}

/// `Config` manages the application's configuration, including options, keybindings, and window-specific parameters.
/// It provides methods for loading, reloading, and querying configuration settings.
#[derive(Clone, Debug, Resource)]
pub struct Config {
    inner: Arc<ArcSwap<InnerConfig>>,
}

impl Config {
    /// Atomically adopts another config's inner data via a lock-free
    /// `ArcSwap::store`, so every shared handle to this `Config` observes the
    /// new settings. Used by the Lua hot-reload path.
    #[cfg(feature = "lua")]
    pub(crate) fn replace_inner_from(&self, other: &Config) {
        self.inner.store(other.inner.load_full());
    }

    /// Returns a read guard to the inner `InnerConfig` for read-only access.
    ///
    /// # Returns
    ///
    /// A `Guard<Arc<InnerConfig>>` allowing read access to `InnerConfig`.
    fn inner(&self) -> Guard<Arc<InnerConfig>> {
        self.inner.load()
    }

    /// Returns a clone of the `MainOptions` from the current configuration.
    ///
    /// # Returns
    ///
    /// A `MainOptions` struct containing the main configuration options.
    pub fn options(&self) -> MainOptions {
        self.inner().options.clone()
    }

    pub fn bar_preferences(&self) -> crate::bar::BarPreferences {
        self.inner().bar.clone()
    }

    // Exponential ease-out decay rate (per second) consumed by the animation
    // systems as `t = 1 - e^(-rate*dt)`. Higher values feel snappier; very large
    // values collapse to an instant snap.
    // Suggested range: 8..20 for a fluid feel. Unset = instant (no animation),
    pub fn animation_speed(&self) -> f64 {
        self.options()
            .animation_speed
            // If unset, set it to something high, so the move happens immediately,
            // effectively disabling animation.
            .unwrap_or(1_000_000.0)
            .max(0.0)
    }

    #[cfg(test)]
    pub fn find_window_properties(&self, title: &str, bundle_id: &str) -> Vec<WindowParams> {
        self.match_window_rules(Some(title), Some(bundle_id), None, None)
            .params
    }

    /// Higher priority wins per field; equal priorities use ascending rule names.
    /// Unknown metadata defers potentially matching rules, never matches an empty string.
    pub(crate) fn match_window_rules(
        &self,
        title: Option<&str>,
        bundle_id: Option<&str>,
        role: Option<&str>,
        subrole: Option<&str>,
    ) -> MatchedWindowRules {
        let inner = self.inner();
        let Some(windows) = &inner.windows else {
            return MatchedWindowRules::default();
        };
        let mut ordered = windows.iter().collect::<Vec<_>>();
        ordered.sort_by(|(a_name, a), (b_name, b)| {
            b.priority.cmp(&a.priority).then_with(|| a_name.cmp(b_name))
        });
        let mut matched = MatchedWindowRules::default();
        for (_, rule) in ordered {
            let predicates = [
                rule.title
                    .as_ref()
                    .map(|regex| title.map(|value| regex.is_match(value))),
                rule.bundle_id
                    .as_deref()
                    .map(|expected| bundle_id.map(|value| value == expected)),
                rule.role
                    .as_deref()
                    .map(|expected| role.map(|value| value == expected)),
                rule.subrole
                    .as_deref()
                    .map(|expected| subrole.map(|value| value == expected)),
            ];
            if predicates.contains(&Some(Some(false))) {
                continue;
            }
            if predicates.contains(&Some(None)) {
                matched.pending = true;
            } else {
                matched.params.push(rule.clone());
            }
        }
        matched
    }

    /// Returns `true` if any window rule for the given bundle ID requests that the
    /// process be forcibly tracked even when macOS reports it as unobservable.
    pub fn should_force_track_process(&self, process: &dyn ProcessApi) -> bool {
        self.inner().windows.as_ref().is_some_and(|windows| {
            let Some(bundle_id) = process
                .application()
                .as_ref()
                .and_then(|app| app.bundleIdentifier())
                .map(|id| id.to_string())
            else {
                return false;
            };
            windows.values().any(|params| {
                params
                    .bundle_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == bundle_id)
                    && params.track.is_some_and(|track| track)
            })
        })
    }

    pub fn sliver_height(&self) -> f64 {
        self.options().sliver_height.unwrap_or(1.0).clamp(0.1, 1.0)
    }

    pub fn sliver_width(&self) -> i32 {
        i32::from(self.options().sliver_width.unwrap_or(5)).max(1)
    }

    pub fn edge_padding(&self) -> (i32, i32, i32, i32) {
        let config = self.inner();
        let o = &config.options;
        let p = config.padding.as_ref();
        (
            i32::from(p.and_then(|p| p.top).or(o.padding_top).unwrap_or(0)),
            i32::from(p.and_then(|p| p.right).or(o.padding_right).unwrap_or(0)),
            i32::from(p.and_then(|p| p.bottom).or(o.padding_bottom).unwrap_or(0)),
            i32::from(p.and_then(|p| p.left).or(o.padding_left).unwrap_or(0)),
        )
    }

    pub fn preset_column_widths(&self) -> Vec<f64> {
        self.options().preset_column_widths
    }

    pub fn swipe_gesture_direction(&self) -> SwipeGestureDirection {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.gesture.as_ref())
            .and_then(|gesture| gesture.direction.clone())
            .clone()
            .or(config.options.swipe_gesture_direction.clone())
            .unwrap_or(SwipeGestureDirection::Natural)
    }

    pub fn swipe_gesture_fingers(&self) -> Option<usize> {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.gesture.as_ref())
            .and_then(|gesture| gesture.fingers_count)
            .or(config.options.swipe_gesture_fingers)
    }

    pub fn swipe_vertical(&self) -> bool {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.gesture.as_ref())
            .and_then(|gesture| gesture.vertical)
            // Enabled by default
            .is_none_or(|vertical| vertical)
    }

    pub fn dim_inactive_opacity(&self) -> f32 {
        let config = self.inner();
        let color = config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.inactive.as_ref())
            .and_then(|inactive| inactive.dim.as_ref())
            .and_then(|dim| dim.color.as_ref())
            .or(config.options.dim_inactive_color.as_ref());
        if color.is_none() {
            return 0.0;
        }
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.inactive.as_ref())
            .and_then(|inactive| inactive.dim.as_ref())
            .and_then(|dim| dim.opacity)
            .or(config.options.dim_inactive_windows)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    pub fn dim_inactive_color(&self) -> (f64, f64, f64) {
        let config = self.inner();
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.inactive.as_ref())
            .and_then(|inactive| inactive.dim.as_ref())
            .and_then(|dim| dim.color.as_deref())
            .or(config.options.dim_inactive_color.as_deref())
            .map_or((0.0, 0.0, 0.0), parse_hex_color)
    }

    pub fn border_active_window(&self) -> bool {
        let config = self.inner();
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.active.as_ref())
            .and_then(|active| active.border.as_ref())
            .and_then(|border| border.enabled)
            .or(config.options.border_active_window)
            .unwrap_or(false)
    }

    pub fn border_color(&self) -> (f64, f64, f64) {
        let config = self.inner();
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.active.as_ref())
            .and_then(|active| active.border.as_ref())
            .and_then(|border| border.color.as_deref())
            .or(config.options.border_color.as_deref())
            .map_or((1.0, 1.0, 1.0), parse_hex_color)
    }

    pub fn border_opacity(&self) -> f64 {
        let config = self.inner();
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.active.as_ref())
            .and_then(|active| active.border.as_ref())
            .and_then(|border| border.opacity)
            .or(config.options.border_opacity)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0)
    }

    pub fn border_width(&self) -> f64 {
        let config = self.inner();
        config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.active.as_ref())
            .and_then(|active| active.border.as_ref())
            .and_then(|border| border.width)
            .or(config.options.border_width)
            .unwrap_or(2.0)
            .max(0.0)
    }

    pub fn border_radius(&self) -> BorderRadiusOption {
        let config = self.inner();
        match config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.active.as_ref())
            .and_then(|active| active.border.as_ref())
            .and_then(|border| border.radius.clone())
            .or(config.options.border_radius.clone())
            .unwrap_or(BorderRadiusOption::Auto)
        {
            BorderRadiusOption::Auto if macos_major_version() == 26 => BorderRadiusOption::Auto,
            BorderRadiusOption::Value(value) => BorderRadiusOption::Value(value.max(0.0)),
            BorderRadiusOption::Auto => BorderRadiusOption::Value(10.0),
        }
    }

    pub fn menubar_height(&self) -> Option<i32> {
        self.options().menubar_height.map(i32::from)
    }

    pub fn swipe_sensitivity(&self) -> f64 {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.sensitivity)
            .or(config.options.swipe_sensitivity)
            .unwrap_or(0.35)
            .clamp(0.1, 2.0)
    }

    pub fn continuous_swipe(&self) -> bool {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.continuous)
            .or(config.options.continuous_swipe)
            // Default: true (enabled).
            .unwrap_or(true)
    }

    pub fn swipe_deceleration(&self) -> f64 {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.deceleration)
            .or(config.options.swipe_deceleration)
            .unwrap_or(4.0)
            .clamp(1.0, 10.0)
    }

    pub fn mouse_resize_modifier(&self) -> Option<Modifiers> {
        self.options().mouse_resize_modifier
    }

    pub fn restore_enabled(&self) -> bool {
        self.inner()
            .restore
            .as_ref()
            .and_then(|restore| restore.enabled)
            .unwrap_or(true)
    }

    pub fn restore_startup_grace(&self) -> Duration {
        Duration::from_millis(
            self.inner()
                .restore
                .as_ref()
                .and_then(|restore| restore.startup_grace_ms)
                .unwrap_or(2000),
        )
    }

    pub fn restore_missing_windows(&self) -> MissingWindowBehavior {
        self.inner()
            .restore
            .as_ref()
            .and_then(|restore| restore.missing_windows)
            .unwrap_or(MissingWindowBehavior::Ignore)
    }

    pub fn swipe_scroll_modifier(&self) -> Modifiers {
        let config = self.inner();
        config
            .swipe
            .as_ref()
            .and_then(|swipe| swipe.scroll.as_ref())
            .and_then(|scroll| scroll.modifier)
            .unwrap_or(Modifiers::ALT)
    }

    pub fn window_dim_ratio(&self, is_dark: bool) -> Option<f32> {
        let config = self.inner();
        if config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.inactive.as_ref())
            .and_then(|inactive| inactive.dim.as_ref())
            .and_then(|dim| dim.color.as_ref())
            .is_some()
            || config.options.dim_inactive_color.is_some()
        {
            // This is not our dimming - it's the color one.
            return None;
        }

        let dim = config
            .decorations
            .as_ref()
            .and_then(|decorations| decorations.inactive.as_ref())
            .and_then(|inactive| inactive.dim.as_ref());

        if is_dark {
            dim.and_then(|d| d.opacity_night)
                .or(dim.and_then(|d| d.opacity))
                .or(config.options.dim_inactive_windows)
        } else {
            dim.and_then(|d| d.opacity)
                .or(config.options.dim_inactive_windows)
        }
    }

    /// Returns the allowed hidden fraction of a window before a focus change
    /// forces it into view. 0.0 = always bring into view (eager),
    /// 1.0 = never move unless fully invisible (lazy). Default: 0.0.
    pub fn window_hidden_ratio(&self) -> f64 {
        self.options()
            .window_hidden_ratio
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    pub fn window_resize_cycle(&self) -> bool {
        self.options().window_resize_cycle.unwrap_or(true)
    }

    pub fn floating_window_move_step(&self) -> i32 {
        self.options()
            .floating_window_move_step
            .unwrap_or(20)
            .max(1)
    }

    pub fn floating_window_resize_step(&self) -> i32 {
        self.options()
            .floating_window_resize_step
            .unwrap_or(40)
            .max(1)
    }

    pub fn auto_center(&self) -> bool {
        self.options().auto_center.is_some_and(|center| center)
    }

    pub fn horizontal_mouse_warp(&self) -> Option<i16> {
        self.options().horizontal_mouse_warp
    }

    /// Returns `true` if focus should follow the mouse based on the current configuration.
    /// If the configuration option is not set, it defaults to `true`.
    pub fn focus_follows_mouse(&self) -> bool {
        // Default is enabled.
        self.options().focus_follows_mouse.is_none_or(|ffm| ffm)
    }

    /// Returns `true` if the mouse cursor should follow the focused window based on the current configuration.
    /// If the configuration option is not set, it defaults to `true`.
    pub fn mouse_follows_focus(&self) -> bool {
        // Default is enabled.
        self.options().mouse_follows_focus.is_none_or(|mff| mff)
    }

    pub fn horizontal_mouse_warp_offset(&self) -> i32 {
        self.options().horizontal_mouse_warp_offset.unwrap_or(0)
    }

    /// Enables the private, capability-probed Space control adapter.
    /// Off by default; observe-only Space state remains available.
    pub fn space_control_enabled(&self) -> bool {
        self.options()
            .experimental_space_control
            .is_some_and(|enabled| enabled)
    }

    /// Uses Mission Control's animated Control+Arrow transition when focusing
    /// a Space. Enabled by default; set the option to false for instant switching.
    pub fn space_switch_animation(&self) -> bool {
        self.options()
            .space_switch_animation
            .is_none_or(|enabled| enabled)
    }
}

fn parse_hex_color(hex: &str) -> (f64, f64, f64) {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.is_ascii() {
        return (1.0, 1.0, 1.0);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255);
    (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    )
}

impl Default for Config {
    /// Returns a default `Config` instance with an empty `InnerConfig`.
    fn default() -> Self {
        Config {
            inner: Arc::new(ArcSwap::from_pointee(InnerConfig::default())),
        }
    }
}

#[cfg(test)]
impl TryFrom<&str> for Config {
    type Error = crate::errors::Error;

    fn try_from(input: &str) -> std::result::Result<Self, Self::Error> {
        Ok(Config {
            inner: Arc::new(ArcSwap::from_pointee(serde_json::from_str(input)?)),
        })
    }
}

impl From<(MainOptions, Vec<WindowParams>)> for Config {
    fn from((options, params): (MainOptions, Vec<WindowParams>)) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(InnerConfig {
                options,
                windows: params
                    .into_iter()
                    .enumerate()
                    .map(|(nr, param)| Some((format!("param{nr:020}"), param)))
                    .collect(),
                ..Default::default()
            })),
        }
    }
}

/// Configuration published atomically to the ECS and input handler.
#[derive(Deserialize, Debug, Default)]
struct InnerConfig {
    // Defaulted so a config may omit these; otherwise serde requires them.
    #[serde(default)]
    options: MainOptions,
    #[serde(default)]
    bar: crate::bar::BarPreferences,
    windows: Option<HashMap<String, WindowParams>>,
    decorations: Option<decorations::DecorationsOptions>,
    swipe: Option<swipe::SwipeOptions>,
    padding: Option<padding::PaddingOptions>,
    restore: Option<RestoreOptions>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MissingWindowBehavior {
    Ignore,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct RestoreOptions {
    pub enabled: Option<bool>,
    pub startup_grace_ms: Option<u64>,
    pub missing_windows: Option<MissingWindowBehavior>,
}

/// `MainOptions` represents the primary configuration options for the window manager.
/// These options control various behaviors such as mouse focus, gesture recognition, and window animation.
#[derive(Deserialize, Clone, Debug)]
pub struct MainOptions {
    /// Enables or disables focus follows mouse behavior.
    pub focus_follows_mouse: Option<bool>,
    /// Enables or disables mouse follows focus behavior.
    pub mouse_follows_focus: Option<bool>,
    /// Warps the mouse to the closest screen when at the edge.
    pub horizontal_mouse_warp: Option<i16>,
    /// Vertical pixel offset applied to the warp landing position, signed by
    /// warp direction. Use to compensate for physical desk arrangement
    /// differing from the macOS arrangement (e.g. portrait monitor sitting
    /// higher or lower than the laptop). When warping downward (target below
    /// source) the offset is added; when warping upward, subtracted.
    pub horizontal_mouse_warp_offset: Option<i32>,
    /// A list of preset column widths (as ratios) used for resizing windows.
    #[serde(default = "default_preset_column_widths")]
    pub preset_column_widths: Vec<f64>,
    /// The animation speed for window movements in pixels per second.
    pub animation_speed: Option<f64>,
    /// Automatically center the window when switching focus with keyboard.
    pub auto_center: Option<bool>,
    /// Height of off-screen window slivers as a ratio (0.0–1.0) of the display height.
    /// Lower values hide the window's corner radius at screen edges.
    /// Default: 1.0 (full height).
    pub sliver_height: Option<f64>,
    /// Width of off-screen window slivers in pixels.
    /// Default: 5 pixels.
    pub sliver_width: Option<u16>,
    /// Legacy top-level padding (deprecated; use `[padding]`).
    pub padding_top: Option<u16>,
    pub padding_bottom: Option<u16>,
    pub padding_left: Option<u16>,
    pub padding_right: Option<u16>,
    /// Legacy top-level dim options (deprecated; use `[decorations.inactive.dim]`).
    pub dim_inactive_windows: Option<f32>,
    pub dim_inactive_color: Option<String>,
    /// Legacy top-level border options (deprecated; use `[decorations.active.border]`).
    pub border_active_window: Option<bool>,
    pub border_color: Option<String>,
    pub border_opacity: Option<f64>,
    pub border_width: Option<f64>,
    #[serde(
        default,
        deserialize_with = "decorations::deserialize_border_radius_option"
    )]
    pub border_radius: Option<BorderRadiusOption>,
    /// Legacy top-level swipe options (deprecated; use `[swipe]`).
    pub swipe_gesture_fingers: Option<usize>,
    pub swipe_gesture_direction: Option<SwipeGestureDirection>,

    #[allow(dead_code)]
    pub continuous_swipe: Option<bool>,
    pub swipe_sensitivity: Option<f64>,
    pub swipe_deceleration: Option<f64>,
    /// The modifier key used for mouse-based window resizing.
    #[serde(default, deserialize_with = "deserialize_modifier")]
    pub mouse_resize_modifier: Option<Modifiers>,
    /// Override the system menubar height (in pixels).
    /// When set, this value is used instead of the height reported by macOS.
    pub menubar_height: Option<u16>,
    /// How much of a window may be hidden before a focus change forces it into
    /// view. 0.0 (default) = always bring into view. 1.0 = never move unless
    /// fully invisible. E.g. 0.5 = tolerate up to 50% hidden.
    pub window_hidden_ratio: Option<f64>,

    /// Whether grow/shrink cycles back when reaching the end of presets.
    /// Default: true (cycles). Set to false to stop at the limits.
    pub window_resize_cycle: Option<bool>,

    /// Pixel distance used by `window move <direction>` for floating windows.
    pub floating_window_move_step: Option<i32>,

    /// Pixel distance used by grow/shrink actions for floating windows.
    pub floating_window_resize_step: Option<i32>,

    /// Opts into private Space control when the running macOS exposes
    /// the required bridged operation. Never disables SIP or injects Dock.
    pub experimental_space_control: Option<bool>,

    /// Use the native Mission Control animation when switching Spaces.
    /// Default: true.
    pub space_switch_animation: Option<bool>,
}

impl Default for MainOptions {
    fn default() -> Self {
        Self {
            focus_follows_mouse: None,
            mouse_follows_focus: None,
            horizontal_mouse_warp: None,
            horizontal_mouse_warp_offset: None,
            preset_column_widths: default_preset_column_widths(),
            animation_speed: None,
            auto_center: None,
            sliver_height: None,
            sliver_width: None,
            padding_top: None,
            padding_bottom: None,
            padding_left: None,
            padding_right: None,
            dim_inactive_windows: None,
            dim_inactive_color: None,
            border_active_window: None,
            border_color: None,
            border_opacity: None,
            border_width: None,
            border_radius: None,
            swipe_gesture_fingers: None,
            swipe_gesture_direction: None,
            continuous_swipe: None,
            swipe_sensitivity: None,
            swipe_deceleration: None,
            mouse_resize_modifier: None,
            menubar_height: None,
            window_hidden_ratio: None,
            window_resize_cycle: None,
            floating_window_move_step: None,
            floating_window_resize_step: None,
            experimental_space_control: None,
            space_switch_animation: None,
        }
    }
}

/// Returns a default set of column widths.
pub fn default_preset_column_widths() -> Vec<f64> {
    vec![0.25, 0.33333, 0.50, 0.66667, 0.75, 1.0, 1.5, 2.0]
}

/// `WindowParams` defines rules and properties for specific windows based on their title or bundle ID.
/// These parameters can override default window management behavior, such as forcing a window to float or setting its initial index.
#[derive(Clone, Debug, Deserialize)]
pub struct WindowParams {
    /// A regular expression to match against the window's title.
    #[serde(default, deserialize_with = "deserialize_title")]
    title: Option<Regex>,
    /// An optional bundle identifier to match against the application's bundle ID.
    bundle_id: Option<String>,
    role: Option<String>,
    subrole: Option<String>,
    #[serde(default)]
    priority: i32,
    /// If `true`, the tracked window starts floating instead of tiled.
    pub floating: Option<bool>,
    /// Explicit admission override. False excludes; true permits nonstandard
    /// independent windows, but cannot turn a menu/control/child into a window.
    pub track: Option<bool>,
    /// An optional preferred index for the window's position in the window strip.
    pub index: Option<usize>,
    pub vertical_padding: Option<i32>,
    pub horizontal_padding: Option<i32>,
    pub dont_focus: Option<bool>,
    /// An optional positive initial width ratio relative to the display width.
    /// Values above 1.0 create an oversized, horizontally scrollable window.
    /// Overrides the default column width when the window is first tiled.
    pub width: Option<f64>,
    /// Grid placement for floating windows: "cols:rows:x:y:w:h".
    /// Divides the display into a grid and positions the window at the given cell/span.
    pub grid: Option<String>,
    /// Per-window override for the active window border corner radius.
    pub border_radius: Option<f64>,
    /// Keyboard shortcuts that should be passed through to this app instead of
    /// being intercepted by spool. Uses the same plus-separated format as
    /// `spool.bind` (e.g. `"ctrl+alt+h"`).
    #[serde(default)]
    #[cfg_attr(not(feature = "lua"), allow(dead_code))]
    bindings_passthrough: Vec<String>,
    /// Resolved `(keycode, modifiers)` pairs from `bindings_passthrough`.
    #[serde(skip)]
    parsed_passthrough: Vec<(u8, Modifiers)>,
}

impl WindowParams {
    #![allow(unused)]
    pub fn new(title: &str, bundle_id: Option<String>) -> Self {
        Self {
            title: Some(Regex::new(title).unwrap()),
            bundle_id,
            role: None,
            subrole: None,
            priority: 0,
            floating: None,
            track: None,
            index: None,
            vertical_padding: None,
            horizontal_padding: None,
            dont_focus: None,
            width: None,
            grid: None,
            border_radius: None,
            bindings_passthrough: Vec::new(),
            parsed_passthrough: Vec::new(),
        }
    }

    /// Returns the resolved passthrough keybindings for this window rule.
    pub fn passthrough_keys(&self) -> &[(u8, Modifiers)] {
        &self.parsed_passthrough
    }

    /// Parses six finite grid fields into `(x_ratio, y_ratio, w_ratio, h_ratio)`.
    pub fn grid_ratios(&self) -> Option<(f64, f64, f64, f64)> {
        let grid = self.grid.as_ref()?;
        let parts: Vec<f64> = grid
            .split(':')
            .map(|part| part.parse().ok())
            .collect::<Option<_>>()?;
        let parts: [f64; 6] = parts.try_into().ok()?;
        if !parts.iter().all(|part| part.is_finite()) {
            return None;
        }
        let [cols, rows, x, y, width, height] = parts;
        if cols <= 0.0 || rows <= 0.0 {
            return None;
        }
        let ratios = [x / cols, y / rows, width / cols, height / rows];
        ratios
            .iter()
            .all(|ratio| ratio.is_finite())
            .then_some((ratios[0], ratios[1], ratios[2], ratios[3]))
    }
}

/// Deserializes a regular expression from a string for window titles.
fn deserialize_title<'de, D>(deserializer: D) -> std::result::Result<Option<Regex>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Regex::new(&s).map(Some).map_err(de::Error::custom)
}

#[derive(Default)]
pub(crate) struct MatchedWindowRules {
    pub(crate) params: Vec<WindowParams>,
    pub(crate) pending: bool,
}

#[cfg(test)]
mod window_rule_tests {
    use super::*;

    #[test]
    fn window_rules_resolve_fields_by_priority_then_name() {
        let config = Config::try_from(
            r#"{"windows": {
            "z_default": {"floating": true, "track": true, "width": 0.4, "priority": -100},
            "b_tie": {"title": ".*", "floating": true, "priority": 10},
            "a_override": {"bundle_id": "test", "floating": false, "track": false, "priority": 10}
        }}"#,
        )
        .unwrap();
        for _ in 0..20 {
            let matched = config.match_window_rules(Some("Settings"), Some("test"), None, None);
            assert!(!matched.pending);
            assert_eq!(matched.params.iter().find_map(|p| p.floating), Some(false));
            assert_eq!(matched.params.iter().find_map(|p| p.track), Some(false));
            assert!(
                matched
                    .params
                    .iter()
                    .find_map(|p| p.width)
                    .is_some_and(|width| (width - 0.4).abs() < f64::EPSILON)
            );
        }
    }

    #[test]
    fn window_rules_match_roles_without_requiring_a_title() {
        let config = Config::try_from(
            r#"{"windows": {
            "dialog": {"role": "AXWindow", "subrole": "AXDialog", "floating": true},
            "other_app": {"title": ".*", "bundle_id": "other", "track": false}
        }}"#,
        )
        .unwrap();
        let matched =
            config.match_window_rules(None, Some("test"), Some("AXWindow"), Some("AXDialog"));
        assert!(
            !matched.pending,
            "a definite bundle mismatch must beat an unknown title"
        );
        assert_eq!(matched.params.len(), 1);
        assert_eq!(matched.params[0].floating, Some(true));
    }

    #[test]
    fn window_rules_do_not_turn_failed_reads_into_empty_strings() {
        let config = Config::try_from(
            r#"{"windows": {
            "untitled": {"title": "^$", "floating": true}
        }}"#,
        )
        .unwrap();
        let unknown = config.match_window_rules(None, None, None, None);
        assert!(unknown.pending);
        assert!(unknown.params.is_empty());
        let empty = config.match_window_rules(Some(""), None, None, None);
        assert!(!empty.pending);
        assert_eq!(empty.params.len(), 1);
    }

    #[test]
    fn window_rules_absent_bundle_does_not_defer_unrelated_preferences() {
        let config = Config::try_from(
            r#"{"windows": {
            "settings": {"bundle_id": "com.apple.systempreferences", "floating": true}
        }}"#,
        )
        .unwrap();
        let matched = config.match_window_rules(
            Some("Untitled"),
            Some(""),
            Some("AXWindow"),
            Some("AXStandardWindow"),
        );
        assert!(!matched.pending);
        assert!(matched.params.is_empty());
    }

    #[cfg(feature = "lua")]
    #[test]
    fn window_rules_shipped_preferences_are_editable_lua_not_core_defaults() {
        let lua = mlua::Lua::new();
        let source = format!(
            "local config; spool = {{setup = function(c) config = c end}}; {}\nreturn config",
            include_str!("../config/default.lua")
        );
        let value = lua.load(source).eval().unwrap();
        let shipped = config_from_lua(&lua, value).unwrap();
        let rules = shipped.match_window_rules(
            None,
            Some("com.apple.systempreferences"),
            Some("AXWindow"),
            Some("AXStandardWindow"),
        );
        assert!(!rules.pending);
        assert_eq!(rules.params.iter().find_map(|p| p.floating), Some(true));
        assert!(
            Config::default()
                .match_window_rules(
                    None,
                    Some("com.apple.systempreferences"),
                    Some("AXWindow"),
                    Some("AXStandardWindow")
                )
                .params
                .is_empty()
        );
    }
}

fn deserialize_modifier<'de, D>(deserializer: D) -> std::result::Result<Option<Modifiers>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(s) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_modifiers(&s)
        .map(Some)
        .map_err(|e: Error| serde::de::Error::custom(e.to_string()))
}

/// Builds a [`Config`] from the Lua table passed to `spool.setup{...}`,
/// Keybindings are registered separately through `spool.bind`.
///
/// # Errors
///
/// Returns an error if `value` is not a table, fails to deserialize, or contains
/// a non-finite numeric setting.
#[cfg(feature = "lua")]
pub(crate) fn config_from_lua(lua: &mlua::Lua, value: mlua::Value) -> mlua::Result<Config> {
    use mlua::LuaSerdeExt;

    if !value.is_table() {
        return Err(mlua::Error::RuntimeError(
            "spool.setup: expected a table".to_string(),
        ));
    }

    let mut inner: InnerConfig = lua.from_value(value)?;
    inner.validate().map_err(mlua::Error::RuntimeError)?;

    // Resolve window passthrough chords into keycodes. `parsed_passthrough` is `#[serde(skip)]`, so it starts empty.
    let needs_keys = inner.windows.as_ref().is_some_and(|windows| {
        windows
            .values()
            .any(|params| !params.bindings_passthrough.is_empty())
    });
    if needs_keys {
        // The primed keymap, not a fresh one: this runs on the Lua worker, and
        // generating it goes through Carbon/TIS. See [`prime_virtual_keymap`].
        let virtual_keys = virtual_keymap();
        if let Some(windows) = &mut inner.windows {
            for params in windows.values_mut() {
                for chord in &params.bindings_passthrough {
                    match resolve_keybinding_str(chord, virtual_keys) {
                        Ok(pair) => params.parsed_passthrough.push(pair),
                        Err(err) => error!("spool.setup passthrough: {err}"),
                    }
                }
            }
        }
    }

    Ok(Config {
        inner: Arc::new(ArcSwap::from_pointee(inner)),
    })
}

/// Resolves a keybinding chord string like `"ctrl+alt+h"` into a `(keycode, Modifiers)`
/// pair, generating the layout-aware virtual keymap on demand.
///
/// This is the entry point used by the Lua runtime's `spool.bind`, so scripted
/// keybinds use the same chord syntax as window passthrough rules.
#[cfg(feature = "lua")]
pub(crate) fn resolve_chord(input: &str) -> Result<(u8, Modifiers)> {
    // Fast path: avoids the virtual keymap for common chords, keeping this
    // testable headlessly.
    if let Ok(resolved) = resolve_keybinding_str(input, &[]) {
        return Ok(resolved);
    }
    resolve_keybinding_str(input, virtual_keymap())
}

/// The layout-aware virtual keymap, computed once.
///
/// `generate_virtual_keymap` goes through Carbon/TIS, which is main-thread/
/// GUI-session sensitive, but `spool.bind` runs on the Lua worker thread. So
/// the daemon primes this from the main thread at startup; the worker only
/// ever reads what was left behind.
#[cfg(feature = "lua")]
static VIRTUAL_KEYMAP: std::sync::OnceLock<Vec<(String, u8)>> = std::sync::OnceLock::new();

/// Computes the virtual keymap now, on the calling thread. Call once from the
/// main thread before anything can reach [`resolve_chord`].
#[cfg(feature = "lua")]
pub(crate) fn prime_virtual_keymap() {
    let _ = VIRTUAL_KEYMAP.set(generate_virtual_keymap());
}

/// The primed keymap. Falls back to computing it in place if priming never
/// happened (harmless in unit tests, rare elsewhere).
#[cfg(feature = "lua")]
fn virtual_keymap() -> &'static [(String, u8)] {
    VIRTUAL_KEYMAP.get_or_init(generate_virtual_keymap)
}

/// Resolves a keybinding string like `"ctrl+alt+h"` into a `(keycode, Modifiers)` pair.
#[cfg(feature = "lua")]
fn resolve_keybinding_str(input: &str, virtual_keys: &[(String, u8)]) -> Result<(u8, Modifiers)> {
    let (key, modifiers) = parse_chord_parts(input)?;

    let code = keycode_for_key_name(key, virtual_keys).ok_or_else(|| {
        Error::InvalidConfig(format!("Unknown key '{key}' in keybinding: {input:?}"))
    })?;

    Ok((code, modifiers))
}

/// Splits a chord whose last token is the physical key and whose preceding
/// tokens are modifiers. The single `+` separator keeps chord spelling
/// identical: `alt+shift+minus`.
#[cfg(feature = "lua")]
fn parse_chord_parts(input: &str) -> Result<(&str, Modifiers)> {
    if input.contains('-') {
        return Err(Error::InvalidConfig(format!(
            "Invalid keybinding {input:?}: use '+' between modifiers and the key"
        )));
    }

    let mut parts = input.split('+').map(str::trim).collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(Error::InvalidConfig(format!(
            "Invalid keybinding: {input:?}"
        )));
    }

    let key = parts
        .pop()
        .ok_or_else(|| Error::InvalidConfig("Empty keybinding string".to_string()))?;
    let modifiers = if parts.is_empty() {
        Modifiers::empty()
    } else {
        parse_modifiers(&parts.join("+"))?
    };
    Ok((key, modifiers))
}

#[cfg(feature = "lua")]
fn keycode_for_key_name(key: &str, virtual_keys: &[(String, u8)]) -> Option<u8> {
    virtual_keys
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, c)| *c)
        .or_else(|| virtual_keycode().find(|(k, _)| *k == key).map(|(_, c)| *c))
        .or_else(|| literal_keycode().find(|(k, _)| *k == key).map(|(_, c)| *c))
}

/// Parses a string containing modifier names (e.g., "alt", "shift", "cmd", "ctrl") separated by "+", and returns their combined bitmask.
///
/// # Arguments
///
/// * `input` - The string containing modifier names (e.g., "ctrl+alt").
///
/// # Returns
///
/// `Ok(Modifiers)` with the combined modifier bitmask if parsing is successful, otherwise `Err(String)` with an error message for an invalid modifier.
fn parse_modifiers(input: &str) -> Result<Modifiers> {
    let mut out = Modifiers::empty();

    let modifiers = input.split('+').map(str::trim).collect::<Vec<_>>();
    for modifier in &modifiers {
        out |= match *modifier {
            "alt" => Modifiers::ALT,
            "lalt" => Modifiers::LALT,
            "ralt" => Modifiers::RALT,
            "shift" => Modifiers::SHIFT,
            "lshift" => Modifiers::LSHIFT,
            "rshift" => Modifiers::RSHIFT,
            "cmd" => Modifiers::CMD,
            "lcmd" => Modifiers::LCMD,
            "rcmd" => Modifiers::RCMD,
            "ctrl" => Modifiers::CTRL,
            "lctrl" => Modifiers::LCTRL,
            "rctrl" => Modifiers::RCTRL,
            "fn" => Modifiers::FN,
            _ => {
                return Err(Error::InvalidConfig(format!(
                    "{}: Invalid modifier: {modifier}",
                    function_name!()
                )));
            }
        }
    }
    Ok(out)
}

#[cfg(feature = "lua")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// Returns a reference to the currently selected keyboard layout input source that is ASCII-capable.
    ///
    /// # Returns
    ///
    /// A raw pointer to the `TISInputSourceRef` (a `c_void` pointer) if successful, otherwise `null_mut()`.
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;

    /// Retrieves a specified property of an input source.
    ///
    /// # Arguments
    ///
    /// * `keyboard` - The raw pointer to the `TISInputSourceRef`.
    /// * `property` - The `CFStringRef` representing the property to retrieve (e.g., `kTISPropertyUnicodeKeyLayoutData`).
    ///
    /// # Returns
    ///
    /// A raw pointer to `CFData` containing the property value.
    fn TISGetInputSourceProperty(keyboard: *const c_void, property: CFStringRef) -> *mut CFData;

    /// Translates a virtual key code to a Unicode string according to the specified keyboard layout.
    ///
    /// # Arguments
    ///
    /// * `keyLayoutPtr` - A pointer to the keyboard layout data.
    /// * `virtualKeyCode` - The virtual key code to translate.
    /// * `keyAction` - The key action (e.g., `UCKeyAction::Down`).
    /// * `modifierKeyState` - The state of the modifier keys (e.g., `kUCKeyModifierAlphaLockBit`).
    /// * `keyboardType` - The type of keyboard, typically obtained from `LMGetKbdType()`.
    /// * `keyTranslateOptions` - Options for the translation process.
    /// * `deadKeyState` - A mutable reference to a `u32` representing the dead key state.
    /// * `maxStringLength` - The maximum length of the output Unicode string buffer.
    /// * `actualStringLength` - A mutable reference to an `isize` to store the actual length of the output Unicode string.
    /// * `unicodeString` - A mutable pointer to the buffer to store the resulting Unicode string.
    ///
    /// # Returns
    ///
    /// An `OSStatus` indicating success or failure.
    fn UCKeyTranslate(
        keyLayoutPtr: *mut u8,
        virtualKeyCode: u16,
        keyAction: u16,
        modifierKeyState: u32,
        keyboardType: u32,
        keyTranslateOptions: u32,
        deadKeyState: &mut u32,
        maxStringLength: usize,
        actualStringLength: &mut isize,
        unicodeString: *mut u16,
    ) -> OSStatus;

    /// Returns the keyboard type for the system.
    ///
    /// # Returns
    ///
    /// A `u8` representing the keyboard type.
    fn LMGetKbdType() -> u8;

    /// A constant `CFStringRef` representing the property key for Unicode keyboard layout data.
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;

}

/// Returns an iterator over static tuples of virtual key names and their corresponding keycodes.
/// These keycodes identify physical keys on an ANSI-standard US keyboard layout.
///
/// # Returns
///
/// An iterator yielding references to `(&'static str, u8)` tuples.
#[cfg(feature = "lua")]
fn virtual_keycode() -> impl Iterator<Item = &'static (&'static str, u8)> {
    /*
     *  Summary:
     *    Virtual keycodes
     *
     *  Discussion:
     *    These constants are the virtual keycodes defined originally in
     *    Inside Mac Volume V, pg. V-191. They identify physical keys on a
     *    keyboard. Those constants with "ANSI" in the name are labeled
     *    according to the key position on an ANSI-standard US keyboard.
     *    For example, kVK_ANSI_A indicates the virtual keycode for the key
     *    with the letter 'A' in the US keyboard layout. Other keyboard
     *    layouts may have the 'A' key label on a different physical key;
     *    in this case, pressing 'A' will generate a different virtual
     *    keycode.
     */
    static VIRTUAL_KEYCODE: LazyLock<Vec<(&'static str, u8)>> = LazyLock::new(|| {
        vec![
            ("a", 0x00),
            ("s", 0x01),
            ("d", 0x02),
            ("f", 0x03),
            ("h", 0x04),
            ("g", 0x05),
            ("z", 0x06),
            ("x", 0x07),
            ("c", 0x08),
            ("v", 0x09),
            ("section", 0x0a), // iso keyboards only.
            ("b", 0x0b),
            ("q", 0x0c),
            ("w", 0x0d),
            ("e", 0x0e),
            ("r", 0x0f),
            ("y", 0x10),
            ("t", 0x11),
            ("1", 0x12),
            ("2", 0x13),
            ("3", 0x14),
            ("4", 0x15),
            ("6", 0x16),
            ("5", 0x17),
            ("equal", 0x18),
            ("9", 0x19),
            ("7", 0x1a),
            ("minus", 0x1b),
            ("8", 0x1c),
            ("0", 0x1d),
            ("rightbracket", 0x1e),
            ("o", 0x1f),
            ("u", 0x20),
            ("leftbracket", 0x21),
            ("i", 0x22),
            ("p", 0x23),
            ("l", 0x25),
            ("j", 0x26),
            ("quote", 0x27),
            ("k", 0x28),
            ("semicolon", 0x29),
            ("backslash", 0x2a),
            ("comma", 0x2b),
            ("slash", 0x2c),
            ("n", 0x2d),
            ("m", 0x2e),
            ("period", 0x2f),
            ("grave", 0x32),
            ("keypaddecimal", 0x41),
            ("keypadmultiply", 0x43),
            ("keypadplus", 0x45),
            ("keypadclear", 0x47),
            ("keypaddivide", 0x4b),
            ("keypadenter", 0x4c),
            ("keypadminus", 0x4e),
            ("keypadequals", 0x51),
            ("keypad0", 0x52),
            ("keypad1", 0x53),
            ("keypad2", 0x54),
            ("keypad3", 0x55),
            ("keypad4", 0x56),
            ("keypad5", 0x57),
            ("keypad6", 0x58),
            ("keypad7", 0x59),
            ("keypad8", 0x5b),
            ("keypad9", 0x5c),
        ]
    });
    VIRTUAL_KEYCODE.iter()
}

/// Returns an iterator over static tuples of literal key names and their corresponding keycodes.
/// These keycodes are for keys that are independent of the keyboard layout (e.g., Return, Tab, Space).
///
/// # Returns
///
/// An iterator yielding references to `(&'static str, u8)` tuples.
#[cfg(feature = "lua")]
fn literal_keycode() -> impl Iterator<Item = &'static (&'static str, u8)> {
    /* keycodes for keys that are independent of keyboard layout*/
    static LITERAL_KEYCODE: LazyLock<Vec<(&'static str, u8)>> = LazyLock::new(|| {
        vec![
            ("return", 0x24),
            ("tab", 0x30),
            ("space", 0x31),
            ("delete", 0x33),
            ("escape", 0x35),
            ("command", 0x37),
            ("shift", 0x38),
            ("capslock", 0x39),
            ("option", 0x3a),
            ("control", 0x3b),
            ("rightcommand", 0x36),
            ("rightshift", 0x3c),
            ("rightoption", 0x3d),
            ("rightcontrol", 0x3e),
            ("function", 0x3f),
            ("f17", 0x40),
            ("volumeup", 0x48),
            ("volumedown", 0x49),
            ("mute", 0x4a),
            ("f18", 0x4f),
            ("f19", 0x50),
            ("f20", 0x5a),
            ("f5", 0x60),
            ("f6", 0x61),
            ("f7", 0x62),
            ("f3", 0x63),
            ("f8", 0x64),
            ("f9", 0x65),
            ("f11", 0x67),
            ("f13", 0x69),
            ("f16", 0x6a),
            ("f14", 0x6b),
            ("f10", 0x6d),
            ("contextualmenu", 0x6e),
            ("f12", 0x6f),
            ("f15", 0x71),
            ("help", 0x72),
            ("home", 0x73),
            ("pageup", 0x74),
            ("forwarddelete", 0x75),
            ("f4", 0x76),
            ("end", 0x77),
            ("f2", 0x78),
            ("pagedown", 0x79),
            ("f1", 0x7a),
            ("leftarrow", 0x7b),
            ("rightarrow", 0x7c),
            ("downarrow", 0x7d),
            ("uparrow", 0x7e),
        ]
    });
    LITERAL_KEYCODE.iter()
}

/// Represents the action of a key, used in `UCKeyTranslate`.
#[cfg(feature = "lua")]
enum UCKeyAction {
    /// The key is going down.
    Down = 0, // key is going down
              /*
              Up = 1,      // key is going up
              AutoKey = 2, // auto-key down
              Display = 3, // get information for key display (as in Key Caps)
              */
}

/// Generates a vector of (`key_name`, keycode) tuples for virtual keys based on the current ASCII-capable keyboard layout.
/// This involves using macOS Carbon API functions to translate virtual keycodes to Unicode characters.
///
/// # Returns
///
/// A `Vec<(String, u8)>` containing the translated key names and their keycodes. Returns an empty vector if an error occurs during keyboard layout fetching.
#[cfg(feature = "lua")]
fn generate_virtual_keymap() -> Vec<(String, u8)> {
    let keyboard = AXUIWrapper::from_retained(unsafe {
        TISCopyCurrentASCIICapableKeyboardLayoutInputSource()
    })
    .ok();
    let keyboard_layout = keyboard
        .and_then(|keyboard| {
            NonNull::new(unsafe {
                TISGetInputSourceProperty(
                    keyboard.as_ptr::<c_void>(),
                    kTISPropertyUnicodeKeyLayoutData,
                )
            })
        })
        .and_then(|uchr| NonNull::new(unsafe { CFData::byte_ptr(uchr.as_ref()).cast_mut() }));
    let Some(keyboard_layout) = keyboard_layout else {
        error!(
            "{}: problem fetching current virtual keyboard layout.",
            function_name!()
        );
        return vec![];
    };

    let mut state = 0u32;
    let mut chars = vec![0u16; 256];
    let mut got: isize = 0;
    virtual_keycode()
        .filter_map(|(_, keycode)| {
            unsafe {
                UCKeyTranslate(
                    keyboard_layout.as_ptr(),
                    (*keycode).into(),
                    UCKeyAction::Down as u16,
                    0,
                    LMGetKbdType().into(),
                    1,
                    &mut state,
                    chars.len(),
                    &mut got,
                    chars.as_mut_ptr(),
                )
            }
            .to_result(function_name!())
            .ok()
            .map(|()| {
                let name = unsafe { CFString::with_characters(None, chars.as_ptr(), got) }
                    .map(|chars| chars.to_string());
                name.zip(Some(*keycode))
            })
        })
        .flatten()
        .collect()
}

#[test]
fn test_parse_resize_commands() {
    assert!(matches!(
        parse_action(&["window", "grow", "width"]).unwrap(),
        Action::Window(Operation::Resize {
            axis: ResizeAxis::Width,
            direction: ResizeDirection::Grow
        })
    ));
    assert!(matches!(
        parse_action(&["window", "shrink", "width"]).unwrap(),
        Action::Window(Operation::Resize {
            axis: ResizeAxis::Width,
            direction: ResizeDirection::Shrink
        })
    ));
    assert!(matches!(
        parse_action(&["window", "grow", "height"]).unwrap(),
        Action::Window(Operation::Resize {
            axis: ResizeAxis::Height,
            direction: ResizeDirection::Grow
        })
    ));
    assert!(matches!(
        parse_action(&["window", "shrink", "height"]).unwrap(),
        Action::Window(Operation::Resize {
            axis: ResizeAxis::Height,
            direction: ResizeDirection::Shrink
        })
    ));
    assert!(parse_action(&["window", "resize"]).is_err());
    assert!(parse_action(&["window", "grow"]).is_err());
    assert!(parse_action(&["window", "shrink"]).is_err());
    assert!(matches!(
        parse_action(&["window", "shrink", "width"]).unwrap(),
        Action::Window(Operation::Resize {
            axis: ResizeAxis::Width,
            direction: ResizeDirection::Shrink
        })
    ));
}

#[test]
fn test_parse_restart_command() {
    assert!(matches!(
        parse_action(&["restart"]).unwrap(),
        Action::Restart
    ));
}

#[test]
#[allow(clippy::float_cmp)]
fn test_grid_ratios() {
    use regex::Regex;

    let make = |grid: Option<&str>| WindowParams {
        title: Some(Regex::new(".*").unwrap()),
        bundle_id: None,
        role: None,
        subrole: None,
        priority: 0,
        floating: None,
        track: None,
        index: None,
        vertical_padding: None,
        horizontal_padding: None,
        dont_focus: None,
        width: None,
        grid: grid.map(Into::into),
        border_radius: None,
        bindings_passthrough: vec![],
        parsed_passthrough: vec![],
    };

    // Standard 2x2 grid, cell (1,1), span 1x1 → bottom-right quarter.
    assert_eq!(
        make(Some("2:2:1:1:1:1")).grid_ratios(),
        Some((0.5, 0.5, 0.5, 0.5))
    );

    // 3x3 grid, cell (0,0), span 2x1 → top-left, 2/3 width, 1/3 height.
    assert_eq!(
        make(Some("3:3:0:0:2:1")).grid_ratios(),
        Some((0.0, 0.0, 2.0 / 3.0, 1.0 / 3.0))
    );

    // Full screen: 1x1 grid, cell (0,0), span 1x1.
    assert_eq!(
        make(Some("1:1:0:0:1:1")).grid_ratios(),
        Some((0.0, 0.0, 1.0, 1.0))
    );

    // Invalid: too few parts.
    assert_eq!(make(Some("2:2:1:1")).grid_ratios(), None);

    // Invalid: zero columns.
    assert_eq!(make(Some("0:2:0:0:1:1")).grid_ratios(), None);

    // No grid set.
    assert_eq!(make(None).grid_ratios(), None);
}

#[test]
fn test_parse_hex_color_valid() {
    assert_eq!(
        parse_hex_color("#89b4fa"),
        (
            f64::from(0x89) / 255.0,
            f64::from(0xb4) / 255.0,
            f64::from(0xfa) / 255.0
        )
    );
    assert_eq!(parse_hex_color("#000000"), (0.0, 0.0, 0.0));
    assert_eq!(parse_hex_color("#FFFFFF"), (1.0, 1.0, 1.0));
    assert_eq!(parse_hex_color("#FF0000"), (1.0, 0.0, 0.0));
}

#[test]
fn grid_rejects_invalid_fields_and_non_finite_ratios() {
    for grid in [
        "2:bad:2:1:1:1:1",
        "2::2:1:1:1:1",
        "NaN:2:0:0:1:1",
        "2:inf:0:0:1:1",
        "2:2:0:0:NaN:1",
        "5e-324:2:1:0:1:1",
    ] {
        let mut params = WindowParams::new(".*", None);
        params.grid = Some(grid.to_string());
        assert_eq!(params.grid_ratios(), None, "{grid}");
    }
}

#[test]
fn test_parse_hex_color_no_hash() {
    assert_eq!(
        parse_hex_color("89b4fa"),
        (
            f64::from(0x89) / 255.0,
            f64::from(0xb4) / 255.0,
            f64::from(0xfa) / 255.0
        )
    );
    assert_eq!(parse_hex_color("FF0000"), (1.0, 0.0, 0.0));
}

#[test]
fn test_parse_hex_color_invalid_length() {
    // Short strings fall back to white.
    assert_eq!(parse_hex_color("#FFF"), (1.0, 1.0, 1.0));
    assert_eq!(parse_hex_color(""), (1.0, 1.0, 1.0));
    assert_eq!(parse_hex_color("#FF"), (1.0, 1.0, 1.0));
}

#[test]
fn test_parse_hex_color_malformed_hex() {
    // Non-hex digits fall back to 255 per channel.
    assert_eq!(parse_hex_color("ZZZZZZ"), (1.0, 1.0, 1.0));
    assert_eq!(parse_hex_color("GG0000"), (1.0, 0.0, 0.0));
}

#[test]
fn configured_colors_handle_non_ascii_input_without_panicking() {
    for color in [
        "\u{4e2d}\u{6587}",
        "#\u{4e2d}\u{6587}",
        "\u{1f4a1}ab",
        "a\u{e9}bcd",
    ] {
        let config: Config = (
            MainOptions {
                border_color: Some(color.to_string()),
                dim_inactive_color: Some(color.to_string()),
                ..Default::default()
            },
            vec![],
        )
            .into();
        assert_eq!(config.border_color(), (1.0, 1.0, 1.0), "{color:?}");
        assert_eq!(config.dim_inactive_color(), (1.0, 1.0, 1.0), "{color:?}");
    }
}

#[test]
#[allow(clippy::float_cmp)]
fn test_config_defaults() {
    let config = Config::default();
    assert_eq!(config.dim_inactive_opacity(), 0.0);
    assert_eq!(config.dim_inactive_color(), (0.0, 0.0, 0.0));
    assert!(!config.border_active_window());
    assert_eq!(config.border_color(), (1.0, 1.0, 1.0));
    assert_eq!(config.border_opacity(), 1.0);
    assert_eq!(config.border_width(), 2.0);
    assert_eq!(config.border_radius(), BorderRadiusOption::Auto);
    assert_eq!(config.menubar_height(), None);
}

#[test]
fn test_window_rules_track() {
    let input = r#"{"windows": {
    "btt_main": {"bundle_id": "com.hegenberg.BetterTouchTool", "title": "BetterTouchTool", "track": true},
    "btt_floating": {"bundle_id": "com.hegenberg.BetterTouchTool", "title": "Screenshot.*", "floating": true}
}}"#;
    let config = Config::try_from(input).expect("config should parse");

    // Main window should match the canonical track rule.
    let props = config.find_window_properties("BetterTouchTool", "com.hegenberg.BetterTouchTool");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].track, Some(true));

    // Screenshot window matches the floating rule.
    let props = config.find_window_properties("Screenshot 1", "com.hegenberg.BetterTouchTool");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].floating, Some(true));
}

#[test]
fn test_restore_config_defaults() {
    let config = Config::try_from("{}").expect("config should parse");

    assert!(config.restore_enabled());
    assert_eq!(config.restore_startup_grace(), Duration::from_secs(2));
    assert_eq!(
        config.restore_missing_windows(),
        MissingWindowBehavior::Ignore
    );
}

#[test]
fn test_restore_config_explicit_values() {
    let config = Config::try_from(
        r#"{"restore": {"enabled": false, "startup_grace_ms": 750, "missing_windows": "ignore"}}"#,
    )
    .expect("config should parse");

    assert!(!config.restore_enabled());
    assert_eq!(config.restore_startup_grace(), Duration::from_millis(750));
    assert_eq!(
        config.restore_missing_windows(),
        MissingWindowBehavior::Ignore
    );
}

#[test]
fn test_restore_config_rejects_unsupported_missing_window_policy() {
    let err = Config::try_from(r#"{"restore": {"missing_windows": "reserve"}}"#)
        .expect_err("unsupported restore missing-window policy should fail");

    assert!(err.to_string().contains("unknown variant"));
}

#[cfg(all(test, feature = "lua"))]
mod lua_setup_tests {
    use super::*;
    use mlua::Lua;

    /// Runs a `spool.setup`-style table (as a Lua chunk that returns it) through
    /// the same `config_from_lua` path `spool.setup` uses.
    fn config_from_source(source: &str) -> Config {
        let lua = Lua::new();
        let value: mlua::Value = lua.load(source).eval().expect("lua chunk should evaluate");
        config_from_lua(&lua, value).expect("config_from_lua should succeed")
    }

    #[test]
    fn default_lua_adds_preferences_without_changing_other_builtin_defaults() {
        let source = format!(
            "local configured; spool = {{ setup = function(value) configured = value end }};\n{DEFAULT_LUA_SCRIPT}\nreturn configured"
        );
        let lua = Lua::new();
        let table: mlua::Table = lua.load(&source).eval().unwrap();
        let generated = config_from_lua(&lua, mlua::Value::Table(table.clone())).unwrap();
        let defaults = Config::default();
        assert_eq!(generated.bar_preferences(), defaults.bar_preferences());
        assert!(generated.inner().windows.is_some());
        assert!(defaults.inner().windows.is_none());
        table.set("windows", mlua::Nil).unwrap();
        let without_preferences = config_from_lua(&lua, mlua::Value::Table(table)).unwrap();
        assert_eq!(
            format!("{:?}", without_preferences.inner()),
            format!("{:?}", defaults.inner()),
        );
        let omitted = config_from_source("return {}");
        assert_eq!(
            omitted.preset_column_widths(),
            default_preset_column_widths()
        );
    }

    #[test]
    fn first_launch_creates_lua_without_overwriting_existing_configuration() {
        let directory = std::env::temp_dir().join(format!(
            "spool-default-lua-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let legacy = directory.join("spool.toml");
        std::fs::write(&legacy, "legacy configuration left untouched").unwrap();
        let path = directory.join("init.lua");
        assert!(create_lua_file_at(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_LUA_SCRIPT);
        let custom = "spool.setup { bar = { show_workspace_labels = false } }";
        std::fs::write(&path, custom).unwrap();
        assert!(!create_lua_file_at(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(
            std::fs::read_to_string(legacy).unwrap(),
            "legacy configuration left untouched"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bar_settings_share_configuration_and_reset_when_omitted() {
        let config = config_from_source(
            "return { options = { sliver_width = 9 }, bar = { show_workspace_labels = false, icon_size = 18 } }",
        );
        let shared = config.clone();
        assert!(!shared.bar_preferences().show_workspace_labels);
        assert_eq!(shared.bar_preferences().background_color, "#00000000");
        assert!((shared.bar_preferences().icon_size - 18.0).abs() < f64::EPSILON);
        config.replace_inner_from(&config_from_source("return {}"));
        assert_eq!(
            shared.bar_preferences(),
            crate::bar::BarPreferences::default()
        );
        assert_eq!(shared.sliver_width(), Config::default().sliver_width());
    }

    #[test]
    fn invalid_bar_settings_are_rejected() {
        let lua = Lua::new();
        let value = lua
            .load("return { bar = { show_workspace_labels = 'no' } }")
            .eval()
            .unwrap();
        assert!(config_from_lua(&lua, value).is_err());
        for source in [
            "return { bar = { corner_radius = 0/0 } }",
            "return { bar = { icon_size = math.huge } }",
            "return { bar = { handle_height = math.huge } }",
            "return { bar = { handle_radius = 0/0 } }",
        ] {
            let value = lua.load(source).eval().unwrap();
            assert!(config_from_lua(&lua, value).is_err());
        }
    }

    #[test]
    fn non_finite_settings_are_rejected_before_publication() {
        let lua = Lua::new();
        for field in [
            "swipe = { sensitivity = NUMBER }",
            "swipe = { deceleration = NUMBER }",
            "options = { swipe_sensitivity = NUMBER }",
            "options = { swipe_deceleration = NUMBER }",
            "options = { animation_speed = NUMBER }",
            "options = { sliver_height = NUMBER }",
            "options = { dim_inactive_windows = NUMBER }",
            "options = { border_opacity = NUMBER }",
            "options = { border_width = NUMBER }",
            "options = { border_radius = NUMBER }",
            "options = { window_hidden_ratio = NUMBER }",
            "options = { preset_column_widths = { 0.5, NUMBER, 2.0 } }",
            "decorations = { active = { border = { opacity = NUMBER } } }",
            "decorations = { active = { border = { width = NUMBER } } }",
            "decorations = { active = { border = { radius = NUMBER } } }",
            "decorations = { inactive = { dim = { opacity = NUMBER } } }",
            "decorations = { inactive = { dim = { opacity_night = NUMBER } } }",
            "decorations = { inactive = { border = { radius = NUMBER } } }",
            "decorations = { active = { dim = { opacity = NUMBER } } }",
            "windows = { example = { width = NUMBER } }",
            "windows = { example = { border_radius = NUMBER } }",
        ] {
            for number in ["0/0", "math.huge", "-math.huge"] {
                let source = format!("return {{ {} }}", field.replace("NUMBER", number));
                let value = lua.load(&source).eval().expect("valid Lua");
                assert!(
                    config_from_lua(&lua, value).is_err(),
                    "non-finite configuration was accepted: {source}"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "clamp endpoints are exact configuration constants"
    )]
    fn finite_settings_keep_existing_clamps_and_oversized_widths() {
        let config = config_from_source(
            r#"return {
                options = {
                    animation_speed = -1,
                    sliver_height = 0,
                    window_hidden_ratio = 2,
                    border_width = -1,
                    border_radius = "auto",
                    preset_column_widths = { 0.5, 1, 2 },
                },
                swipe = { sensitivity = -1, deceleration = 100 },
                windows = { example = { width = 2.0 } },
            }"#,
        );
        assert_eq!(config.animation_speed(), 0.0);
        assert_eq!(config.sliver_height(), 0.1);
        assert_eq!(config.window_hidden_ratio(), 1.0);
        assert_eq!(config.border_width(), 0.0);
        assert_eq!(config.swipe_sensitivity(), 0.1);
        assert_eq!(config.swipe_deceleration(), 10.0);
        assert_eq!(config.preset_column_widths(), vec![0.5, 1.0, 2.0]);
        assert_eq!(
            config.inner().windows.as_ref().unwrap()["example"].width,
            Some(2.0)
        );
    }

    #[test]
    fn bar_customization_is_loaded_through_lua() {
        let config = config_from_source(
            r##"return { bar = {
            notch_side = "left",
            icon_size = 20, label_font_size = 13, foreground_color = "#112233FF",
            inactive_workspace_color = "#00000010", workspace_corner_radius = 6,
            show_mission_control = false, show_desktop = true,
            handle_height = 12, handle_radius = 6,
        } }"##,
        );
        let preferences = config.bar_preferences();
        assert!(!preferences.show_mission_control);
        assert!(preferences.show_desktop);
        assert_eq!(preferences.foreground_color, "#112233FF");
        assert_eq!(preferences.inactive_workspace_color, "#00000010");
        assert!((preferences.toolbar_width() - 38.0).abs() < f64::EPSILON);
        // The handle is the one piece of Bar geometry with keys of its own.
        assert!((preferences.handle_height - 12.0).abs() < f64::EPSILON);
        assert!((preferences.handle_radius - 6.0).abs() < f64::EPSILON);
        let handle = preferences.handle_metrics();
        assert!((handle.height - 12.0).abs() < f64::EPSILON);
        assert!((handle.radius - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn retired_bar_geometry_keys_are_ignored_rather_than_rejected() {
        // The Bar now takes over the menu bar, so these keys have no meaning.
        // An existing init.lua that still sets them must keep loading.
        let config = config_from_source(
            r"return { bar = {
            embed_in_menu_bar = false, height = 40, top_offset = 8,
            max_width = 900, screen_padding = 20, show_workspace_labels = false,
        } }",
        );
        let preferences = config.bar_preferences();
        assert!(!preferences.show_workspace_labels);
        assert!(
            preferences.height.abs() < f64::EPSILON,
            "height comes from the menu bar"
        );
        assert_eq!(
            preferences,
            crate::bar::BarPreferences {
                show_workspace_labels: false,
                ..crate::bar::BarPreferences::default()
            }
        );
    }

    #[test]
    fn setup_table_populates_accessors() {
        let config = config_from_source(
            r"return {
                options = {
                    sliver_width = 9,
                    focus_follows_mouse = false,
                    experimental_space_control = true,
                    space_switch_animation = false,
                },
                padding = { top = 10, bottom = 4 },
            }",
        );
        assert_eq!(config.sliver_width(), 9);
        assert!(!config.focus_follows_mouse());
        assert!(config.space_control_enabled());
        assert!(!config.space_switch_animation());
        let (top, _right, bottom, _left) = config.edge_padding();
        assert_eq!((top, bottom), (10, 4));
    }

    #[test]
    fn missing_options_and_bindings_are_ok() {
        // Regression guard for the `#[serde(default)]` fix: a table that omits
        // `options` and `bindings` must still deserialize.
        let config = config_from_source(r"return { padding = { top = 4 } }");
        assert_eq!(config.edge_padding().0, 4);
        assert!(config.space_switch_animation());
    }

    #[test]
    fn window_rule_passthrough_is_resolved() {
        let config = config_from_source(
            r#"return {
                windows = { term = { title = "kitty", bindings_passthrough = { "ctrl+alt+h" } } },
            }"#,
        );
        let rules = config.find_window_properties("kitty", "");
        assert_eq!(rules.len(), 1);
        assert!(
            !rules[0].passthrough_keys().is_empty(),
            "passthrough chords should resolve to keycodes"
        );
    }

    #[test]
    fn non_table_argument_is_rejected() {
        let lua = Lua::new();
        assert!(config_from_lua(&lua, mlua::Value::Integer(3)).is_err());
    }
}

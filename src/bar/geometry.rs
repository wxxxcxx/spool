//! Per-display Bar panel geometry the user set by hand.
//!
//! [`super::placement`] stays authoritative for every display without an
//! override. A gesture on the Bar's grip or an edge records one, and this store
//! keeps it across runs. The file is versioned separately from the layout state
//! file, like the script store, because the two have nothing to say to each
//! other: this is presentation, not window topology.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bevy::ecs::resource::Resource;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

pub const BAR_STATE_FILE_NAME: &str = "bar-state.json";
const SUPPORTED_BAR_STATE_VERSION: u32 = 1;

/// One display's hand-set panel geometry.
///
/// A `None` field keeps the automatic value for that axis, so resizing the Bar
/// does not freeze the position it happened to have at that moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PanelOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

impl PanelOverride {
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.x.is_none() && self.y.is_none() && self.width.is_none() && self.height.is_none()
    }

    /// Drops values a gesture should never have produced. A non-finite number
    /// cannot be laid out, and a zero or negative size cannot be drawn, so they
    /// fall back to the automatic value instead of poisoning the placement.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let positive = |value: Option<f64>| value.filter(|value| value.is_finite() && *value > 0.0);
        Self {
            x: self.x.filter(|value| value.is_finite()),
            y: self.y.filter(|value| value.is_finite()),
            width: positive(self.width),
            height: positive(self.height),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SavedDisplayBar {
    display_id: u32,
    #[serde(flatten)]
    panel: PanelOverride,
}

/// The on-disk shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SavedBarState {
    version: u32,
    displays: Vec<SavedDisplayBar>,
}

/// The live store.
#[derive(Debug, Default, Resource)]
pub struct BarGeometryStore {
    displays: HashMap<u32, PanelOverride>,
    dirty: bool,
}

impl BarGeometryStore {
    /// The overrides saved by a previous run, or an empty store if there is no
    /// file, it cannot be read, or it was written by an incompatible version.
    #[must_use]
    pub fn load() -> Self {
        let path = Self::default_file_path();
        let Some(displays) = Self::read_file(&path) else {
            return Self::default();
        };
        debug!("Loaded Bar geometry from {}", path.display());
        Self {
            displays,
            dirty: false,
        }
    }

    fn read_file(path: &Path) -> Option<HashMap<u32, PanelOverride>> {
        let data = fs::read_to_string(path).ok()?;
        match serde_json::from_str::<SavedBarState>(&data) {
            Ok(saved) if saved.version == SUPPORTED_BAR_STATE_VERSION => Some(
                saved
                    .displays
                    .into_iter()
                    .map(|saved| (saved.display_id, saved.panel.sanitized()))
                    .filter(|(_, panel)| !panel.is_empty())
                    .collect(),
            ),
            Ok(saved) => {
                warn!(
                    "Ignoring Bar geometry at {}: version {}, expected {SUPPORTED_BAR_STATE_VERSION}",
                    path.display(),
                    saved.version
                );
                None
            }
            Err(err) => {
                warn!(
                    "Ignoring unreadable Bar geometry at {}: {err}",
                    path.display()
                );
                None
            }
        }
    }

    #[must_use]
    pub fn panel(&self, display_id: u32) -> Option<PanelOverride> {
        self.displays.get(&display_id).copied()
    }

    /// Records a gesture result. An override that carries no value at all is
    /// the same as clearing it, which is what double-clicking the grip does.
    pub fn set(&mut self, display_id: u32, panel: PanelOverride) {
        let panel = panel.sanitized();
        if panel.is_empty() {
            self.clear(display_id);
            return;
        }
        if self.displays.get(&display_id) == Some(&panel) {
            return;
        }
        self.displays.insert(display_id, panel);
        self.dirty = true;
    }

    pub fn clear(&mut self, display_id: u32) {
        if self.displays.remove(&display_id).is_some() {
            self.dirty = true;
        }
    }

    /// Writes the store out if anything has changed since the last save. Same
    /// write-to-temp-then-rename as the other state files, so a crash mid-save
    /// leaves the previous file intact rather than a truncated one.
    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let path = Self::default_file_path();
        match self.write_file(&path) {
            Ok(()) => {
                self.dirty = false;
                debug!("Bar geometry saved to {}", path.display());
            }
            Err(err) => error!("Failed to save Bar geometry to {}: {err}", path.display()),
        }
    }

    fn write_file(&self, path: &Path) -> Result<(), std::io::Error> {
        let mut displays = self
            .displays
            .iter()
            .map(|(display_id, panel)| SavedDisplayBar {
                display_id: *display_id,
                panel: *panel,
            })
            .collect::<Vec<_>>();
        displays.sort_by_key(|saved| saved.display_id);
        let saved = SavedBarState {
            version: SUPPORTED_BAR_STATE_VERSION,
            displays,
        };
        let json = serde_json::to_string_pretty(&saved).map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, json)?;
        fs::rename(tmp_path, path)?;
        Ok(())
    }

    #[must_use]
    pub fn default_file_path() -> PathBuf {
        xdg::BaseDirectories::with_prefix("spool")
            .get_state_file(BAR_STATE_FILE_NAME)
            .expect("XDG state directory should be available")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spool-bar-geometry-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn override_round_trips_per_display() {
        let path = unique_path("round-trip");
        let mut store = BarGeometryStore::default();
        store.set(
            2,
            PanelOverride {
                x: Some(120.0),
                width: Some(600.0),
                ..PanelOverride::default()
            },
        );
        store.set(
            1,
            PanelOverride {
                y: Some(44.0),
                height: Some(48.0),
                ..PanelOverride::default()
            },
        );
        store.write_file(&path).expect("write should succeed");

        let restored = BarGeometryStore::read_file(&path).expect("file should parse");
        assert_eq!(
            restored.get(&2),
            Some(&PanelOverride {
                x: Some(120.0),
                width: Some(600.0),
                ..PanelOverride::default()
            })
        );
        assert_eq!(
            restored.get(&1),
            Some(&PanelOverride {
                y: Some(44.0),
                height: Some(48.0),
                ..PanelOverride::default()
            })
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unset_fields_stay_automatic_across_a_reload() {
        let path = unique_path("partial");
        let mut store = BarGeometryStore::default();
        store.set(
            3,
            PanelOverride {
                width: Some(400.0),
                ..PanelOverride::default()
            },
        );
        store.write_file(&path).expect("write should succeed");
        let text = fs::read_to_string(&path).expect("file should exist");
        assert!(
            !text.contains("\"x\""),
            "unset axes must not be frozen: {text}"
        );
        let restored = BarGeometryStore::read_file(&path).expect("file should parse");
        assert_eq!(
            restored.get(&3).and_then(|panel| panel.x),
            None,
            "a width-only override keeps the automatic position"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_and_non_finite_values_fall_back_to_automatic() {
        let mut store = BarGeometryStore::default();
        store.set(
            1,
            PanelOverride {
                x: Some(f64::NAN),
                width: Some(-10.0),
                height: Some(0.0),
                ..PanelOverride::default()
            },
        );
        assert_eq!(store.panel(1), None, "nothing usable was recorded");

        store.set(
            1,
            PanelOverride {
                x: Some(f64::INFINITY),
                width: Some(320.0),
                ..PanelOverride::default()
            },
        );
        assert_eq!(
            store.panel(1),
            Some(PanelOverride {
                width: Some(320.0),
                ..PanelOverride::default()
            })
        );
    }

    #[test]
    fn clearing_a_display_marks_the_store_dirty_once() {
        let mut store = BarGeometryStore::default();
        store.set(
            1,
            PanelOverride {
                x: Some(10.0),
                ..PanelOverride::default()
            },
        );
        assert!(store.dirty);
        store.dirty = false;
        store.clear(1);
        assert!(store.dirty);
        assert_eq!(store.panel(1), None);
        store.dirty = false;
        store.clear(1);
        assert!(!store.dirty, "clearing an unknown display changes nothing");
    }

    #[test]
    fn a_file_from_another_version_is_ignored() {
        let path = unique_path("version");
        fs::write(
            &path,
            r#"{"version":99,"displays":[{"display_id":1,"x":10.0}]}"#,
        )
        .expect("write should succeed");
        assert!(BarGeometryStore::read_file(&path).is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn setting_the_same_geometry_twice_does_not_dirty_the_store() {
        let mut store = BarGeometryStore::default();
        let panel = PanelOverride {
            x: Some(10.0),
            ..PanelOverride::default()
        };
        store.set(1, panel);
        store.dirty = false;
        store.set(1, panel);
        assert!(!store.dirty, "a repeated gesture result is not a change");
    }
}

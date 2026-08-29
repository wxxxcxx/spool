use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::app::AppExit;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Query, Res, SystemParam};
use bevy::math::IRect;
use objc2_core_graphics::CGDirectDisplayID;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::native_space::{NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::params::Windows;
use crate::ecs::{ActiveDisplayMarker, ActiveWorkspaceMarker};
use crate::manager::{Application, Display, WindowManager};
use crate::platform::{Pid, ProcessSerialNumber, WinID, WorkspaceId};
use spool_shared_types::windowset::WindowSet;

pub const STATE_FILE_NAME: &str = "state.json";
const LEGACY_STATE_VERSION: u32 = 2;
const SUPPORTED_STATE_VERSION: u32 = 3;
const LEGACY_BACKUP_FILE_NAME: &str = "state.v2.backup.json";

#[derive(Clone, Debug, Serialize)]
pub struct StateMigrationReport {
    pub source_version: u32,
    pub needs_migration: bool,
    pub native_spaces: usize,
    pub virtual_rows: usize,
    pub backup_path: PathBuf,
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Resource)]
pub struct SpoolState {
    pub version: u32,
    pub timestamp: u64,
    pub active_display_id: Option<CGDirectDisplayID>,
    #[serde(default)]
    pub displays: Vec<SavedDisplay>,
    pub spaces: Vec<SavedSpace>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedDisplay {
    pub display_id: CGDirectDisplayID,
    pub bounds: SavedRect,
    pub active: bool,
    pub space_ids: Vec<WorkspaceId>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedSpace {
    pub space_id: WorkspaceId,
    pub display_id: Option<CGDirectDisplayID>,
    /// Per-display order at save time; a restore hint, never a stable key.
    pub ordinal: Option<u32>,
    pub kind: SpaceKind,
    pub active: bool,
    pub columns: Vec<SavedColumn>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacySpoolStateV2 {
    version: u32,
    timestamp: u64,
    active_display_id: Option<CGDirectDisplayID>,
    #[serde(default)]
    displays: Vec<LegacySavedDisplayV2>,
    workspaces: Vec<LegacySavedWorkspaceV2>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacySavedDisplayV2 {
    display_id: CGDirectDisplayID,
    bounds: SavedRect,
    active: bool,
    workspace_ids: Vec<WorkspaceId>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacySavedWorkspaceV2 {
    workspace_id: WorkspaceId,
    display_id: Option<CGDirectDisplayID>,
    #[allow(dead_code)]
    active_virtual_index: Option<u32>,
    strips: Vec<LegacySavedStripV2>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacySavedStripV2 {
    virtual_index: u32,
    columns: Vec<SavedColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SavedColumn {
    Single(SavedWindow),
    Stack(Vec<SavedStackItem>),
    Tabs(Vec<SavedWindow>),
    Fullscreen(SavedWindow),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SavedStackItem {
    Single(SavedWindow),
    Tabs(Vec<SavedWindow>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedWindow {
    // Primary matching (stable across WM restarts)
    pub window_id: WinID,
    pub pid: Pid,
    pub psn: ProcessSerialNumber,

    // Heuristic matching (if IDs change or apps restarted)
    pub bundle_id: String,
    pub title: String,
    pub identifier: String,
    pub role: String,
    pub subrole: String,
}

// These wire-format types live in the shared `spool_shared_types` crate;
// aliased here to the names the rest of the daemon already uses.
pub use spool_shared_types::state::{
    ActiveState as SpoolActiveState, DisplayState as SpoolDisplayState, Frame,
    QueryState as SpoolQueryState, SpaceCapabilities, SpaceKind, SpaceState as SpoolSpaceState,
    StateEvent, StateQueryKind, WindowState as SpoolWindowState,
};

/// Resolves which display a window frame is on and whether more than a sliver of
/// it is showing there. Returns `None` when the frame misses every display.
fn window_visibility(
    frame: IRect,
    displays: &Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    sliver_width: i32,
) -> Option<(CGDirectDisplayID, bool)> {
    displays
        .iter()
        // Pick the display showing the largest slice of the window.
        .map(|(display, _, _)| {
            let overlap = frame.intersect(display.bounds());
            let (width, height) = (overlap.width().max(0), overlap.height().max(0));
            (display.id(), width, height)
        })
        .filter(|(_, width, height)| *width > 0 && *height > 0)
        .max_by_key(|(_, width, height)| i64::from(*width) * i64::from(*height))
        .map(|(display_id, width, height)| (display_id, width > sliver_width && height > 0))
}

impl From<IRect> for SavedRect {
    fn from(rect: IRect) -> Self {
        Self {
            min_x: rect.min.x,
            min_y: rect.min.y,
            max_x: rect.max.x,
            max_y: rect.max.y,
        }
    }
}

impl SavedWindow {
    pub fn from_entity(
        entity: Entity,
        windows: &Windows,
        apps: &Query<&Application>,
    ) -> Option<Self> {
        let window = windows.get(entity)?;
        let (_, _, app_entity) = windows.find_parent(window.id())?;
        let app = apps.get(app_entity).ok()?;

        Some(Self {
            window_id: window.id(),
            pid: window.pid().ok()?,
            psn: app.psn(),
            bundle_id: app.bundle_id().unwrap_or_default().clone(),
            title: window.title().unwrap_or_default(),
            identifier: window.identifier().unwrap_or_default(),
            role: window.role().unwrap_or_default(),
            subrole: window.subrole().unwrap_or_default(),
        })
    }

    pub fn hard_match(&self, other_id: WinID, other_proc_id: Pid, other_bundle: &str) -> bool {
        // 1. Exact match (including bundle to avoid cross-app PID collisions in edge cases)
        self.window_id == other_id && self.pid == other_proc_id && self.bundle_id == other_bundle
    }
}

impl SpoolState {
    #[allow(clippy::too_many_lines)]
    pub fn extract(
        workspaces: &Query<(
            Option<&ChildOf>,
            &LayoutStrip,
            &NativeSpace,
            Has<ActiveWorkspaceMarker>,
        )>,
        displays: &Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
        windows: &Windows,
        apps: &Query<&Application>,
    ) -> Self {
        let mut display_entity_ids = HashMap::new();
        let mut display_space_ids: HashMap<Entity, Vec<WorkspaceId>> = HashMap::new();
        let mut space_map: HashMap<WorkspaceId, SavedSpaceBuilder> = HashMap::new();
        let active_display_id = displays
            .iter()
            .find(|(_, _, active)| *active)
            .map(|(display, _, _)| display.id());

        for (display, entity, _) in displays {
            display_entity_ids.insert(entity, display.id());
            display_space_ids.insert(entity, Vec::new());
        }

        for (child, strip, native_space, active_workspace) in workspaces {
            let display_entity = child.map(ChildOf::parent);
            let display_id =
                display_entity.and_then(|entity| display_entity_ids.get(&entity).copied());
            if let Some(entity) = display_entity
                && let Some(space_ids) = display_space_ids.get_mut(&entity)
                && !space_ids.contains(&strip.id())
            {
                space_ids.push(strip.id());
            }

            let mut saved_columns = Vec::new();
            for col in strip.columns() {
                let saved_col = match col {
                    Column::Single(entity) => {
                        SavedWindow::from_entity(*entity, windows, apps).map(SavedColumn::Single)
                    }
                    Column::Stack(items) => {
                        let saved_items = items
                            .iter()
                            .filter_map(|item| match item {
                                StackItem::Single(entity) => {
                                    SavedWindow::from_entity(*entity, windows, apps)
                                        .map(SavedStackItem::Single)
                                }
                                StackItem::Tabs(tabs) => {
                                    let saved_tabs: Vec<_> = tabs
                                        .iter()
                                        .filter_map(|&e| SavedWindow::from_entity(e, windows, apps))
                                        .collect();
                                    if saved_tabs.is_empty() {
                                        None
                                    } else {
                                        Some(SavedStackItem::Tabs(saved_tabs))
                                    }
                                }
                            })
                            .collect::<Vec<_>>();
                        if saved_items.is_empty() {
                            None
                        } else {
                            Some(SavedColumn::Stack(saved_items))
                        }
                    }
                    Column::Tabs(tabs) => {
                        let saved_tabs: Vec<_> = tabs
                            .iter()
                            .filter_map(|&e| SavedWindow::from_entity(e, windows, apps))
                            .collect();
                        if saved_tabs.is_empty() {
                            None
                        } else {
                            Some(SavedColumn::Tabs(saved_tabs))
                        }
                    }
                    Column::Fullscren(entity) => SavedWindow::from_entity(*entity, windows, apps)
                        .map(SavedColumn::Fullscreen),
                };

                if let Some(sc) = saved_col {
                    saved_columns.push(sc);
                }
            }

            let space = space_map
                .entry(strip.id())
                .or_insert_with(|| SavedSpaceBuilder {
                    display_id,
                    ordinal: native_space.ordinal,
                    kind: native_space.kind,
                    active: false,
                    columns: Vec::new(),
                });
            if space.display_id.is_none() {
                space.display_id = display_id;
            }
            if active_workspace {
                space.active = true;
            }
            space.columns.extend(saved_columns);
        }

        let spaces = space_map
            .into_iter()
            .map(|(space_id, space)| SavedSpace {
                space_id,
                display_id: space.display_id,
                ordinal: Some(space.ordinal),
                kind: space.kind,
                active: space.active,
                columns: space.columns,
            })
            .collect();
        let displays = displays
            .iter()
            .map(|(display, entity, active)| SavedDisplay {
                display_id: display.id(),
                bounds: display.bounds().into(),
                active,
                space_ids: display_space_ids.remove(&entity).unwrap_or_default(),
            })
            .collect();

        Self {
            version: SUPPORTED_STATE_VERSION,
            timestamp: now_timestamp(),
            active_display_id,
            displays,
            spaces,
        }
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            error!("Failed to serialize state: {e}");
            std::io::Error::other(e)
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, json)?;
        fs::rename(tmp_path, path)?;
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        let version = serde_json::from_str::<serde_json::Value>(&data)
            .ok()?
            .get("version")?
            .as_u64()?;
        match u32::try_from(version).ok()? {
            SUPPORTED_STATE_VERSION => serde_json::from_str(&data).ok(),
            LEGACY_STATE_VERSION => {
                let backup = path.with_file_name(LEGACY_BACKUP_FILE_NAME);
                if !backup.exists()
                    && let Err(error) = fs::write(&backup, &data)
                {
                    error!(%error, path = %backup.display(), "unable to back up v2 state");
                    return None;
                }
                let legacy: LegacySpoolStateV2 = serde_json::from_str(&data).ok()?;
                let state = migrate_v2_state(legacy);
                info!(
                    backup = %backup.display(),
                    "loaded v2 state using safe Space fold"
                );
                Some(state)
            }
            _ => None,
        }
    }

    pub fn default_state_file_path() -> PathBuf {
        xdg::BaseDirectories::with_prefix("spool")
            .get_state_file(STATE_FILE_NAME)
            .expect("XDG state directory should be available")
    }

    pub fn migrate_file(path: &Path, apply: bool) -> crate::errors::Result<StateMigrationReport> {
        let data = fs::read_to_string(path)?;
        let version = serde_json::from_str::<serde_json::Value>(&data)?
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| crate::errors::Error::InvalidInput("state version is missing".into()))?;
        let backup_path = path.with_file_name(LEGACY_BACKUP_FILE_NAME);
        if version == SUPPORTED_STATE_VERSION {
            let state: Self = serde_json::from_str(&data)?;
            return Ok(StateMigrationReport {
                source_version: version,
                needs_migration: false,
                native_spaces: state.spaces.len(),
                virtual_rows: 0,
                backup_path,
                applied: false,
            });
        }
        if version != LEGACY_STATE_VERSION {
            return Err(crate::errors::Error::InvalidInput(format!(
                "unsupported state version {version}"
            )));
        }
        let legacy: LegacySpoolStateV2 = serde_json::from_str(&data)?;
        let native_spaces = legacy.workspaces.len();
        let virtual_rows = legacy
            .workspaces
            .iter()
            .map(|workspace| workspace.strips.len())
            .sum();
        if apply {
            if !backup_path.exists() {
                fs::write(&backup_path, &data)?;
            }
            migrate_v2_state(legacy).save_to_file(path)?;
        }
        Ok(StateMigrationReport {
            source_version: version,
            needs_migration: true,
            native_spaces,
            virtual_rows,
            backup_path,
            applied: apply,
        })
    }
}

fn migrate_v2_state(legacy: LegacySpoolStateV2) -> SpoolState {
    debug_assert_eq!(legacy.version, LEGACY_STATE_VERSION);
    let displays = legacy
        .displays
        .iter()
        .map(|display| SavedDisplay {
            display_id: display.display_id,
            bounds: display.bounds,
            active: display.active,
            space_ids: display.workspace_ids.clone(),
        })
        .collect::<Vec<_>>();
    let spaces = legacy
        .workspaces
        .into_iter()
        .map(|workspace| {
            let ordinal = workspace.display_id.and_then(|display_id| {
                legacy
                    .displays
                    .iter()
                    .find(|display| display.display_id == display_id)
                    .and_then(|display| {
                        display
                            .workspace_ids
                            .iter()
                            .position(|space_id| *space_id == workspace.workspace_id)
                    })
                    .and_then(|ordinal| ordinal.try_into().ok())
            });
            let mut strips = workspace.strips;
            strips.sort_by_key(|strip| strip.virtual_index);
            let columns = strips
                .into_iter()
                .flat_map(|strip| strip.columns)
                .collect::<Vec<_>>();
            let kind = if columns
                .iter()
                .any(|column| matches!(column, SavedColumn::Fullscreen(_)))
            {
                SpaceKind::Fullscreen
            } else {
                SpaceKind::User
            };
            SavedSpace {
                space_id: workspace.workspace_id,
                display_id: workspace.display_id,
                ordinal,
                kind,
                active: workspace.active_virtual_index.is_some(),
                columns,
            }
        })
        .collect();
    SpoolState {
        version: SUPPORTED_STATE_VERSION,
        timestamp: legacy.timestamp,
        active_display_id: legacy.active_display_id,
        displays,
        spaces,
    }
}

struct SavedSpaceBuilder {
    display_id: Option<CGDirectDisplayID>,
    ordinal: u32,
    kind: SpaceKind,
    active: bool,
    columns: Vec<SavedColumn>,
}

/// The world access [`QueryState::extract`] needs, bundled so callers (the
/// socket query handler, the embedded Lua runtime) take one parameter instead
/// of six.
#[derive(SystemParam)]
pub struct QueryStateParams<'w, 's> {
    workspaces: Query<
        'w,
        's,
        (
            &'static ChildOf,
            &'static LayoutStrip,
            Has<ActiveWorkspaceMarker>,
            Has<VisibleNativeSpaceMarker>,
        ),
    >,
    displays: Query<'w, 's, (&'static Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Windows<'w, 's>,
    apps: Query<'w, 's, &'static Application>,
    window_manager: Res<'w, WindowManager>,
    config: Res<'w, Config>,
}

impl QueryStateParams<'_, '_> {
    /// The window queries backing the extract, for callers that also need to
    /// look a window up directly rather than through the state document.
    pub fn windows(&self) -> &Windows<'_, '_> {
        &self.windows
    }

    /// Builds the state document from the current world.
    ///
    /// # Errors
    ///
    /// Returns an error if the window manager cannot enumerate a workspace.
    pub fn extract(&self) -> crate::errors::Result<SpoolQueryState> {
        SpoolQueryState::extract(
            &self.workspaces,
            &self.displays,
            &self.windows,
            &self.apps,
            &self.window_manager,
            &self.config,
        )
    }
}

/// Reads the layout as the tree a script transforms. Unlike
/// [`QueryState::extract`], which flattens each workspace into a list of
/// windows, this keeps the strip's column structure — needed for `ws:swap`,
/// `ws:east`, `ws:stack` and friends to know what is beside what.
impl QueryStateParams<'_, '_> {
    pub fn extract_window_set(&self) -> crate::errors::Result<WindowSet> {
        use spool_shared_types::windowset::{ColumnSet, DisplaySet, WorkspaceSet};

        let focused_entity = self.windows.focused().map(|(_, entity)| entity);
        let sliver_width = self.config.sliver_width();
        let active_workspace_id = self
            .workspaces
            .iter()
            .find_map(|(_, strip, active, _)| active.then_some(strip.id()));

        // Group the workspace strips by the display entity that owns them, so
        // each display can be built with its own workspaces in one pass.
        let mut strips_by_display: HashMap<Entity, Vec<WorkspaceSet>> = HashMap::new();
        for (child, strip, active_workspace, visible_space) in self.workspaces {
            // Only ask for floating windows on a workspace that's actually
            // showing, since this read goes out to the window server.
            let floating_entities =
                if active_workspace || visible_space && active_workspace_id != Some(strip.id()) {
                    self.window_manager.windows_in_workspace(strip.id())?
                } else {
                    Vec::new()
                };

            // A window stays tracked by the strip after it's floated (so it can
            // be re-tiled later), so it's the `Floating` marker — not strip
            // membership — that decides whether it goes in `columns` or `floating`.
            let mut floating = Vec::new();
            let mut columns: Vec<ColumnSet> = Vec::new();
            for column in strip.columns() {
                let mut tiled = Vec::new();
                for entity in column.window_iter() {
                    let Some(record) = self.window_record(entity, focused_entity, sliver_width)
                    else {
                        continue;
                    };
                    if record.floating {
                        floating.push(record);
                    } else {
                        tiled.push(record);
                    }
                }
                if tiled.is_empty() {
                    continue;
                }
                let width_ratio = column
                    .window_iter()
                    .find_map(|entity| self.windows.width_ratio(entity))
                    .unwrap_or(1.0);
                let selected = column
                    .top()
                    .and_then(|top| self.windows.get(top).map(|window| window.id()))
                    .and_then(|id| tiled.iter().position(|window| window.id == id))
                    .unwrap_or(0);
                columns.push(ColumnSet {
                    kind: column_kind(column),
                    width_ratio,
                    selected,
                    windows: std::sync::Arc::new(tiled),
                });
            }

            // Floating windows the strip never knew about.
            floating.extend(
                floating_entities
                    .into_iter()
                    .filter_map(|window_id| {
                        let (_, entity) = self.windows.find(window_id)?;
                        let (_, _, state) = self.windows.get_tracked(entity)?;
                        (state.is_floating() && state.is_visible() && !strip.contains(entity))
                            .then_some(entity)
                    })
                    .filter_map(|entity| self.window_record(entity, focused_entity, sliver_width)),
            );

            strips_by_display
                .entry(child.parent())
                .or_default()
                .push(WorkspaceSet {
                    space_id: strip.id(),
                    ordinal: 0,
                    active: active_workspace,
                    columns: std::sync::Arc::new(columns),
                    floating: std::sync::Arc::new(floating),
                });
        }

        let displays = self
            .displays
            .iter()
            .map(|(display, entity, active)| {
                let bounds = display.bounds();
                let mut workspaces = strips_by_display.remove(&entity).unwrap_or_default();
                workspaces.sort_by_key(|workspace| workspace.ordinal);
                DisplaySet {
                    id: display.id(),
                    frame: Frame {
                        x: bounds.min.x,
                        y: bounds.min.y,
                        width: bounds.width(),
                        height: bounds.height(),
                    },
                    active,
                    workspaces: std::sync::Arc::new(workspaces),
                }
            })
            .collect();

        let focused = focused_entity
            .and_then(|entity| self.windows.get(entity))
            .map(|window| window.id());
        Ok(WindowSet::new(displays, focused))
    }

    /// One window, as a script sees it. `None` for an entity that is no longer
    /// a window we know anything about.
    fn window_record(
        &self,
        entity: Entity,
        focused: Option<Entity>,
        sliver_width: i32,
    ) -> Option<spool_shared_types::windowset::WindowRec> {
        let (window, _, state) = self.windows.get_tracked(entity)?;
        let (_, _, app_entity) = self.windows.find_parent(window.id())?;
        let app = self.apps.get(app_entity).ok()?;
        let frame = self.windows.frame(entity);
        // Minimized and hidden windows are never on screen, whatever their last
        // known frame says.
        let hidden = !state.is_visible();
        let visible = frame
            .and_then(|frame| window_visibility(frame, &self.displays, sliver_width))
            .is_some_and(|(_, visible)| visible && !hidden);

        Some(spool_shared_types::windowset::WindowRec {
            id: window.id(),
            app_name: app.name().to_string(),
            bundle_id: app.bundle_id().unwrap_or_default().clone(),
            title: window.title().unwrap_or_default(),
            frame: frame.map(|frame| Frame {
                x: frame.min.x,
                y: frame.min.y,
                width: frame.width(),
                height: frame.height(),
            }),
            floating: state.is_floating(),
            visible,
            focused: focused == Some(entity),
        })
    }
}

/// How a layout column arranges its windows, in the vocabulary a script sees.
fn column_kind(column: &Column) -> spool_shared_types::windowset::ColumnKind {
    use spool_shared_types::windowset::ColumnKind;
    match column {
        Column::Single(_) => ColumnKind::Single,
        Column::Stack(_) => ColumnKind::Stack,
        Column::Tabs(_) => ColumnKind::Tabs,
        Column::Fullscren(_) => ColumnKind::Fullscreen,
    }
}

pub trait QueryState: std::marker::Sized {
    fn extract(
        workspaces: &Query<(
            &ChildOf,
            &LayoutStrip,
            Has<ActiveWorkspaceMarker>,
            Has<VisibleNativeSpaceMarker>,
        )>,
        displays: &Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
        windows: &Windows,
        apps: &Query<&Application>,
        window_manager: &WindowManager,
        config: &Config,
    ) -> crate::errors::Result<Self>;
}

/// Builds the query/subscribe state document from the ECS world.
///
/// A free function rather than an inherent method because [`SpoolQueryState`]
/// belongs to the shared protocol crate, which knows nothing about the ECS.
impl QueryState for SpoolQueryState {
    #[allow(clippy::too_many_lines)]
    fn extract(
        workspaces: &Query<(
            &ChildOf,
            &LayoutStrip,
            Has<ActiveWorkspaceMarker>,
            Has<VisibleNativeSpaceMarker>,
        )>,
        displays: &Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
        windows: &Windows,
        apps: &Query<&Application>,
        window_manager: &WindowManager,
        config: &Config,
    ) -> crate::errors::Result<Self> {
        let focused_entity = windows.focused().map(|(_, entity)| entity);
        let sliver_width = config.sliver_width();

        let active_display = displays
            .iter()
            .find_map(|(display, entity, active)| active.then_some((display.id(), entity)));
        let active_workspace_id = workspaces
            .iter()
            .find_map(|(_, strip, active, _)| active.then_some(strip.id()));

        let mut windows_by_space: HashMap<WorkspaceId, Vec<SpoolWindowState>> = HashMap::new();
        let mut active = SpoolActiveState {
            display_id: active_display.map(|(display_id, _)| display_id),
            ..SpoolActiveState::default()
        };

        for (child, strip, active_workspace, visible_space) in workspaces {
            let floating =
                if active_workspace || visible_space && active_workspace_id != Some(strip.id()) {
                    window_manager.windows_in_workspace(strip.id())?
                } else {
                    Vec::new()
                }
                .into_iter()
                .filter_map(|window_id| {
                    let (_, entity) = windows.find(window_id)?;
                    let (_, _, state) = windows.get_tracked(entity)?;
                    (state.is_floating() && state.is_visible() && !strip.contains(entity))
                        .then_some(entity)
                });
            let row_windows = strip
                .all_windows()
                .into_iter()
                .chain(floating)
                .filter_map(|entity| {
                    let (window, _, state) = windows.get_tracked(entity)?;
                    let (_, _, app_entity) = windows.find_parent(window.id())?;
                    let app = apps.get(app_entity).ok()?;
                    let bundle_id = app.bundle_id().unwrap_or_default().clone();
                    let app_name = app.name().to_string();
                    let title = window.title().unwrap_or_default();
                    let frame = windows.frame(entity);
                    // Minimized and hidden windows are never on screen, whatever
                    // their last known frame says.
                    let hidden = !state.is_visible();
                    let visibility = frame
                        .and_then(|frame| window_visibility(frame, displays, sliver_width))
                        .map(|(display_id, visible)| (display_id, visible && !hidden));
                    Some(SpoolWindowState {
                        window_id: window.id(),
                        bundle_id,
                        app_name,
                        title,
                        focused: focused_entity == Some(entity),
                        floating: state.is_floating(),
                        display_id: visibility.map(|(display_id, _)| display_id),
                        frame: frame.map(|frame| Frame {
                            x: frame.min.x,
                            y: frame.min.y,
                            width: frame.width(),
                            height: frame.height(),
                        }),
                        visible: visibility.is_some_and(|(_, visible)| visible),
                    })
                })
                .collect::<Vec<_>>();

            if active_workspace {
                active.space_id = Some(strip.id());
            }

            if active_workspace
                && let Some(window) = row_windows.iter().find(|window| window.focused)
            {
                active.focused_window_id = Some(window.window_id);
                active.focused_bundle_id = Some(window.bundle_id.clone());
                active.focused_app_name = Some(window.app_name.clone());
                active.focused_window_title = Some(window.title.clone());
            }

            windows_by_space
                .entry(strip.id())
                .or_default()
                .extend(row_windows);

            if active_workspace
                && let Some((display_id, display_entity)) = active_display
                && child.parent() == display_entity
            {
                active.display_id = Some(display_id);
            }
        }

        let mut display_states = displays
            .iter()
            .map(|(display, _, display_active)| SpoolDisplayState {
                display_id: display.id(),
                active: display_active,
                visible_space_id: window_manager.active_display_space(display.id()).ok(),
            })
            .collect::<Vec<_>>();
        display_states.sort_by_key(|display| display.display_id);

        let mut spaces = Vec::new();
        for (display, space_ids) in window_manager.present_displays() {
            let visible_id = window_manager.active_display_space(display.id()).ok();
            for (ordinal, space_id) in space_ids.into_iter().enumerate() {
                let mut space_windows = windows_by_space.remove(&space_id).unwrap_or_default();
                space_windows.sort_by_key(|window| window.window_id);
                space_windows.dedup_by_key(|window| window.window_id);
                spaces.push(SpoolSpaceState {
                    space_id,
                    display_id: display.id(),
                    ordinal: ordinal.try_into().unwrap_or(u32::MAX),
                    kind: if window_manager.workspace_is_fullscreen(space_id) {
                        SpaceKind::Fullscreen
                    } else {
                        SpaceKind::User
                    },
                    visible: visible_id == Some(space_id),
                    focused: active.space_id == Some(space_id),
                    windows: space_windows,
                });
            }
        }
        spaces.sort_by_key(|space| (space.display_id, space.ordinal));

        Ok(SpoolQueryState {
            version: 3,
            timestamp: now_timestamp(),
            active,
            capabilities: {
                let capabilities = window_manager.native_space_capabilities();
                let enabled = config.space_control_enabled();
                SpaceCapabilities {
                    move_windows: enabled && capabilities.move_windows,
                    focus: enabled && capabilities.focus,
                    create: enabled && capabilities.create,
                    delete: enabled && capabilities.delete,
                }
            },
            displays: display_states,
            spaces,
        })
    }
}

fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn periodic_state_save(
    workspaces: Query<(
        Option<&ChildOf>,
        &LayoutStrip,
        &NativeSpace,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Windows,
    apps: Query<&Application>,
) {
    let state = SpoolState::extract(&workspaces, &displays, &windows, &apps);
    let path = SpoolState::default_state_file_path();
    if let Err(e) = state.save_to_file(&path) {
        warn!("Failed to save state: {e}");
    } else {
        debug!("State saved to {}", path.display());
    }
}

pub fn cleanup_on_exit(
    mut exit_events: MessageReader<AppExit>,
    workspaces: Query<(
        Option<&ChildOf>,
        &LayoutStrip,
        &NativeSpace,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Windows,
    apps: Query<&Application>,
) {
    if exit_events.read().next().is_some() {
        info!("Exiting, saving state...");
        let state = SpoolState::extract(&workspaces, &displays, &windows, &apps);
        let path = SpoolState::default_state_file_path();
        if let Err(e) = state.save_to_file(&path) {
            error!("Failed to save state on exit: {e}");
        }
    }
}

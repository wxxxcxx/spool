use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::app::AppExit;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Query, Res, ResMut, SystemParam};
use bevy::math::IRect;
use objc2_core_graphics::CGDirectDisplayID;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::Config;
use crate::ecs::layout::{Column, ColumnId, LayoutStrip, StackItem, WidthIntent};
use crate::ecs::native_space::{NativeSpace, SpaceControl, VisibleNativeSpaceMarker};
use crate::ecs::params::Windows;
use crate::ecs::topology::{SpaceMemberships, WindowMemberships};
use crate::ecs::{ActiveDisplayMarker, ActiveWorkspaceMarker};
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, WinID, WorkspaceId};
use spool_shared_types::windowset::{ColumnKind, ColumnSet, StackItemSet, WindowRec, WindowSet};

pub const STATE_FILE_NAME: &str = "state.json";
pub const INTENT_STATE_VERSION: u32 = 6;

#[derive(Clone, Debug, Resource)]
pub struct StateFilePath(PathBuf);

impl StateFilePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}
impl From<PathBuf> for StateFilePath {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}
impl Default for StateFilePath {
    fn default() -> Self {
        Self(SpoolState::default_state_file_path())
    }
}

/// Raw layout intent, never a native topology snapshot or an automatic restore plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Resource)]
#[serde(deny_unknown_fields)]
pub struct SpoolState {
    pub version: u32,
    pub revision: u64,
    pub spaces: Vec<SavedSpace>,
    /// Floating frames retain their own intent; an older file simply has none,
    /// which is why the field is defaulted rather than versioned.
    #[serde(default)]
    pub floating: Vec<SavedFloatingWindow>,
    /// Declared Spaces are session-scoped intent; a Space id is a candidate hint
    /// that a startup owner may map, never a cross-session identity.
    #[serde(default)]
    pub membership: Vec<SavedMembership>,
    /// Per-Space focus memory is session-scoped intent like the rest: the Space
    /// id and every cached window identity are candidate hints only.
    #[serde(default)]
    pub focus: Vec<SavedFocus>,
}

/// One Space's saved focus memory, with cached identity hints only.
///
/// A role whose identity was not resolvable at capture is left out of the file
/// entirely, not written as `null`: unlike a member slot, an absent focus hint
/// carries no structure, and `null` would claim "no preference", which capture
/// cannot support. An entry whose roles both failed is not written at all. No
/// reader may infer identity from either hint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedFocus {
    pub space_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preference: Option<SavedWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SavedWindow>,
}

/// One tracked window's declared Space, with cached identity hints only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedMembership {
    pub window_id: WinID,
    pub pid: Pid,
    pub bundle_id: String,
    pub space_id: crate::platform::WorkspaceId,
}

/// One floating window's authored frame, with cached identity hints only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedFloatingWindow {
    pub window_id: WinID,
    pub pid: Pid,
    pub bundle_id: String,
    pub frame: Frame,
}

/// Numeric Space and column IDs are candidate hints, not cross-daemon identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SavedSpace {
    pub space_id: WorkspaceId,
    pub columns: Vec<SavedColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SavedColumn {
    pub column_id: ColumnId,
    pub width: WidthIntent,
    pub kind: ColumnKind,
    pub items: Vec<SavedItem>,
}

/// One independent vertical slot; native tabs share its raw height preference.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SavedItem {
    pub item_id: u64,
    pub weight: f64,
    pub tabs: bool,
    /// One entry per retained member, in retained order.
    pub members: Vec<SavedMember>,
}

/// One retained member position. `hint` is a locally cached binding hint and
/// may be absent when the member's identity was not resolvable at capture; an
/// absent hint still names a member, so the slot is preserved instead of
/// silently shrinking the item. No reader may infer identity from it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedMember {
    #[serde(default)]
    pub hint: Option<SavedWindow>,
}

/// Locally cached binding hints only; no title/AX fetch is needed during save.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedWindow {
    pub window_id: WinID,
    pub pid: Pid,
    pub bundle_id: String,
}

pub use spool_shared_types::state::{
    ActiveState as SpoolActiveState, DisplayState as SpoolDisplayState, Frame,
    QueryState as SpoolQueryState, SpaceCapabilities, SpaceState as SpoolSpaceState, StateEvent,
    StateQueryKind, WindowState as SpoolWindowState,
};

impl SpoolState {
    /// Captures only accepted domain state, including currently unobservable Spaces.
    pub fn extract(
        strips: &Query<&LayoutStrip>,
        windows: &Windows,
        apps: &Query<&Application>,
        floating: &Query<(
            &Window,
            &ChildOf,
            &crate::ecs::floating_geometry::FloatingGeometry,
        )>,
        membership: &Query<(&Window, &ChildOf, &crate::ecs::native_space::DeclaredSpace)>,
        focus: &crate::ecs::focus::FocusCoordinator,
    ) -> Self {
        Self::from_layouts(
            strips.iter(),
            |entity| {
                let (window, _, app_entity) = windows.get_parent_any(entity)?;
                let app = apps.get(app_entity).ok()?;
                Some(SavedWindow {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                })
            },
            floating.iter().filter_map(|(window, parent, geometry)| {
                let app = apps.get(parent.parent()).ok()?;
                let frame = geometry.frame;
                Some(SavedFloatingWindow {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                    frame: Frame {
                        x: frame.min.x,
                        y: frame.min.y,
                        width: frame.width(),
                        height: frame.height(),
                    },
                })
            }),
            membership.iter().filter_map(|(window, parent, declared)| {
                let app = apps.get(parent.parent()).ok()?;
                Some(SavedMembership {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                    space_id: declared.target?,
                })
            }),
            focus.focus_memory(),
        )
    }

    /// Shared capture for persistence and inspection. The caller supplies only
    /// cached identity hints; neither path may query native state to save intent.
    pub(crate) fn from_layouts<'a>(
        strips: impl IntoIterator<Item = &'a LayoutStrip>,
        mut window_hint: impl FnMut(Entity) -> Option<SavedWindow>,
        floating: impl IntoIterator<Item = SavedFloatingWindow>,
        membership: impl IntoIterator<Item = SavedMembership>,
        focus: impl IntoIterator<Item = crate::ecs::focus::FocusMemory>,
    ) -> Self {
        let mut spaces = strips
            .into_iter()
            .map(|strip| SavedSpace {
                space_id: strip.id(),
                columns: strip
                    .columns()
                    .zip(strip.column_states())
                    .map(|(column, state)| SavedColumn {
                        column_id: state.id,
                        width: state.width,
                        kind: match column {
                            Column::Single(_) => ColumnKind::Single,
                            Column::Stack(_) => ColumnKind::Stack,
                            Column::Tabs(_) => ColumnKind::Tabs,
                            Column::Fullscreen(_) => ColumnKind::Fullscreen,
                        },
                        items: state
                            .height_items
                            .iter()
                            .enumerate()
                            .map(|(index, item)| SavedItem {
                                item_id: item.id.0,
                                weight: item.weight,
                                tabs: match column {
                                    Column::Tabs(_) => true,
                                    Column::Stack(items) => {
                                        matches!(items.get(index), Some(StackItem::Tabs(_)))
                                    }
                                    _ => false,
                                },
                                members: item
                                    .members
                                    .iter()
                                    .copied()
                                    .map(|member| SavedMember {
                                        hint: window_hint(member),
                                    })
                                    .collect(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        spaces.sort_by_key(|space| space.space_id);
        let mut floating = floating.into_iter().collect::<Vec<_>>();
        floating.sort_by_key(|window| window.window_id);
        let mut membership = membership.into_iter().collect::<Vec<_>>();
        membership.sort_by_key(|entry| entry.window_id);
        let mut focus = focus
            .into_iter()
            .filter_map(|memory| {
                let preference = memory.preference.and_then(&mut window_hint);
                let selection = memory.selection.and_then(&mut window_hint);
                (preference.is_some() || selection.is_some()).then_some(SavedFocus {
                    space_id: memory.space_id,
                    preference,
                    selection,
                })
            })
            .collect::<Vec<_>>();
        focus.sort_by_key(|entry| entry.space_id);
        Self {
            version: INTENT_STATE_VERSION,
            revision: 0,
            spaces,
            floating,
            membership,
            focus,
        }
    }

    pub(crate) fn valid(&self) -> bool {
        let mut spaces = HashSet::new();
        let mut columns = HashSet::new();
        let mut items = HashSet::new();
        let mut focus_spaces = HashSet::new();
        self.version == INTENT_STATE_VERSION
            && self
                .focus
                .iter()
                .all(|entry| focus_spaces.insert(entry.space_id))
            && self.spaces.iter().all(|space| {
                spaces.insert(space.space_id)
                    && space.columns.iter().all(|column| {
                        columns.insert(column.column_id)
                            && column.width.validate().is_ok()
                            && match column.kind {
                                ColumnKind::Stack => column.items.len() >= 2,
                                ColumnKind::Tabs => column.items.len() == 1 && column.items[0].tabs,
                                ColumnKind::Single | ColumnKind::Fullscreen => {
                                    column.items.len() == 1 && !column.items[0].tabs
                                }
                            }
                            && column.items.iter().all(|item| {
                                items.insert(item.item_id)
                                    && item.weight.is_finite()
                                    && item.weight > 0.0
                            })
                    })
            })
    }

    /// Parsing has no authority to bind windows, edit layout, or issue native effects.
    pub fn load_from_file(path: &Path) -> Option<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                warn!(%error, path = %path.display(), "Unable to read intent candidates");
                return None;
            }
        };
        match serde_json::from_slice::<Self>(&data) {
            Ok(state) if state.valid() => Some(state),
            _ => {
                warn!(path = %path.display(), "Ignoring unsupported or invalid intent candidates; no migration performed");
                None
            }
        }
    }

    pub fn default_state_file_path() -> PathBuf {
        xdg::BaseDirectories::with_prefix("spool")
            .get_state_file(STATE_FILE_NAME)
            .expect("XDG state directory should be available")
    }
}

/// The only file writer. Captures are versioned, stale commits cannot replace a newer
/// file, and a failed commit never advances the durable revision. This synchronous
/// writer is deliberately serialized; it can move behind a worker without changing
/// the snapshot/commit protocol.
#[derive(Debug, Default, Resource)]
pub struct StatePersistence {
    accepted_revision: u64,
    saved_revision: Option<u64>,
    latest: Option<SpoolState>,
}

impl StatePersistence {
    /// Retains only the durable file's counter floor across daemon sessions.
    /// The old candidate payload remains isolated and is not accepted intent.
    pub fn starting_after(revision: u64) -> Self {
        Self {
            accepted_revision: revision,
            ..Self::default()
        }
    }

    pub fn accepted_revision(&self) -> u64 {
        self.accepted_revision
    }
    pub fn saved_revision(&self) -> Option<u64> {
        self.saved_revision
    }
    pub fn is_dirty(&self) -> bool {
        self.latest.is_some() && self.saved_revision != Some(self.accepted_revision)
    }

    pub(crate) fn capture(&mut self, mut state: SpoolState) -> std::io::Result<SpoolState> {
        if !state.valid() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid layout intent snapshot",
            ));
        }
        if self
            .latest
            .as_ref()
            .is_none_or(|latest| latest.spaces != state.spaces)
        {
            self.accepted_revision = self
                .accepted_revision
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("save revision exhausted"))?;
            state.revision = self.accepted_revision;
            self.latest = Some(state);
        }
        self.latest
            .clone()
            .ok_or_else(|| std::io::Error::other("no captured intent"))
    }

    pub(crate) fn commit(&mut self, snapshot: &SpoolState, path: &Path) -> std::io::Result<bool> {
        // Compare the whole captured value as well as its token: callers cannot
        // reuse a current revision with a different payload.
        if self.latest.as_ref() != Some(snapshot) || self.saved_revision == Some(snapshot.revision)
        {
            return Ok(false);
        }
        let data = serde_json::to_vec_pretty(snapshot).map_err(std::io::Error::other)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let tmp_path = path.with_extension("json.tmp");
        let result = (|| {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&tmp_path, path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result?;
        self.saved_revision = Some(snapshot.revision);
        Ok(true)
    }
}

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

/// The world access [`QueryStateParams::extract`] needs, bundled so callers (the
/// socket query handler, the embedded Lua runtime) take one parameter instead
/// of six.
type QueryWorkspaceProjection = (
    &'static ChildOf,
    &'static LayoutStrip,
    &'static NativeSpace,
    Has<ActiveWorkspaceMarker>,
    Has<VisibleNativeSpaceMarker>,
);

#[derive(SystemParam)]
pub struct QueryStateParams<'w, 's> {
    workspaces: Query<'w, 's, QueryWorkspaceProjection>,
    displays: Query<'w, 's, (&'static Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Windows<'w, 's>,
    apps: Query<'w, 's, &'static Application>,
    window_manager: Res<'w, WindowManager>,
    topology: Res<'w, super::topology::NativeTopology>,
    layout_session: Res<'w, super::layout_snapshot::LayoutSession>,
    config: Res<'w, Config>,
}

impl QueryStateParams<'_, '_> {
    /// The window queries backing the extract, for callers that also need to
    /// look a window up directly rather than through the state document.
    pub fn windows(&self) -> &Windows<'_, '_> {
        &self.windows
    }

    fn space_is_visible(&self, child: &ChildOf, strip: &LayoutStrip) -> bool {
        self.displays
            .get(child.parent())
            .is_ok_and(|(display, _, _)| {
                self.topology.visible_space(display.id()) == Some(strip.id())
            })
    }

    /// One membership scan, shared by every read model built from this world.
    ///
    /// A caller that needs more than one model — the Bar builds both — takes
    /// this once and hands it down; `None` means the scan failed, and a failed
    /// scan publishes no destination at all rather than a partial map.
    pub(crate) fn memberships(&self) -> Option<WindowMemberships> {
        self.topology.observe_memberships(&self.window_manager).ok()
    }

    /// Membership answers for one epoch, asked a Space at a time.
    ///
    /// Use this instead of [`Self::memberships`] when the caller also answers
    /// per-Space questions: it takes the scan once and reads a Space on its own
    /// when the scan could not cover it, rather than publishing nothing for
    /// every Space.
    pub(crate) fn space_memberships(&self) -> SpaceMemberships<'_> {
        self.topology.space_memberships(&self.window_manager)
    }

    /// Tracked floating windows by native window id.
    pub(crate) fn floating_windows(&self) -> HashMap<WinID, Entity> {
        self.windows
            .iter()
            .filter_map(|(window, entity)| {
                self.windows
                    .get_tracked(entity)
                    .is_some_and(|(_, _, state)| state.is_floating())
                    .then_some((window.id(), entity))
            })
            .collect()
    }

    /// The membership scan a read model needs, taken only when a float exists
    /// to place: nothing else in either model depends on Space membership.
    ///
    /// A caller that builds more than one model — the Bar builds both — takes
    /// [`Self::memberships`] once instead and hands the same scan to each.
    pub(crate) fn floating_memberships(
        &self,
        floating: &HashMap<WinID, Entity>,
    ) -> Option<WindowMemberships> {
        if floating.is_empty() {
            return None;
        }
        self.memberships()
    }

    /// Both public read models use one complete membership scan per extraction.
    ///
    /// `memberships` is that scan; `None` means no scan was needed or it
    /// failed, and either way no float gets a destination from it.
    fn floating_by_space(
        &self,
        floating: &HashMap<WinID, Entity>,
        memberships: Option<&WindowMemberships>,
    ) -> HashMap<WorkspaceId, Vec<Entity>> {
        if floating.is_empty() {
            return HashMap::new();
        }
        let Some(memberships) = memberships else {
            return HashMap::new();
        };
        self.workspaces
            .iter()
            .map(|(_, strip, _, _, _)| {
                (
                    strip.id(),
                    memberships
                        .windows_in_space(strip.id())
                        .filter_map(|id| floating.get(&id).copied())
                        .collect(),
                )
            })
            .collect()
    }
}

/// Reads the layout as the tree a script transforms. Unlike
/// [`QueryStateParams::extract`], which flattens each workspace into a list of
/// windows, this keeps the strip's column structure — needed for `ws:swap`,
/// `ws:east`, `ws:stack` and friends to know what is beside what.
impl QueryStateParams<'_, '_> {
    pub fn extract_window_set(&self) -> WindowSet {
        let floating = self.floating_windows();
        let memberships = self.floating_memberships(&floating);
        self.window_set(&floating, memberships.as_ref())
    }

    /// The same read model from a float set and membership scan the caller
    /// already took.
    pub(crate) fn window_set(
        &self,
        floating: &HashMap<WinID, Entity>,
        memberships: Option<&WindowMemberships>,
    ) -> WindowSet {
        use spool_shared_types::windowset::{DisplaySet, WorkspaceSet};

        let focused_entity = self.windows.focused().map(|(_, entity)| entity);
        let sliver_width = self.config.sliver_width();
        let floating_by_space = self.floating_by_space(floating, memberships);

        // Group the workspace strips by the display entity that owns them, so
        // each display can be built with its own workspaces in one pass.
        let mut strips_by_display: HashMap<Entity, Vec<WorkspaceSet>> = HashMap::new();
        for (child, strip, native, _, _) in self.workspaces {
            let space_visible = self.space_is_visible(child, strip);
            let columns = strip
                .columns()
                .enumerate()
                .filter_map(|(index, column)| {
                    self.column_record(
                        column,
                        strip.width_ratio(index),
                        focused_entity,
                        sliver_width,
                        space_visible,
                    )
                })
                .collect();
            // Native membership, not visibility or a remembered strip, owns a float's Space.
            let floating = floating_by_space
                .get(&strip.id())
                .into_iter()
                .flatten()
                .copied()
                .filter_map(|entity| {
                    self.window_record(entity, focused_entity, sliver_width, space_visible)
                })
                .collect();

            strips_by_display
                .entry(child.parent())
                .or_default()
                .push(WorkspaceSet {
                    space_id: strip.id(),
                    ordinal: native.ordinal,
                    active: space_visible,
                    columns: std::sync::Arc::new(columns),
                    floating: std::sync::Arc::new(floating),
                });
        }

        let displays = self
            .displays
            .iter()
            .map(|(display, entity, _)| {
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
                    active: self.topology.active_display() == Some(display.id()),
                    workspaces: std::sync::Arc::new(workspaces),
                }
            })
            .collect();

        let focused = focused_entity
            .and_then(|entity| self.windows.get(entity))
            .map(|window| window.id());
        WindowSet::new(displays, focused).with_snapshot(
            self.layout_session
                .capture(&self.windows, self.workspaces.iter().map(|row| row.1)),
        )
    }

    fn column_record(
        &self,
        column: &Column,
        width_ratio: Option<f64>,
        focused: Option<Entity>,
        sliver_width: i32,
        space_visible: bool,
    ) -> Option<ColumnSet> {
        let project_item = |members: &[Entity]| {
            let mut tiled = Vec::new();
            for &entity in members {
                let Some(record) = self.window_record(entity, focused, sliver_width, space_visible)
                else {
                    continue;
                };
                if !record.floating {
                    tiled.push(record);
                }
            }
            StackItemSet::from_windows(tiled)
        };
        let items = match column {
            Column::Single(entity) | Column::Fullscreen(entity) => {
                project_item(std::slice::from_ref(entity))
                    .into_iter()
                    .collect()
            }
            Column::Tabs(members) => project_item(members).into_iter().collect(),
            Column::Stack(items) => items
                .iter()
                .filter_map(|item| match item {
                    StackItem::Single(entity) => project_item(std::slice::from_ref(entity)),
                    StackItem::Tabs(members) => project_item(members),
                })
                .collect(),
        };
        let mut projected = ColumnSet::from_items(items, width_ratio)?;
        if matches!(column, Column::Fullscreen(_)) {
            projected.kind = ColumnKind::Fullscreen;
        }
        let selected = column
            .top()
            .and_then(|top| self.windows.get(top).map(|window| window.id()))
            .and_then(|id| projected.windows().position(|window| window.id == id))
            .unwrap_or(0);
        projected.selected = selected;
        Some(projected)
    }

    /// One window, as a script sees it. `None` for an entity that is no longer
    /// a window we know anything about.
    fn window_record(
        &self,
        entity: Entity,
        focused: Option<Entity>,
        sliver_width: i32,
        space_visible: bool,
    ) -> Option<WindowRec> {
        let window = self.window_state(entity, focused, sliver_width, space_visible)?;
        Some(WindowRec {
            id: window.window_id,
            app_name: window.app_name,
            bundle_id: window.bundle_id,
            title: window.title,
            frame: window.frame,
            floating: window.floating,
            visible: window.visible,
            focused: window.focused,
        })
    }

    fn window_state(
        &self,
        entity: Entity,
        focused: Option<Entity>,
        sliver_width: i32,
        space_visible: bool,
    ) -> Option<SpoolWindowState> {
        let (window, _, state) = self.windows.get_tracked(entity)?;
        let (_, _, app_entity) = self.windows.get_parent(entity)?;
        let app = self.apps.get(app_entity).ok()?;
        let frame = self.windows.observed_frame(entity);
        // Minimized and hidden windows are never on screen, whatever their last
        // known frame says.
        let hidden = !state.is_visible();
        let visibility = frame
            .and_then(|frame| window_visibility(frame, &self.displays, sliver_width))
            .map(|(display, visible)| (display, space_visible && visible && !hidden));

        Some(SpoolWindowState {
            window_id: window.id(),
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
            display_id: visibility.map(|(display, _)| display),
            visible: visibility.is_some_and(|(_, visible)| visible),
            focused: focused == Some(entity),
        })
    }
}

/// Builds the query/subscribe state document from the ECS world.
impl QueryStateParams<'_, '_> {
    #[allow(clippy::too_many_lines)]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserve the query adapter Result contract; unknown observations are represented in the payload"
    )]
    pub fn extract(&self) -> crate::errors::Result<SpoolQueryState> {
        let floating = self.floating_windows();
        let memberships = self.floating_memberships(&floating);
        self.query_state(&floating, memberships.as_ref())
    }

    /// The same read model from a float set and membership scan the caller
    /// already took.
    #[allow(clippy::too_many_lines)]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserve the query adapter Result contract; unknown observations are represented in the payload"
    )]
    pub(crate) fn query_state(
        &self,
        floating_by_window: &HashMap<WinID, Entity>,
        memberships: Option<&WindowMemberships>,
    ) -> crate::errors::Result<SpoolQueryState> {
        let Self {
            workspaces,
            displays,
            windows,
            window_manager,
            config,
            ..
        } = self;
        let focused_entity = windows.focused().map(|(_, entity)| entity);
        let sliver_width = config.sliver_width();
        let floating = self.floating_by_space(floating_by_window, memberships);

        let active_display = displays
            .iter()
            .find_map(|(display, entity, active)| active.then_some((display.id(), entity)));

        let mut windows_by_space: HashMap<WorkspaceId, Vec<SpoolWindowState>> = HashMap::new();
        let mut active = SpoolActiveState {
            display_id: active_display.map(|(display_id, _)| display_id),
            ..SpoolActiveState::default()
        };

        for (child, strip, _, active_workspace, _) in workspaces {
            let space_visible = self.space_is_visible(child, strip);
            let floating = floating.get(&strip.id()).into_iter().flatten().copied();
            let row_windows = strip
                .all_windows()
                .into_iter()
                .filter(|&entity| {
                    windows
                        .get_tracked(entity)
                        .is_some_and(|(_, _, state)| state.is_tiled())
                })
                .chain(floating)
                .filter_map(|entity| {
                    self.window_state(entity, focused_entity, sliver_width, space_visible)
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

        let display_ids = displays
            .iter()
            .map(|(display, entity, _)| (entity, display.id()))
            .collect::<HashMap<_, _>>();
        let visible_spaces = workspaces
            .iter()
            .filter_map(|(child, strip, _, _, _)| {
                self.space_is_visible(child, strip)
                    .then_some((child.parent(), strip.id()))
            })
            .collect::<HashMap<_, _>>();
        let mut display_states = displays
            .iter()
            .map(|(display, entity, display_active)| SpoolDisplayState {
                display_id: display.id(),
                active: display_active,
                visible_space_id: visible_spaces.get(&entity).copied(),
            })
            .collect::<Vec<_>>();
        display_states.sort_by_key(|display| display.display_id);

        let mut spaces = Vec::new();
        for (child, strip, native, _, _) in workspaces {
            let Some(display_id) = display_ids.get(&child.parent()).copied() else {
                continue;
            };
            let mut space_windows = windows_by_space.remove(&strip.id()).unwrap_or_default();
            // The vector was built by traversing LayoutStrip columns, so its
            // order is semantic. Deduplicate stably in case transient duplicate
            // strips project the same entity; sorting by ID would falsify it.
            let mut seen = HashSet::new();
            space_windows.retain(|window| seen.insert(window.window_id));
            spaces.push(SpoolSpaceState {
                space_id: strip.id(),
                display_id,
                ordinal: native.ordinal,
                kind: native.kind,
                visible: self.space_is_visible(child, strip),
                focused: active.space_id == Some(strip.id()),
                windows: space_windows,
            });
        }
        spaces.sort_by_key(|space| (space.display_id, space.ordinal));

        Ok(SpoolQueryState {
            version: 3,
            timestamp: now_timestamp(),
            active,
            capabilities: {
                let capabilities = SpaceControl::effective(config, window_manager).capabilities();
                SpaceCapabilities {
                    move_windows: capabilities.move_windows,
                    focus: capabilities.focus,
                    create: capabilities.create,
                    delete: capabilities.delete,
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

/// The accepted-intent inputs every save path captures from, bundled so each
/// path declares one parameter instead of six.
#[derive(SystemParam)]
pub struct IntentCapture<'w, 's> {
    strips: Query<'w, 's, &'static LayoutStrip>,
    windows: Windows<'w, 's>,
    apps: Query<'w, 's, &'static Application>,
    floating: Query<
        'w,
        's,
        (
            &'static Window,
            &'static ChildOf,
            &'static crate::ecs::floating_geometry::FloatingGeometry,
        ),
    >,
    membership: Query<
        'w,
        's,
        (
            &'static Window,
            &'static ChildOf,
            &'static crate::ecs::native_space::DeclaredSpace,
        ),
    >,
    focus: Res<'w, crate::ecs::focus::FocusCoordinator>,
}

impl IntentCapture<'_, '_> {
    /// The accepted domain state, including Spaces that native inventory cannot
    /// currently observe.
    pub fn capture(&self) -> SpoolState {
        SpoolState::extract(
            &self.strips,
            &self.windows,
            &self.apps,
            &self.floating,
            &self.membership,
            &self.focus,
        )
    }
}

/// Publishes accepted snapshot revisions independently of the save interval.
/// No native calls or filesystem IO; equality ignores effective/presented values.
pub fn capture_state_changes(capture: IntentCapture, mut persistence: ResMut<StatePersistence>) {
    if let Err(error) = persistence.capture(capture.capture()) {
        warn!(%error, "Unable to capture accepted layout intent");
    }
}

pub fn periodic_state_save(
    capture: IntentCapture,
    path: Res<StateFilePath>,
    mut persistence: ResMut<StatePersistence>,
) {
    let state = capture.capture();
    match persistence
        .capture(state)
        .and_then(|snapshot| persistence.commit(&snapshot, path.as_path()))
    {
        Ok(true) => debug!(
            revision = persistence.accepted_revision(),
            "Layout intent durably saved"
        ),
        Ok(false) => {}
        Err(error) => warn!(%error, "Layout intent save failed; latest intent remains dirty"),
    }
}

pub fn cleanup_on_exit(
    mut exit_events: MessageReader<AppExit>,
    capture: IntentCapture,
    path: Res<StateFilePath>,
    mut persistence: ResMut<StatePersistence>,
) {
    if exit_events.read().next().is_some() {
        let state = capture.capture();
        if let Err(error) = persistence
            .capture(state)
            .and_then(|snapshot| persistence.commit(&snapshot, path.as_path()))
        {
            warn!(%error, "Layout intent save on exit failed; latest intent remains dirty");
        }
    }
}

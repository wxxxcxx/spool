use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::observer::On;
use bevy::ecs::query::With;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut, SystemParam};
use bevy::time::{Time, Timer, TimerMode, Virtual};
use tracing::{Level, info, instrument, warn};

use crate::config::{Config, MissingWindowBehavior};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{WindowCtx, Windows};
use crate::ecs::state::{SavedColumn, SavedSpace, SavedStackItem, SavedWindow, SpoolState};
use crate::ecs::workspace::PendingSpaceDestruction;
use crate::ecs::{Floating, RefreshWindowSizes, RestoreWindowState};
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, WinID, WorkspaceId};

#[derive(Debug, Resource)]
pub(crate) struct SessionRestore {
    state: SpoolState,
    timer: Timer,
}

impl SessionRestore {
    fn new(state: SpoolState, grace: Duration) -> Self {
        Self {
            state,
            timer: Timer::new(grace, TimerMode::Once),
        }
    }
}

#[derive(Resource)]
pub(super) struct RestoreRetry(Timer);

fn defer_restore(commands: &mut Commands) {
    commands.queue(|world: &mut bevy::prelude::World| {
        world
            .resource_mut::<super::topology::NativeTopology>()
            .request_refresh();
    });
    commands.insert_resource(RestoreRetry(Timer::new(
        Duration::from_millis(250),
        TimerMode::Once,
    )));
}

pub(super) fn tick_restore_grace(
    time: Res<Time<Virtual>>,
    mut session: Option<ResMut<SessionRestore>>,
    mut retry: Option<ResMut<RestoreRetry>>,
    mut commands: Commands,
) {
    let Some(session) = session.as_mut() else {
        return;
    };

    session.timer.tick(time.delta());
    if session.timer.is_finished() {
        info!("Session restore grace period ended");
        commands.remove_resource::<SessionRestore>();
        commands.remove_resource::<SpoolState>();
        commands.remove_resource::<RestoreRetry>();
    } else if let Some(retry) = retry.as_mut() {
        retry.0.tick(time.delta());
        if retry.0.is_finished() {
            commands.remove_resource::<RestoreRetry>();
            commands.trigger(RestoreWindowState);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CurrentWindowIdentity {
    pub entity: Entity,
    pub window_id: WinID,
    pub pid: Pid,
    pub bundle_id: String,
    pub title: String,
    pub identifier: String,
    pub role: String,
    pub subrole: String,
}

impl CurrentWindowIdentity {
    fn hard_key(&self) -> WindowHardMatchKey {
        WindowHardMatchKey::new(self.window_id, self.pid, self.bundle_id.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WindowHardMatchKey {
    window_id: WinID,
    pid: Pid,
    bundle_id: String,
}

impl WindowHardMatchKey {
    fn new(window_id: WinID, pid: Pid, bundle_id: String) -> Self {
        Self {
            window_id,
            pid,
            bundle_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedColumn {
    Single(Entity),
    Stack(Vec<PlannedStackItem>),
    Tabs(Vec<Entity>),
    Fullscreen(Entity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedStackItem {
    Single(Entity),
    Tabs(Vec<Entity>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedStrip {
    pub workspace_id: WorkspaceId,
    pub columns: Vec<PlannedColumn>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RestorePlan {
    pub strips: Vec<PlannedStrip>,
    pub consumed_entities: HashSet<Entity>,
    pub ignored_missing_windows: usize,
    pub skipped_ambiguous_matches: usize,
}

pub(crate) struct RestorePlanner<'a> {
    state: &'a SpoolState,
    present_spaces: &'a HashSet<WorkspaceId>,
    saved_hard_keys: HashSet<WindowHardMatchKey>,
}

impl<'a> RestorePlanner<'a> {
    pub(crate) fn for_present_spaces(
        state: &'a SpoolState,
        present_spaces: &'a HashSet<WorkspaceId>,
    ) -> Self {
        Self {
            state,
            present_spaces,
            saved_hard_keys: saved_hard_match_keys_for_spaces(state, present_spaces),
        }
    }

    fn saved_spaces(&self) -> impl Iterator<Item = &'a SavedSpace> {
        self.state
            .spaces
            .iter()
            .filter(|space| self.present_spaces.contains(&space.space_id))
    }

    fn saved_windows(&self) -> impl Iterator<Item = &'a SavedWindow> {
        self.saved_spaces()
            .flat_map(|space| &space.columns)
            .flat_map(saved_windows_in_column)
    }

    fn has_saved_fallback_windows(&self) -> bool {
        self.saved_windows().any(|window| !window.title.is_empty())
    }

    pub(crate) fn plan(
        &self,
        current: &[CurrentWindowIdentity],
        memberships: &HashMap<WinID, Option<WorkspaceId>>,
    ) -> RestorePlan {
        let mut plan = RestorePlan::default();

        for space in self.saved_spaces() {
            let eligible = current
                .iter()
                .filter(|window| memberships.get(&window.window_id) == Some(&Some(space.space_id)))
                .cloned()
                .collect::<Vec<_>>();
            let surviving_strips = self.plan_space(space, &eligible, &mut plan);
            plan.strips.extend(surviving_strips);
        }

        plan
    }

    fn plan_space(
        &self,
        space: &SavedSpace,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Vec<PlannedStrip> {
        let columns = space
            .columns
            .iter()
            .filter_map(|column| self.plan_column(column, current, plan))
            .collect::<Vec<_>>();
        if columns.is_empty() {
            Vec::new()
        } else {
            vec![PlannedStrip {
                workspace_id: space.space_id,
                columns,
            }]
        }
    }

    fn plan_column(
        &self,
        column: &SavedColumn,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<PlannedColumn> {
        match column {
            SavedColumn::Single(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedColumn::Single),
            SavedColumn::Fullscreen(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedColumn::Fullscreen),
            SavedColumn::Tabs(tabs) => compact_entities(
                tabs.iter()
                    .filter_map(|saved| self.match_window(saved, current, plan))
                    .collect(),
            ),
            SavedColumn::Stack(items) => compact_stack_items(
                items
                    .iter()
                    .filter_map(|item| self.plan_stack_item(item, current, plan))
                    .collect(),
            ),
        }
    }

    fn plan_stack_item(
        &self,
        item: &SavedStackItem,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<PlannedStackItem> {
        match item {
            SavedStackItem::Single(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedStackItem::Single),
            SavedStackItem::Tabs(tabs) => compact_stack_tabs(
                tabs.iter()
                    .filter_map(|saved| self.match_window(saved, current, plan))
                    .collect(),
            ),
        }
    }

    fn match_window(
        &self,
        saved: &SavedWindow,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<Entity> {
        if let Some(window) = current.iter().find(|window| {
            !plan.consumed_entities.contains(&window.entity)
                && saved.hard_match(window.window_id, window.pid, &window.bundle_id)
        }) {
            plan.consumed_entities.insert(window.entity);
            return Some(window.entity);
        }

        let fallback_matches = current
            .iter()
            .filter(|window| {
                !plan.consumed_entities.contains(&window.entity)
                    && saved.fallback_match(window)
                    && !self.current_window_has_saved_hard_match(window)
            })
            .collect::<Vec<_>>();

        match fallback_matches.as_slice() {
            [window] => {
                plan.consumed_entities.insert(window.entity);
                Some(window.entity)
            }
            [] => {
                plan.ignored_missing_windows += 1;
                None
            }
            _ => {
                plan.skipped_ambiguous_matches += 1;
                None
            }
        }
    }

    fn current_window_has_saved_hard_match(&self, current: &CurrentWindowIdentity) -> bool {
        self.saved_hard_keys.contains(&current.hard_key())
    }
}

fn saved_windows_in_column(column: &SavedColumn) -> Box<dyn Iterator<Item = &SavedWindow> + '_> {
    match column {
        SavedColumn::Single(saved) | SavedColumn::Fullscreen(saved) => {
            Box::new(std::iter::once(saved))
        }
        SavedColumn::Tabs(tabs) => Box::new(tabs.iter()),
        SavedColumn::Stack(items) => Box::new(items.iter().flat_map(saved_windows_in_stack_item)),
    }
}

fn saved_windows_in_stack_item(
    item: &SavedStackItem,
) -> Box<dyn Iterator<Item = &SavedWindow> + '_> {
    match item {
        SavedStackItem::Single(saved) => Box::new(std::iter::once(saved)),
        SavedStackItem::Tabs(tabs) => Box::new(tabs.iter()),
    }
}

impl SavedWindow {
    fn hard_key(&self) -> WindowHardMatchKey {
        WindowHardMatchKey::new(self.window_id, self.pid, self.bundle_id.clone())
    }

    fn fallback_match(&self, current: &CurrentWindowIdentity) -> bool {
        !self.title.is_empty()
            && self.bundle_id == current.bundle_id
            && self.title == current.title
            && self.identifier == current.identifier
            && self.role == current.role
            && self.subrole == current.subrole
    }
}

fn saved_hard_match_keys_for_spaces(
    state: &SpoolState,
    present_spaces: &HashSet<WorkspaceId>,
) -> HashSet<WindowHardMatchKey> {
    state
        .spaces
        .iter()
        .filter(|space| present_spaces.contains(&space.space_id))
        .flat_map(|space| &space.columns)
        .flat_map(saved_windows_in_column)
        .map(SavedWindow::hard_key)
        .collect()
}

pub(crate) fn eligible_restore_space_ids(
    present_spaces: impl IntoIterator<Item = WorkspaceId>,
    pending_space_ids: impl IntoIterator<Item = WorkspaceId>,
) -> HashSet<WorkspaceId> {
    let pending = pending_space_ids.into_iter().collect::<HashSet<_>>();
    present_spaces
        .into_iter()
        .filter(|workspace_id| !pending.contains(workspace_id))
        .collect()
}

pub(crate) fn matches_startup_restore_state(
    window: &Window,
    app: &Application,
    session: Option<&SessionRestore>,
    restoration: Option<&SpoolState>,
    config: &Config,
    present_spaces: &HashSet<WorkspaceId>,
) -> bool {
    if !config.restore_enabled() {
        return false;
    }

    let Ok(pid) = window.pid() else {
        return false;
    };
    let bundle_id = app.bundle_id().unwrap_or_default().clone();
    let state = if let Some(session) = session {
        &session.state
    } else if let Some(state) = restoration {
        state
    } else {
        return false;
    };
    RestorePlanner::for_present_spaces(state, present_spaces)
        .saved_windows()
        .any(|saved| saved.hard_match(window.id(), pid, &bundle_id))
}

#[derive(SystemParam)]
pub(super) struct RestoreWindowStateCtx<'w, 's> {
    workspaces: Query<
        'w,
        's,
        (Entity, &'static mut LayoutStrip, Option<&'static ChildOf>),
        With<super::native_space::NativeSpace>,
    >,
    displays: Query<'w, 's, &'static Display>,
    apps: Query<'w, 's, &'static Application>,
    pending_spaces: Query<'w, 's, &'static PendingSpaceDestruction>,
    session: Option<Res<'w, SessionRestore>>,
    restoration: Option<Res<'w, SpoolState>>,
    window_manager: Res<'w, WindowManager>,
    topology: Res<'w, super::topology::NativeTopology>,
    window: WindowCtx<'w, 's>,
}

#[allow(clippy::too_many_lines)]
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn restore_window_state(_: On<RestoreWindowState>, restore: RestoreWindowStateCtx) {
    let RestoreWindowStateCtx {
        mut workspaces,
        displays,
        apps,
        pending_spaces,
        session,
        restoration,
        window_manager,
        topology,
        window: mut ctx,
    } = restore;
    let restoration = if let Some(session) = session.as_deref() {
        &session.state
    } else {
        let Some(restoration) = restoration.as_deref() else {
            return;
        };
        if !ctx.config.restore_enabled() {
            info!("Session restore disabled by configuration");
            ctx.commands.remove_resource::<SpoolState>();
            return;
        }
        match ctx.config.restore_missing_windows() {
            MissingWindowBehavior::Ignore => {}
        }
        ctx.commands.insert_resource(SessionRestore::new(
            restoration.clone(),
            ctx.config.restore_startup_grace(),
        ));
        restoration
    };

    if !topology.is_complete() {
        warn!("Session restore deferred: display topology unavailable");
        defer_restore(&mut ctx.commands);
        return;
    }
    let topology = topology.known_displays().collect::<Vec<_>>();
    let mut memberships = HashMap::new();
    for space_id in topology.iter().flat_map(|(_, spaces)| spaces.iter()) {
        let Ok(window_ids) = window_manager.windows_in_workspace(*space_id) else {
            warn!(
                space_id,
                "Session restore deferred: Space membership unavailable"
            );
            defer_restore(&mut ctx.commands);
            return;
        };
        for window_id in window_ids {
            memberships
                .entry(window_id)
                .and_modify(|space| {
                    if *space != Some(*space_id) {
                        *space = None;
                    }
                })
                .or_insert(Some(*space_id));
        }
    }
    let mut live_space_displays = HashMap::new();
    for (display, spaces) in &topology {
        for workspace_id in *spaces {
            live_space_displays.insert(*workspace_id, display.id());
        }
    }
    let present_spaces = eligible_restore_space_ids(
        topology
            .iter()
            .flat_map(|(_, spaces)| spaces.iter().copied()),
        pending_spaces.iter().map(|pending| pending.workspace_id),
    );
    let planner = RestorePlanner::for_present_spaces(restoration, &present_spaces);
    let current = current_window_identities(&ctx.windows, &apps, &planner);
    let plan = planner.plan(&current, &memberships);
    ctx.commands.remove_resource::<RestoreRetry>();

    if plan.consumed_entities.is_empty() {
        info!(
            "Session restore matched 0 windows; missing={}, ambiguous={}",
            plan.ignored_missing_windows, plan.skipped_ambiguous_matches
        );
        return;
    }

    let targets = plan
        .strips
        .iter()
        .filter_map(|planned| {
            workspaces.iter().find_map(|(entity, strip, child)| {
                let display = child.and_then(|child| displays.get(child.parent()).ok())?;
                (strip.id() == planned.workspace_id
                    && !pending_spaces.contains(entity)
                    && live_space_displays.get(&planned.workspace_id) == Some(&display.id()))
                .then_some((planned.workspace_id, entity))
            })
        })
        .collect::<HashMap<_, _>>();
    if targets.len() != plan.strips.len() {
        defer_restore(&mut ctx.commands);
        return;
    }
    for planned in &plan.strips {
        if matches!(planned.columns.as_slice(), [PlannedColumn::Fullscreen(_)])
            && workspaces
                .get(targets[&planned.workspace_id])
                .is_ok_and(|(_, strip, _)| {
                    strip
                        .all_windows()
                        .iter()
                        .any(|entity| !plan.consumed_entities.contains(entity))
                })
        {
            warn!("Skipping fullscreen restore that would discard unmatched live members");
            return;
        }
    }
    for (_, mut strip, _) in &mut workspaces {
        for entity in &plan.consumed_entities {
            strip.remove(*entity);
        }
    }

    for entity in &plan.consumed_entities {
        if let Ok(mut entity_commands) = ctx.commands.get_entity(*entity) {
            entity_commands.try_remove::<Floating>();
        }
    }

    let mut restored_strips = 0;
    for planned in &plan.strips {
        let mut strip = layout_strip_from_plan(planned);
        let entity = targets[&planned.workspace_id];
        let Ok((_, mut existing, _)) = workspaces.get_mut(entity) else {
            continue;
        };
        if !strip.is_fullscreen() {
            strip.append_strip(&mut existing);
        }
        *existing = strip;
        ctx.commands
            .entity(entity)
            .insert(RefreshWindowSizes::default());
        restored_strips += 1;
    }

    info!(
        "Session restore applied: matched={}, strips={}, missing={}, ambiguous={}",
        plan.consumed_entities.len(),
        restored_strips,
        plan.ignored_missing_windows,
        plan.skipped_ambiguous_matches
    );
}

fn layout_strip_from_plan(planned: &PlannedStrip) -> LayoutStrip {
    if let [PlannedColumn::Fullscreen(entity)] = planned.columns.as_slice() {
        return LayoutStrip::fullscreen(planned.workspace_id, *entity);
    }

    let mut strip = LayoutStrip::new(planned.workspace_id);
    apply_planned_columns(&mut strip, &planned.columns);
    strip
}

fn current_window_identities(
    windows: &Windows,
    apps: &Query<&Application>,
    planner: &RestorePlanner,
) -> Vec<CurrentWindowIdentity> {
    let mut current = windows
        .tiled_iter()
        .filter_map(|(window, entity, child)| {
            let app = apps.get(child.parent()).ok()?;
            Some(CurrentWindowIdentity {
                entity,
                window_id: window.id(),
                pid: window.pid().ok()?,
                bundle_id: app.bundle_id().unwrap_or_default().clone(),
                title: String::new(),
                identifier: String::new(),
                role: String::new(),
                subrole: String::new(),
            })
        })
        .collect::<Vec<_>>();

    if planner.has_saved_fallback_windows() {
        hydrate_fallback_identities(&mut current, windows, &planner.saved_hard_keys);
    }

    current
}

fn hydrate_fallback_identities(
    current: &mut [CurrentWindowIdentity],
    windows: &Windows,
    saved_hard_keys: &HashSet<WindowHardMatchKey>,
) {
    for identity in current {
        if saved_hard_keys.contains(&identity.hard_key()) {
            continue;
        }
        let Some(window) = windows.get(identity.entity) else {
            continue;
        };
        identity.title = window.title().unwrap_or_default();
        if identity.title.is_empty() {
            continue;
        }
        identity.identifier = window.identifier().unwrap_or_default();
        identity.role = window.role().unwrap_or_default();
        identity.subrole = window.subrole().unwrap_or_default();
    }
}

fn apply_planned_columns(strip: &mut LayoutStrip, columns: &[PlannedColumn]) {
    for column in columns {
        match column {
            PlannedColumn::Single(entity) | PlannedColumn::Fullscreen(entity) => {
                strip.append(*entity);
            }
            PlannedColumn::Tabs(entities) => {
                append_tabs(strip, entities);
            }
            PlannedColumn::Stack(items) => {
                append_stack(strip, items);
            }
        }
    }
}

fn append_tabs(strip: &mut LayoutStrip, entities: &[Entity]) -> Option<Entity> {
    let leader = *entities.first()?;
    strip.append(leader);
    for follower in &entities[1..] {
        _ = strip.convert_to_tabs(leader, *follower);
    }
    Some(leader)
}

fn append_stack(strip: &mut LayoutStrip, items: &[PlannedStackItem]) {
    let mut first = true;
    for item in items {
        let Some(leader) = append_stack_item(strip, item) else {
            continue;
        };
        if first {
            first = false;
        } else {
            _ = strip.stack(leader);
        }
    }
}

fn append_stack_item(strip: &mut LayoutStrip, item: &PlannedStackItem) -> Option<Entity> {
    match item {
        PlannedStackItem::Single(entity) => {
            strip.append(*entity);
            Some(*entity)
        }
        PlannedStackItem::Tabs(entities) => append_tabs(strip, entities),
    }
}

fn compact_entities(entities: Vec<Entity>) -> Option<PlannedColumn> {
    match entities.as_slice() {
        [] => None,
        [entity] => Some(PlannedColumn::Single(*entity)),
        _ => Some(PlannedColumn::Tabs(entities)),
    }
}

fn compact_stack_tabs(entities: Vec<Entity>) -> Option<PlannedStackItem> {
    match entities.as_slice() {
        [] => None,
        [entity] => Some(PlannedStackItem::Single(*entity)),
        _ => Some(PlannedStackItem::Tabs(entities)),
    }
}

fn compact_stack_items(items: Vec<PlannedStackItem>) -> Option<PlannedColumn> {
    match items.as_slice() {
        [] => None,
        [PlannedStackItem::Single(entity)] => Some(PlannedColumn::Single(*entity)),
        [PlannedStackItem::Tabs(entities)] => Some(PlannedColumn::Tabs(entities.clone())),
        _ => Some(PlannedColumn::Stack(items)),
    }
}

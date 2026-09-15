//! Isolated intent candidates and a trusted-mapping import seam.
//!
//! There is still no native identity matcher: a startup owner must prove every
//! mapping it submits. What exists here is that owner's side — the baseline it
//! freezes, the window in which it may submit, and the import itself.

#![allow(
    dead_code,
    reason = "the seam exposes more than the current startup owner uses; nothing here writes a native effect"
)]

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::ecs::entity::Entity;
use bevy::ecs::resource::Resource;
use bevy::math::{IRect, IVec2};
use bevy::prelude::{Commands, In, Local, Query, Res, ResMut, Time};
use tracing::{debug, warn};

use crate::ecs::floating_geometry::FloatingGeometry;
use crate::ecs::layout::{ColumnId, LayoutStrip, StackItemId};
use crate::ecs::state::SpoolState;
use crate::errors::{Error, Result};
use crate::manager::Window;
use crate::platform::{Pid, WinID};

/// Reading a file only creates this resource. Its numeric binding hints never
/// become macOS topology or authorize an effect.
#[derive(Debug, Resource)]
pub struct RestoreCandidates {
    state: SpoolState,
    import_window_open: bool,
    initial_layouts: Option<HashMap<crate::platform::WorkspaceId, InitialLayout>>,
}

#[derive(Debug)]
struct InitialLayout {
    structure_revision: u64,
    columns: HashMap<ColumnId, u64>,
    items: HashMap<StackItemId, u64>,
}

impl From<SpoolState> for RestoreCandidates {
    fn from(state: SpoolState) -> Self {
        Self {
            state,
            import_window_open: true,
            initial_layouts: None,
        }
    }
}

impl RestoreCandidates {
    pub fn state(&self) -> &SpoolState {
        &self.state
    }

    /// Whether a startup owner may still submit trusted mappings.
    pub fn import_window_open(&self) -> bool {
        self.import_window_open
    }

    /// Called once by a trusted startup owner after initial discovery and before
    /// current-session edits are admitted. Binding discovery may finish later,
    /// but it cannot replace this baseline with a newer layout snapshot.
    /// Reading candidates alone does not freeze or enable an import protocol.
    pub fn freeze_initial_layouts<'a>(
        &mut self,
        layouts: impl IntoIterator<Item = &'a LayoutStrip>,
    ) -> Result<()> {
        if !self.import_window_open || self.initial_layouts.is_some() {
            return Err(Error::InvalidInput(
                "initial import baseline is already frozen or expired".into(),
            ));
        }
        let mut initial = HashMap::new();
        for layout in layouts {
            if initial
                .insert(
                    layout.id(),
                    InitialLayout {
                        structure_revision: layout.structure_revision(),
                        items: layout
                            .column_states()
                            .flat_map(|column| &column.height_items)
                            .map(|item| (item.id, item.intent_revision))
                            .collect(),
                        columns: layout
                            .column_states()
                            .map(|column| (column.id, column.intent_revision))
                            .collect(),
                    },
                )
                .is_some()
            {
                return Err(Error::InvalidInput("duplicate initial import Space".into()));
            }
        }
        self.initial_layouts = Some(initial);
        Ok(())
    }

    /// A caller owning a future startup protocol must close its frozen candidate
    /// set when the startup window ends. No automatic binding provider exists.
    pub fn close_import_window(&mut self) {
        self.import_window_open = false;
    }
}

/// The caller must prove the mapping; this module never infers it from IDs,
/// titles, positions, PID, or process-local native hashes. The private revision
/// gates are frozen before the eventual commit.
#[derive(Clone, Debug)]
pub struct TrustedColumnBinding {
    candidate_space: usize,
    candidate_column: usize,
    target_space: crate::platform::WorkspaceId,
    target_column: ColumnId,
    structure_revision: u64,
    intent_revision: u64,
    heights: Vec<(usize, Entity, StackItemId)>,
}

impl TrustedColumnBinding {
    pub fn new(
        candidate_space: usize,
        candidate_column: usize,
        candidates: &RestoreCandidates,
        target: &LayoutStrip,
        column: ColumnId,
    ) -> Result<Self> {
        if !candidates.import_window_open {
            return Err(Error::InvalidInput(
                "intent import window has expired".into(),
            ));
        }
        let baseline = candidates
            .initial_layouts
            .as_ref()
            .and_then(|layouts| layouts.get(&target.id()))
            .ok_or_else(|| {
                Error::InvalidInput("target has no frozen startup import baseline".into())
            })?;
        let initial_revision = baseline.columns.get(&column).ok_or_else(|| {
            Error::InvalidInput("column was created after the startup import baseline".into())
        })?;
        if baseline.structure_revision != target.structure_revision() {
            return Err(Error::InvalidInput(
                "layout changed after the startup import baseline".into(),
            ));
        }
        let state = target
            .column_states()
            .find(|state| state.id == column)
            .ok_or_else(|| Error::InvalidInput("import target column no longer exists".into()))?;
        // Imports are lower priority than any explicit current-session edit,
        // including an edit made before a delayed identity mapping arrives.
        if state.intent_revision != *initial_revision || state.intent_revision != 0 {
            return Err(Error::InvalidInput(
                "import cannot replace a current-session width edit".into(),
            ));
        }
        Ok(Self {
            candidate_space,
            candidate_column,
            target_space: target.id(),
            target_column: column,
            structure_revision: baseline.structure_revision,
            intent_revision: *initial_revision,
            heights: Vec::new(),
        })
    }

    /// The caller proves each saved slot-to-live-slot correspondence explicitly.
    /// Column shape or a matching ordinal alone never authorizes a height import.
    pub fn with_heights(
        mut self,
        candidates: &RestoreCandidates,
        target: &LayoutStrip,
        mapping: &[(usize, Entity)],
    ) -> Result<Self> {
        if !self.heights.is_empty() {
            return Err(Error::InvalidInput(
                "height import mapping is already bound".into(),
            ));
        }
        let source = candidates
            .state
            .spaces
            .get(self.candidate_space)
            .and_then(|space| space.columns.get(self.candidate_column))
            .ok_or_else(|| Error::InvalidInput("height import candidate absent".into()))?;
        let current = target
            .column_states()
            .find(|column| column.id == self.target_column)
            .ok_or_else(|| Error::InvalidInput("height import column absent".into()))?;
        let live_column = target
            .columns()
            .zip(target.column_states())
            .find(|(_, state)| state.id == self.target_column)
            .map(|(column, _)| column)
            .ok_or_else(|| Error::InvalidInput("height import arrangement absent".into()))?;
        let live_kind = match live_column {
            crate::ecs::layout::Column::Single(_) => {
                spool_shared_types::windowset::ColumnKind::Single
            }
            crate::ecs::layout::Column::Stack(_) => {
                spool_shared_types::windowset::ColumnKind::Stack
            }
            crate::ecs::layout::Column::Tabs(_) => spool_shared_types::windowset::ColumnKind::Tabs,
            crate::ecs::layout::Column::Fullscreen(_) => {
                spool_shared_types::windowset::ColumnKind::Fullscreen
            }
        };
        if source.kind != live_kind {
            return Err(Error::InvalidInput(
                "height import arrangement differs".into(),
            ));
        }
        if mapping.len() != source.items.len() || mapping.len() != current.height_items.len() {
            return Err(Error::InvalidInput(
                "height import requires a complete slot mapping".into(),
            ));
        }
        let mut sources = HashSet::new();
        let mut targets = HashSet::new();
        for &(index, entity) in mapping {
            let item = target
                .height_state(entity)
                .filter(|item| {
                    current
                        .height_items
                        .iter()
                        .any(|candidate| candidate.id == item.id)
                })
                .ok_or_else(|| Error::InvalidInput("height import target absent".into()))?;
            if source.items.get(index).is_none()
                || !sources.insert(index)
                || !targets.insert(item.id)
                || item.intent_revision != 0
                || candidates
                    .initial_layouts
                    .as_ref()
                    .and_then(|layouts| layouts.get(&target.id()))
                    .and_then(|initial| initial.items.get(&item.id))
                    != Some(&0)
            {
                return Err(Error::InvalidInput(
                    "stale or duplicate height import mapping".into(),
                ));
            }
            self.heights.push((index, entity, item.id));
        }
        Ok(self)
    }
}

/// Validates every binding and applies one strip transaction. No group creation,
/// Space movement, focus change, or native calls occur. Mock bindings exercise
/// this seam; they do not establish cross-daemon identity continuity.
pub fn import_intents(
    candidates: &RestoreCandidates,
    target: &mut LayoutStrip,
    bindings: &[TrustedColumnBinding],
) -> Result<usize> {
    if !candidates.import_window_open
        || candidates.initial_layouts.is_none()
        || !candidates.state.valid()
    {
        return Err(Error::InvalidInput(
            "intent import candidates are invalid or expired".into(),
        ));
    }
    let mut target_ids = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut edits = Vec::with_capacity(bindings.len());
    let mut height_edits = Vec::new();
    for binding in bindings {
        let baseline_matches = candidates
            .initial_layouts
            .as_ref()
            .and_then(|layouts| layouts.get(&binding.target_space))
            .is_some_and(|baseline| {
                baseline.structure_revision == binding.structure_revision
                    && baseline.columns.get(&binding.target_column)
                        == Some(&binding.intent_revision)
            });
        if !baseline_matches
            || binding.target_space != target.id()
            || binding.structure_revision != target.structure_revision()
            || !target_ids.insert(binding.target_column)
            || !source_ids.insert((binding.candidate_space, binding.candidate_column))
        {
            return Err(Error::InvalidInput(
                "stale or duplicate intent import mapping".into(),
            ));
        }
        let current = target
            .column_states()
            .find(|state| state.id == binding.target_column)
            .ok_or_else(|| Error::InvalidInput("import target column no longer exists".into()))?;
        if current.intent_revision != binding.intent_revision || current.intent_revision != 0 {
            return Err(Error::InvalidInput(
                "intent import would overwrite a newer width edit".into(),
            ));
        }
        let source = candidates
            .state
            .spaces
            .get(binding.candidate_space)
            .and_then(|space| space.columns.get(binding.candidate_column))
            .ok_or_else(|| Error::InvalidInput("intent import candidate is absent".into()))?;
        // Even a width-only import cannot resurrect pre-edit column state after
        // the user has edited one of this column's height preferences.
        let baseline = candidates
            .initial_layouts
            .as_ref()
            .and_then(|layouts| layouts.get(&target.id()))
            .ok_or_else(|| Error::InvalidInput("height import baseline absent".into()))?;
        if current
            .height_items
            .iter()
            .any(|item| item.intent_revision != 0 || baseline.items.get(&item.id) != Some(&0))
        {
            return Err(Error::InvalidInput(
                "intent import would overwrite a newer height edit".into(),
            ));
        }
        for &(index, entity, id) in &binding.heights {
            if target
                .height_state(entity)
                .is_none_or(|item| item.id != id || item.intent_revision != 0)
            {
                return Err(Error::InvalidInput("height import target changed".into()));
            }
            let item = source
                .items
                .get(index)
                .ok_or_else(|| Error::InvalidInput("height import candidate absent".into()))?;
            height_edits.push((entity, item.weight));
        }
        edits.push((binding.target_column, source.width));
    }
    // Stage only the one domain object, so a future domain validation failure
    // cannot leave an earlier binding partially applied.
    let mut staged = target.clone();
    let mut changed = 0;
    for (column, width) in edits {
        changed += usize::from(staged.set_width_intent(column, width)?);
    }
    for (entity, weight) in height_edits {
        changed += usize::from(staged.set_height_weight(entity, weight)?);
    }
    if changed > 0 {
        *target = staged;
    }
    Ok(changed)
}

/// One floating window's candidate entry, already validated against the live
/// window it claims.
#[derive(Clone, Debug)]
pub struct TrustedFloatingBinding {
    candidate_window: usize,
    target_window_id: WinID,
}

impl TrustedFloatingBinding {
    /// The caller must prove the mapping. The candidate's cached native id, pid
    /// and bundle id must all agree with the live window: the native id has no
    /// documented lifetime or reuse behaviour, so it never authorizes a binding
    /// on its own.
    pub fn new(
        candidate_window: usize,
        candidates: &RestoreCandidates,
        target_window_id: WinID,
        pid: Pid,
        bundle_id: &str,
    ) -> Result<Self> {
        if !candidates.import_window_open || candidates.initial_layouts.is_none() {
            return Err(Error::InvalidInput(
                "intent import candidates are invalid or expired".into(),
            ));
        }
        let saved = candidates
            .state
            .floating
            .get(candidate_window)
            .ok_or_else(|| {
                Error::InvalidInput("candidate floating window does not exist".into())
            })?;
        if saved.window_id != target_window_id || saved.pid != pid || saved.bundle_id != bundle_id {
            return Err(Error::InvalidInput(
                "candidate floating window's cached identity does not match the live window".into(),
            ));
        }
        Ok(Self {
            candidate_window,
            target_window_id,
        })
    }
}

/// Imports floating frames from candidates the caller has proven.
///
/// The frame comes from the candidate and is written as retained intent, so no
/// platform write happens here. The caller validates every binding before any is
/// applied: one bad binding refuses the whole import.
pub fn import_floating_frames(
    candidates: &RestoreCandidates,
    windows: &mut Query<(
        Entity,
        &Window,
        &bevy::ecs::hierarchy::ChildOf,
        Option<&mut FloatingGeometry>,
    )>,
    commands: &mut Commands,
    bindings: &[TrustedFloatingBinding],
) -> Result<usize> {
    if !candidates.import_window_open || !candidates.state.valid() {
        return Err(Error::InvalidInput(
            "intent import candidates are invalid or expired".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut applied = 0;
    for binding in bindings {
        if !seen.insert(binding.candidate_window) {
            return Err(Error::InvalidInput(
                "duplicate floating import binding".into(),
            ));
        }
        let saved = candidates
            .state
            .floating
            .get(binding.candidate_window)
            .ok_or_else(|| {
                Error::InvalidInput("candidate floating window does not exist".into())
            })?;
        let frame = checked_frame_from(saved.frame).ok_or_else(|| {
            Error::InvalidInput("candidate floating frame is not representable".into())
        })?;
        let entity = windows
            .iter()
            .find_map(|(entity, window, _, _)| {
                (window.id() == binding.target_window_id).then_some(entity)
            })
            .ok_or_else(|| Error::InvalidInput("import target window is not tracked".into()))?;
        let Ok((_, _, _, geometry)) = windows.get_mut(entity) else {
            return Err(Error::InvalidInput(
                "import target window is unavailable".into(),
            ));
        };
        match geometry {
            Some(mut geometry) => geometry.state(frame),
            None => {
                commands
                    .entity(entity)
                    .try_insert(FloatingGeometry::new(frame));
            }
        }
        applied += 1;
    }
    Ok(applied)
}

/// A saved frame as a representable window frame.
fn checked_frame_from(frame: spool_shared_types::state::Frame) -> Option<IRect> {
    let origin = IVec2::new(frame.x, frame.y);
    let size = IVec2::new(frame.width, frame.height);
    crate::ecs::window_frame::checked_window_frame(origin, size)
}

/// The startup owner: freezes the candidate baseline once, after initial
/// discovery and before current-session edits are admitted.
pub(crate) fn freeze_restore_baseline(
    candidates: Option<ResMut<RestoreCandidates>>,
    strips: Query<&LayoutStrip>,
    initializing: Option<Res<crate::ecs::Initializing>>,
    mut frozen: Local<bool>,
) {
    let Some(mut candidates) = candidates else {
        return;
    };
    if *frozen || initializing.is_some() || strips.iter().next().is_none() {
        return;
    }
    match candidates.freeze_initial_layouts(strips.iter()) {
        Ok(()) => *frozen = true,
        Err(error) => warn!(%error, "unable to freeze the startup import baseline"),
    }
}

/// How long a startup owner may submit trusted mappings before the candidate set
/// closes. An implementation default, not a measured native value.
const RESTORE_WINDOW: Duration = Duration::from_secs(30);

/// Closes the startup import window when no import arrives in time.
pub(crate) fn close_restore_window(
    candidates: Option<ResMut<RestoreCandidates>>,
    time: Res<Time>,
    mut elapsed: Local<Duration>,
) {
    let Some(mut candidates) = candidates else {
        return;
    };
    if !candidates.import_window_open {
        return;
    }
    *elapsed = elapsed.saturating_add(time.delta());
    if *elapsed >= RESTORE_WINDOW {
        candidates.close_import_window();
        debug!("startup intent import window closed after {RESTORE_WINDOW:?}");
    }
}

/// Where a startup owner's trusted mappings are applied.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct RestoreIntentsCtx<'w, 's> {
    candidates: Option<ResMut<'w, RestoreCandidates>>,
    strips: Query<'w, 's, &'static mut LayoutStrip>,
    windows: Query<
        'w,
        's,
        (
            Entity,
            &'static Window,
            &'static bevy::ecs::hierarchy::ChildOf,
            Option<&'static mut FloatingGeometry>,
        ),
    >,
    declared: Query<
        'w,
        's,
        (
            Entity,
            &'static Window,
            &'static bevy::ecs::hierarchy::ChildOf,
            Option<&'static mut crate::ecs::native_space::DeclaredSpace>,
        ),
    >,
    topology: Res<'w, crate::ecs::topology::NativeTopology>,
    apps: Query<'w, 's, &'static crate::manager::Application>,
    commands: Commands<'w, 's>,
}

/// Applies the trusted mappings a startup owner submitted.
///
/// Every binding is validated before any is applied, so one bad binding refuses
/// the whole import instead of applying part of it. Nothing here writes to the
/// platform: it changes retained intent, and the effect layer realizes it later.
#[allow(
    clippy::too_many_lines,
    reason = "one entry point validates and applies every trusted mapping group in order"
)]
pub(crate) fn restore_intents(
    In(bindings): In<spool_shared_types::commands::RestoreBindings>,
    mut ctx: RestoreIntentsCtx,
) -> crate::errors::Result<()> {
    let Some(mut candidates) = ctx.candidates.take() else {
        return Err(crate::errors::Error::rejected("import_unavailable"));
    };
    if !candidates.import_window_open {
        return Err(crate::errors::Error::rejected("import_window_expired"));
    }
    if candidates.initial_layouts.is_none() {
        return Err(crate::errors::Error::rejected("import_baseline_missing"));
    }

    let mut columns: HashMap<crate::platform::WorkspaceId, Vec<TrustedColumnBinding>> =
        HashMap::new();
    for binding in &bindings.columns {
        let strip = ctx
            .strips
            .iter()
            .find(|strip| strip.id() == binding.target_space)
            .ok_or_else(|| crate::errors::Error::rejected("import_target_space_not_found"))?;
        let trusted = TrustedColumnBinding::new(
            binding.candidate_space,
            binding.candidate_column,
            &candidates,
            strip,
            ColumnId(binding.target_column),
        )
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("import_binding_rejected", error)
        })?;
        columns
            .entry(binding.target_space)
            .or_default()
            .push(trusted);
    }

    let mut floating = Vec::new();
    for binding in &bindings.floating {
        let Some((_, window, parent, _)) = ctx
            .windows
            .iter()
            .find(|(_, window, _, _)| window.id() == binding.target_window_id)
        else {
            return Err(crate::errors::Error::rejected(
                "import_target_window_not_tracked",
            ));
        };
        let app = ctx.apps.get(parent.parent()).map_err(|error| {
            crate::errors::Error::rejection_with_cause("import_target_window_unavailable", error)
        })?;
        let bundle_id = app.bundle_id().unwrap_or_default();
        let trusted = TrustedFloatingBinding::new(
            binding.candidate_window,
            &candidates,
            window.id(),
            app.pid(),
            &bundle_id,
        )
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("import_binding_rejected", error)
        })?;
        floating.push(trusted);
    }

    let mut membership = Vec::new();
    for binding in &bindings.membership {
        // A declared Space must be a user Space that exists: the invariant the
        // declaration keeps, checked here rather than repaired after the fact.
        if !ctx
            .strips
            .iter()
            .any(|strip| strip.id() == binding.target_space)
        {
            return Err(crate::errors::Error::rejected(
                "import_target_space_not_found",
            ));
        }
        if ctx.topology.is_fullscreen(binding.target_space) {
            return Err(crate::errors::Error::rejected(
                "import_target_space_not_user",
            ));
        }
        let Some((_, window, parent, _)) = ctx
            .declared
            .iter()
            .find(|(_, window, _, _)| window.id() == binding.target_window_id)
        else {
            return Err(crate::errors::Error::rejected(
                "import_target_window_not_tracked",
            ));
        };
        let app = ctx.apps.get(parent.parent()).map_err(|error| {
            crate::errors::Error::rejection_with_cause("import_target_window_unavailable", error)
        })?;
        let bundle_id = app.bundle_id().unwrap_or_default();
        let trusted = TrustedMembershipBinding::new(
            binding.candidate_membership,
            &candidates,
            window.id(),
            app.pid(),
            &bundle_id,
            binding.target_space,
        )
        .map_err(|error| {
            crate::errors::Error::rejection_with_cause("import_binding_rejected", error)
        })?;
        membership.push(trusted);
    }

    let mut imported = 0;
    for (space, trusted) in columns {
        let Some(mut strip) = ctx.strips.iter_mut().find(|strip| strip.id() == space) else {
            continue;
        };
        imported += import_intents(&candidates, &mut strip, &trusted)
            .map_err(|error| crate::errors::Error::rejection_with_cause("import_failed", error))?;
    }
    imported += import_floating_frames(&candidates, &mut ctx.windows, &mut ctx.commands, &floating)
        .map_err(|error| crate::errors::Error::rejection_with_cause("import_failed", error))?;
    imported += import_declared_spaces(
        &candidates,
        &mut ctx.declared,
        &mut ctx.commands,
        &membership,
    )
    .map_err(|error| crate::errors::Error::rejection_with_cause("import_failed", error))?;

    if imported > 0 {
        // The startup owner asked explicitly; its window closes with the request.
        candidates.close_import_window();
    }
    debug!(imported, "trusted intent import applied");
    Ok(())
}

/// One membership candidate, already validated against the live window it claims.
#[derive(Clone, Debug)]
pub struct TrustedMembershipBinding {
    candidate_membership: usize,
    target_window_id: WinID,
    target_space: crate::platform::WorkspaceId,
}

impl TrustedMembershipBinding {
    /// The caller must prove the mapping, and the candidate's cached identity must
    /// agree with the live window: a native `WindowServer` id has no documented
    /// lifetime or reuse behaviour and never authorizes a binding by itself.
    pub fn new(
        candidate_membership: usize,
        candidates: &RestoreCandidates,
        target_window_id: WinID,
        pid: Pid,
        bundle_id: &str,
        target_space: crate::platform::WorkspaceId,
    ) -> Result<Self> {
        if !candidates.import_window_open || candidates.initial_layouts.is_none() {
            return Err(Error::InvalidInput(
                "intent import candidates are invalid or expired".into(),
            ));
        }
        let saved = candidates
            .state
            .membership
            .get(candidate_membership)
            .ok_or_else(|| Error::InvalidInput("candidate membership does not exist".into()))?;
        if saved.window_id != target_window_id || saved.pid != pid || saved.bundle_id != bundle_id {
            return Err(Error::InvalidInput(
                "candidate membership's cached identity does not match the live window".into(),
            ));
        }
        Ok(Self {
            candidate_membership,
            target_window_id,
            target_space,
        })
    }
}

/// Declares the Space an imported membership candidate names.
///
/// The declared Space is retained state, so nothing here reaches the platform:
/// the effect layer realizes it when native conditions permit. All-or-nothing,
/// like the other groups: the caller validates every binding first.
pub fn import_declared_spaces(
    candidates: &RestoreCandidates,
    windows: &mut Query<(
        Entity,
        &Window,
        &bevy::ecs::hierarchy::ChildOf,
        Option<&mut crate::ecs::native_space::DeclaredSpace>,
    )>,
    commands: &mut Commands,
    bindings: &[TrustedMembershipBinding],
) -> Result<usize> {
    if !candidates.import_window_open || !candidates.state.valid() {
        return Err(Error::InvalidInput(
            "intent import candidates are invalid or expired".into(),
        ));
    }
    let mut seen_candidates = HashSet::new();
    let mut seen_windows = HashSet::new();
    let mut applied = 0;
    for binding in bindings {
        if !seen_candidates.insert(binding.candidate_membership)
            || !seen_windows.insert(binding.target_window_id)
        {
            return Err(Error::InvalidInput(
                "duplicate membership import binding".into(),
            ));
        }
        let entity = windows
            .iter()
            .find_map(|(entity, window, _, _)| {
                (window.id() == binding.target_window_id).then_some(entity)
            })
            .ok_or_else(|| Error::InvalidInput("import target window is not tracked".into()))?;
        let Ok((_, _, _, declared)) = windows.get_mut(entity) else {
            return Err(Error::InvalidInput(
                "import target window is unavailable".into(),
            ));
        };
        match declared {
            Some(mut declared) => declared.declare(binding.target_space),
            None => {
                commands
                    .entity(entity)
                    .try_insert(crate::ecs::native_space::DeclaredSpace::new(Some(
                        binding.target_space,
                    )));
            }
        }
        applied += 1;
    }
    Ok(applied)
}

//! Isolated intent candidates and a pure import seam. There is deliberately no
//! native identity matcher, startup observer, timer, or automatic restore writer.

#![allow(
    dead_code,
    reason = "the first intent slice exposes only a trusted-mapping pure import seam; no runtime automatic binding provider exists"
)]

use std::collections::{HashMap, HashSet};

use bevy::ecs::resource::Resource;

use crate::ecs::layout::{ColumnId, LayoutStrip, StackItemId};
use crate::ecs::state::SpoolState;
use crate::errors::{Error, Result};
use bevy::ecs::entity::Entity;

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
            crate::ecs::layout::Column::Fullscren(_) => {
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

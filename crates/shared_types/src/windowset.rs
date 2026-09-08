//! A window layout as a pure value, in the style of xmonad's `StackSet`: every
//! transform returns a new [`WindowSet`] rather than mutating the one it was
//! given, and records what it did as a [`LayoutOp`] so the host can replay the
//! change against the live layout.
//!
//! Each level is behind an [`Arc`], so cloning is cheap and a transform copies
//! only the spine it touches. `Arc` rather than `Rc` because the value is built
//! on the window manager's thread and handed to the interpreter on another.
//!
//! Only transforms that follow from the layout alone are here (focus,
//! ordering, workspace membership, stacking, floating, width ratios); ops the
//! layout engine decides (centring, equalise, raising a float, ...) stay as
//! imperative `spool.action.window.*` functions instead. The returned tree is a
//! prediction — the layout engine settles the actual geometry.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::state::Frame;

/// A window's id, as the accessibility layer reports it.
pub type WinID = i32;

/// A tracked window's identity within one daemon session. Both ECS entities
/// and native accessibility handles can be replaced while reusing an ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowIdentity {
    pub entity: u64,
    pub incarnation: u64,
}

/// Immutable provenance of a host snapshot, independent of its predicted
/// layout. A zero/default session is an unbound, pure fixture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub session: [u8; 16],
    pub windows: BTreeMap<WinID, WindowIdentity>,
}

/// Operations and the original identities they address. Hosts validate these
/// bindings at execution, including after any deferred handoff.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutPlan {
    pub snapshot: Arc<LayoutSnapshot>,
    pub ops: Vec<LayoutOp>,
}

impl LayoutPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// What a transform meant, as opposed to what it did to the tree. Replayed
/// against the live world when a handler returns the value carrying it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutOp {
    /// Give `window` the focus.
    Focus(WinID),
    /// Exchange the two windows' positions in the layout.
    Swap(WinID, WinID),
    /// Send `window` to a native Space, optionally following it there.
    MoveToWorkspace {
        window: WinID,
        space_id: u64,
        follow: bool,
    },
    /// Focus a native Space by stable session ID.
    View { space_id: u64 },
    /// Take `window` out of the tiling layout, or put it back in.
    SetFloating { window: WinID, floating: bool },
    /// Set the column width `window` occupies, as a fraction of the display.
    SetWidth { window: WinID, ratio: f64 },
    /// Put `window` at an exact frame. Only meaningful for a floating window:
    /// the layout engine owns where a tiled one goes.
    SetFrame { window: WinID, frame: Frame },
    /// Put `window` into `onto`'s column, as a stack entry or a tab.
    Stack {
        window: WinID,
        onto: WinID,
        tabs: bool,
    },
    /// Give `window` a column of its own again.
    Unstack(WinID),
}

impl LayoutOp {
    /// Every explicitly named endpoint, including a swap/stack's destination.
    pub fn targets(&self) -> impl Iterator<Item = WinID> {
        let other = match self {
            Self::Swap(_, other) | Self::Stack { onto: other, .. } => Some(*other),
            _ => None,
        };
        self.target().into_iter().chain(other)
    }

    /// The window this op acts on, if it names one. `None` for ops that act on
    /// a workspace as a whole.
    #[must_use]
    pub fn target(&self) -> Option<WinID> {
        match self {
            LayoutOp::Focus(window)
            | LayoutOp::Swap(window, _)
            | LayoutOp::Unstack(window)
            | LayoutOp::MoveToWorkspace { window, .. }
            | LayoutOp::SetFloating { window, .. }
            | LayoutOp::SetWidth { window, .. }
            | LayoutOp::SetFrame { window, .. }
            | LayoutOp::Stack { window, .. } => Some(*window),
            LayoutOp::View { .. } => None,
        }
    }
}

/// A rectangle as fractions of a display, in the style of xmonad's
/// `RationalRect`. Proportional rather than absolute, so a scratchpad
/// placement means the same thing on a laptop panel and an external display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RelativeRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl RelativeRect {
    /// Resolves against a display's bounds.
    #[must_use]
    pub fn resolve(self, display: Frame) -> Frame {
        // The clamp is the truncation guard: whatever a script passes lands
        // inside `i32` before the cast, so there is nothing left to truncate.
        #[allow(clippy::cast_possible_truncation)]
        let scale = |fraction: f64, extent: i32| -> i32 {
            let scaled = fraction * f64::from(extent);
            // Saturating rather than wrapping: a script can pass anything.
            if scaled.is_finite() {
                scaled
                    .round()
                    .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
            } else {
                0
            }
        };
        Frame {
            x: display.x.saturating_add(scale(self.x, display.width)),
            y: display.y.saturating_add(scale(self.y, display.height)),
            width: scale(self.width, display.width).max(1),
            height: scale(self.height, display.height).max(1),
        }
    }
}

/// One link of the recorded op list. A cons list rather than a `Vec` so two
/// values branched off the same parent get independent tails without copying
/// the shared prefix.
#[derive(Debug)]
struct OpNode {
    op: LayoutOp,
    prev: Option<Arc<OpNode>>,
}

/// How a column arranges the windows in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    /// One window filling the column.
    Single,
    /// Windows stacked vertically, all visible.
    Stack,
    /// Windows sharing the column, one visible at a time.
    Tabs,
    /// One window covering the display.
    Fullscreen,
}

/// One window, as a script sees it.
// These flags are independent: a floating window can be visible and focused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRec {
    pub id: WinID,
    pub app_name: String,
    pub bundle_id: String,
    pub title: String,
    /// Where it is now, in global display coordinates, when known.
    pub frame: Option<Frame>,
    /// Outside the tiling layout, positioned by hand.
    pub floating: bool,
    /// More than a sliver of it is actually showing.
    pub visible: bool,
    pub focused: bool,
}

/// One indivisible layout entry, including a native tab group inside a stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackItemSet {
    Single(WindowRec),
    Tabs(Arc<Vec<WindowRec>>),
}

impl StackItemSet {
    /// Builds an entry from the tracked members of one native group.
    #[must_use]
    pub fn from_windows(mut windows: Vec<WindowRec>) -> Option<Self> {
        match windows.len() {
            0 => None,
            1 => windows.pop().map(Self::Single),
            _ => Some(Self::Tabs(Arc::new(windows))),
        }
    }

    #[must_use]
    pub fn windows(&self) -> &[WindowRec] {
        match self {
            Self::Single(window) => std::slice::from_ref(window),
            Self::Tabs(windows) => windows,
        }
    }

    fn windows_mut(&mut self) -> &mut [WindowRec] {
        match self {
            Self::Single(window) => std::slice::from_mut(window),
            Self::Tabs(windows) => Arc::make_mut(windows).as_mut_slice(),
        }
    }
}

/// One column of a workspace's layout strip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnSet {
    pub kind: ColumnKind,
    /// Width as a fraction of the display, as the layout engine has it.
    pub width_ratio: f64,
    /// The selected index in the flattened `windows()` order.
    pub selected: usize,
    pub items: Arc<Vec<StackItemSet>>,
}

impl ColumnSet {
    /// A column holding one window.
    #[must_use]
    pub fn single(window: WindowRec, width_ratio: f64) -> Self {
        Self::from_item(StackItemSet::Single(window), width_ratio)
    }

    fn from_item(item: StackItemSet, width_ratio: f64) -> Self {
        Self {
            kind: match item {
                StackItemSet::Single(_) => ColumnKind::Single,
                StackItemSet::Tabs(_) => ColumnKind::Tabs,
            },
            width_ratio,
            selected: 0,
            items: Arc::new(vec![item]),
        }
    }

    /// Builds a nonempty column, retaining each native tab group's identity.
    #[must_use]
    pub fn from_items(items: Vec<StackItemSet>, width_ratio: f64) -> Option<Self> {
        let mut column = Self {
            kind: ColumnKind::Stack,
            width_ratio,
            selected: 0,
            items: Arc::new(items),
        };
        column.normalize();
        (!column.items.is_empty()).then_some(column)
    }

    pub fn windows(&self) -> impl Iterator<Item = &WindowRec> {
        self.items.iter().flat_map(StackItemSet::windows)
    }

    /// The window on top: the only one for a `Single`, the selected one
    /// otherwise.
    #[must_use]
    pub fn top(&self) -> Option<&WindowRec> {
        self.windows()
            .nth(self.selected)
            .or_else(|| self.windows().next())
    }

    fn take_window(&mut self, id: WinID) -> Option<WindowRec> {
        let slot = self.item_of(id)?;
        let selected = self.top().map(|window| window.id);
        let items = Arc::make_mut(&mut self.items);
        let taken = match &mut items[slot] {
            StackItemSet::Single(_) => {
                let StackItemSet::Single(window) = items.remove(slot) else {
                    unreachable!("single item checked above");
                };
                window
            }
            StackItemSet::Tabs(windows) => {
                let at = windows.iter().position(|window| window.id == id)?;
                Arc::make_mut(windows).remove(at)
            }
        };
        self.normalize();
        self.restore_selected(selected);
        Some(taken)
    }

    fn item_of(&self, id: WinID) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.windows().iter().any(|window| window.id == id))
    }

    fn take_item(&mut self, id: WinID) -> Option<StackItemSet> {
        let slot = self.item_of(id)?;
        let selected = self.top().map(|window| window.id);
        let item = Arc::make_mut(&mut self.items).remove(slot);
        self.normalize();
        self.restore_selected(selected);
        Some(item)
    }

    fn normalize(&mut self) {
        let items = Arc::make_mut(&mut self.items);
        items.retain(|item| !item.windows().is_empty());
        for item in items.iter_mut() {
            if let StackItemSet::Tabs(windows) = item
                && windows.len() == 1
            {
                *item = StackItemSet::Single(windows[0].clone());
            }
        }
        self.kind = match items.as_slice() {
            [StackItemSet::Single(_)] if self.kind == ColumnKind::Fullscreen => {
                ColumnKind::Fullscreen
            }
            [StackItemSet::Single(_)] => ColumnKind::Single,
            [StackItemSet::Tabs(_)] => ColumnKind::Tabs,
            _ => ColumnKind::Stack,
        };
        self.selected = self.selected.min(self.windows().count().saturating_sub(1));
    }

    fn restore_selected(&mut self, id: Option<WinID>) {
        let index = self.windows().position(|window| Some(window.id) == id);
        if let Some(index) = index {
            self.selected = index;
        }
    }

    fn selected_slot(&self) -> (usize, usize) {
        let mut offset = self.selected;
        for (slot, item) in self.items.iter().enumerate() {
            if offset < item.windows().len() {
                return (slot, offset);
            }
            offset = offset.saturating_sub(item.windows().len());
        }
        (0, 0)
    }

    fn select_slot(&mut self, slot: usize, offset: usize) {
        self.selected = self
            .items
            .iter()
            .take(slot)
            .map(|item| item.windows().len())
            .sum::<usize>()
            + self
                .items
                .get(slot)
                .map_or(0, |item| offset.min(item.windows().len().saturating_sub(1)));
    }
}

/// One native Space: an ordered strip of columns, plus whatever floats above it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSet {
    /// The macOS Space identity for this session.
    pub space_id: u64,
    /// Current per-display presentation order.
    pub ordinal: u32,
    /// Whether it is the one currently shown on its display.
    pub active: bool,
    pub columns: Arc<Vec<ColumnSet>>,
    pub floating: Arc<Vec<WindowRec>>,
}

impl WorkspaceSet {
    /// Every window on the workspace, tiled first then floating.
    pub fn windows(&self) -> impl Iterator<Item = &WindowRec> {
        self.columns
            .iter()
            .flat_map(ColumnSet::windows)
            .chain(self.floating.iter())
    }

    fn take_window(&mut self, id: WinID) -> Option<WindowRec> {
        let columns = Arc::make_mut(&mut self.columns);
        for index in 0..columns.len() {
            if let Some(taken) = columns[index].take_window(id) {
                if columns[index].items.is_empty() {
                    columns.remove(index);
                }
                return Some(taken);
            }
        }
        let floating = Arc::make_mut(&mut self.floating);
        let at = floating.iter().position(|window| window.id == id)?;
        Some(floating.remove(at))
    }
}

/// One display and the workspaces on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaySet {
    pub id: u32,
    pub frame: Frame,
    /// Whether it holds the focus.
    pub active: bool,
    pub workspaces: Arc<Vec<WorkspaceSet>>,
}

/// The whole layout, as a value.
///
/// See the module documentation for what "as a value" buys and what it costs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WindowSet {
    displays: Arc<Vec<DisplaySet>>,
    focused: Option<WinID>,
    snapshot: Arc<LayoutSnapshot>,
    /// What has been asked of this value, most recent first. Not part of the
    /// layout: two window sets are equal when they describe the same layout,
    /// however they got there, so this field is excluded from `PartialEq` and
    /// not serialized.
    #[serde(skip)]
    ops: Option<Arc<OpNode>>,
}

impl PartialEq for WindowSet {
    fn eq(&self, other: &Self) -> bool {
        self.displays == other.displays && self.focused == other.focused
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

impl WindowSet {
    /// Builds a window set from an extracted layout.
    #[must_use]
    pub fn new(displays: Vec<DisplaySet>, focused: Option<WinID>) -> Self {
        Self {
            displays: Arc::new(displays),
            focused,
            snapshot: Arc::default(),
            ops: None,
        }
    }

    /// Attaches the host's original identities before handing the value to a
    /// script. Transforms and serialization retain them without rebinding IDs.
    #[must_use]
    pub fn with_snapshot(mut self, snapshot: LayoutSnapshot) -> Self {
        self.snapshot = Arc::new(snapshot);
        self
    }

    #[must_use]
    pub fn displays(&self) -> &[DisplaySet] {
        &self.displays
    }

    /// The focused window's id, if anything is focused.
    #[must_use]
    pub fn focused(&self) -> Option<WinID> {
        self.focused
    }

    /// Every workspace, across every display.
    pub fn workspaces(&self) -> impl Iterator<Item = &WorkspaceSet> {
        self.displays
            .iter()
            .flat_map(|display| display.workspaces.iter())
    }

    /// Every window known to the layout.
    pub fn windows(&self) -> impl Iterator<Item = &WindowRec> {
        self.workspaces().flat_map(WorkspaceSet::windows)
    }

    /// One window by id.
    #[must_use]
    pub fn window(&self, id: WinID) -> Option<&WindowRec> {
        self.windows().find(|window| window.id == id)
    }

    /// The native Space identified by `space_id`.
    #[must_use]
    pub fn workspace(&self, space_id: u64) -> Option<&WorkspaceSet> {
        self.workspaces()
            .find(|workspace| workspace.space_id == space_id)
    }

    /// The active workspace of the active display — "here", for a script.
    /// Unknown or ambiguous active observations do not identify a workspace.
    #[must_use]
    pub fn current(&self) -> Option<&WorkspaceSet> {
        let display = unique(self.displays.iter().filter(|display| display.active))?;
        unique(
            display
                .workspaces
                .iter()
                .filter(|workspace| workspace.active),
        )
    }

    /// The display showing `id`.
    #[must_use]
    pub fn display_of(&self, id: WinID) -> Option<&DisplaySet> {
        self.displays.iter().find(|display| {
            display
                .workspaces
                .iter()
                .any(|workspace| workspace.windows().any(|window| window.id == id))
        })
    }

    /// The workspace holding `id`.
    #[must_use]
    pub fn workspace_of(&self, id: WinID) -> Option<&WorkspaceSet> {
        self.workspaces()
            .find(|workspace| workspace.windows().any(|window| window.id == id))
    }

    /// Which column of its workspace holds `id`, counted from the left.
    #[must_use]
    pub fn column_of(&self, id: WinID) -> Option<usize> {
        let workspace = self.workspace_of(id)?;
        workspace
            .columns
            .iter()
            .position(|column| column.item_of(id).is_some())
    }

    /// The window one column to the east (right), staying on the workspace.
    #[must_use]
    pub fn east(&self, id: WinID) -> Option<WinID> {
        self.neighbour(id, 1)
    }

    /// The window one column to the west (left), staying on the workspace.
    #[must_use]
    pub fn west(&self, id: WinID) -> Option<WinID> {
        self.neighbour(id, -1)
    }

    /// The next window in the workspace's own order, wrapping at the end.
    #[must_use]
    pub fn next(&self, id: WinID) -> Option<WinID> {
        self.cycle(id, 1)
    }

    /// The previous window in the workspace's own order, wrapping at the start.
    #[must_use]
    pub fn prev(&self, id: WinID) -> Option<WinID> {
        self.cycle(id, -1)
    }

    /// The top window of the column `offset` columns away, if there is one.
    fn neighbour(&self, id: WinID, offset: isize) -> Option<WinID> {
        let workspace = self.workspace_of(id)?;
        let column = self.column_of(id)?;
        let target = usize::try_from(isize::try_from(column).ok()? + offset).ok()?;
        workspace.columns.get(target)?.top().map(|window| window.id)
    }

    /// The window `offset` places away in workspace order, wrapping around.
    fn cycle(&self, id: WinID, offset: isize) -> Option<WinID> {
        let workspace = self.workspace_of(id)?;
        let ids: Vec<WinID> = workspace.windows().map(|window| window.id).collect();
        if ids.is_empty() {
            return None;
        }
        let at = ids.iter().position(|&window| window == id)?;
        let length = isize::try_from(ids.len()).ok()?;
        let index = (isize::try_from(at).ok()? + offset).rem_euclid(length);
        ids.get(usize::try_from(index).ok()?).copied()
    }

    /// The ops recorded on this value, oldest first, for inspection. Execution
    /// must use [`Self::plan`] so it does not discard the original identities.
    #[must_use]
    pub fn ops(&self) -> Vec<LayoutOp> {
        let mut ops = Vec::new();
        let mut node = self.ops.as_ref();
        while let Some(current) = node {
            ops.push(current.op);
            node = current.prev.as_ref();
        }
        ops.reverse();
        ops
    }

    #[must_use]
    pub fn plan(&self) -> LayoutPlan {
        LayoutPlan {
            snapshot: Arc::clone(&self.snapshot),
            ops: self.ops(),
        }
    }

    /// Whether anything has been asked of this value.
    #[must_use]
    pub fn is_transformed(&self) -> bool {
        self.ops.is_some()
    }
}

// ---------------------------------------------------------------------------
// Transforming
// ---------------------------------------------------------------------------

impl WindowSet {
    /// Returns a copy of `self` with `op` recorded and `edit` applied to the
    /// tree. The engine every transform below is built from: `self` is never
    /// touched, and only the spine `edit` reaches gets copied.
    fn with(&self, op: LayoutOp, edit: impl FnOnce(&mut [DisplaySet])) -> Self {
        let mut next = self.recording(op);
        let displays: &mut Vec<DisplaySet> = Arc::make_mut(&mut next.displays);
        edit(displays);
        next
    }

    /// Records `op` without changing the tree at all — not even copying it.
    fn recording(&self, op: LayoutOp) -> Self {
        Self {
            displays: Arc::clone(&self.displays),
            focused: self.focused,
            snapshot: Arc::clone(&self.snapshot),
            ops: Some(Arc::new(OpNode {
                op,
                prev: self.ops.clone(),
            })),
        }
    }

    fn set_focus(&mut self, window: Option<WinID>) {
        self.focused = window;
        let displays = Arc::make_mut(&mut self.displays);
        for_each_window(displays, |record| {
            record.focused = Some(record.id) == window;
        });
        if window.is_some() {
            for_each_column(displays, |column| column.restore_selected(window));
        }
    }

    /// Focuses `window`.
    #[must_use]
    pub fn focus(&self, window: WinID) -> Self {
        let mut next = self.recording(LayoutOp::Focus(window));
        let Some(target) = self
            .workspace_of(window)
            .and_then(|workspace| workspace_location(&self.displays, workspace.space_id))
        else {
            return next;
        };
        activate_workspace(Arc::make_mut(&mut next.displays).as_mut_slice(), target);
        next.set_focus(Some(window));
        next
    }

    /// Exchanges two windows' places in the layout. A no-op on the tree if
    /// either is missing, though the intent is still recorded — the host may
    /// well be able to resolve a window this snapshot has already lost.
    #[must_use]
    pub fn swap(&self, first: WinID, second: WinID) -> Self {
        self.with(LayoutOp::Swap(first, second), |displays| {
            let Some((workspace, left, right)) = tiled_pair_mut(displays, first, second) else {
                return;
            };
            let columns = Arc::make_mut(&mut workspace.columns);
            let (Some(first_slot), Some(second_slot)) =
                (columns[left].item_of(first), columns[right].item_of(second))
            else {
                return;
            };
            if left == right && first_slot == second_slot {
                return;
            }
            let left_selected = columns[left].selected_slot();
            let right_selected = columns[right].selected_slot();
            if left == right {
                Arc::make_mut(&mut columns[left].items).swap(first_slot, second_slot);
            } else {
                let first_item = columns[left].items[first_slot].clone();
                let second_item = columns[right].items[second_slot].clone();
                Arc::make_mut(&mut columns[left].items)[first_slot] = second_item;
                Arc::make_mut(&mut columns[right].items)[second_slot] = first_item;
            }
            columns[left].normalize();
            columns[right].normalize();
            columns[left].select_slot(left_selected.0, left_selected.1);
            columns[right].select_slot(right_selected.0, right_selected.1);
        })
    }

    /// Sends `window` to native Space `space_id`, without following it.
    #[must_use]
    pub fn shift(&self, window: WinID, space_id: u64) -> Self {
        self.shift_following(window, space_id, false)
    }

    /// Sends `window` to native Space `space_id`, optionally following it there.
    #[must_use]
    pub fn shift_following(&self, window: WinID, space_id: u64, follow: bool) -> Self {
        let mut next = self.recording(LayoutOp::MoveToWorkspace {
            window,
            space_id,
            follow,
        });
        let Some(source) = self.workspace_of(window) else {
            return next;
        };
        // Resolve the destination before lifting anything out of the tree.
        let Some(target) = workspace_location(&self.displays, space_id) else {
            return next;
        };
        if source.space_id != space_id {
            let displays = Arc::make_mut(&mut next.displays);
            let Some(mut record) = take_window(displays, window) else {
                return next;
            };
            record.visible = false;
            let workspace = &mut Arc::make_mut(&mut displays[target.0].workspaces)[target.1];
            if record.floating {
                Arc::make_mut(&mut workspace.floating).push(record);
            } else {
                Arc::make_mut(&mut workspace.columns).push(ColumnSet::single(record, 0.5));
            }
        }
        if follow {
            activate_workspace(Arc::make_mut(&mut next.displays).as_mut_slice(), target);
            next.set_focus(Some(window));
        } else if source.space_id != space_id && self.focused == Some(window) {
            next.set_focus(None);
        }
        next
    }

    /// Requests focus for native Space `space_id`.
    #[must_use]
    pub fn view(&self, space_id: u64) -> Self {
        let mut next = self.recording(LayoutOp::View { space_id });
        let Some(target) = workspace_location(&self.displays, space_id) else {
            return next;
        };
        let focused = self.focused.filter(|&window| {
            self.workspace_of(window)
                .is_some_and(|workspace| workspace.space_id == space_id)
        });
        activate_workspace(Arc::make_mut(&mut next.displays).as_mut_slice(), target);
        next.set_focus(focused);
        next
    }

    /// Takes `window` out of the tiling layout, leaving it where it is —
    /// xmonad's `defaultFloating`.
    #[must_use]
    pub fn float(&self, window: WinID) -> Self {
        self.set_floating(window, true)
    }

    /// Takes `window` out of the tiling layout and puts it at `rect` —
    /// xmonad's `customFloating`.
    ///
    /// The fractions are resolved against the display the window is on *in this
    /// snapshot*, so the op carries an absolute frame and the returned tree can
    /// show where the window ended up.
    #[must_use]
    pub fn float_at(&self, window: WinID, rect: RelativeRect) -> Self {
        let display = self
            .display_of(window)
            .or_else(|| self.displays.iter().find(|display| display.active))
            .or_else(|| self.displays.first());
        let Some(frame) = display.map(|display| rect.resolve(display.frame)) else {
            // No display to resolve against; floating alone is still meaningful.
            return self.float(window);
        };
        self.float(window).set_frame(window, frame)
    }

    /// Puts `window` at an exact frame, in global display coordinates.
    #[must_use]
    pub fn set_frame(&self, window: WinID, frame: Frame) -> Self {
        self.with(LayoutOp::SetFrame { window, frame }, |displays| {
            for_each_window(displays, |record| {
                if record.id == window {
                    record.frame = Some(frame);
                }
            });
        })
    }

    /// Puts a floating `window` back into the tiling layout.
    #[must_use]
    pub fn sink(&self, window: WinID) -> Self {
        self.set_floating(window, false)
    }

    fn set_floating(&self, window: WinID, floating: bool) -> Self {
        let source = self
            .workspace_of(window)
            .map(|workspace| workspace.space_id);
        let changes_mode = self
            .window(window)
            .is_some_and(|record| record.floating != floating);
        self.with(LayoutOp::SetFloating { window, floating }, |displays| {
            if !changes_mode {
                return;
            }
            let Some(target) = source.and_then(|space| find_workspace_mut(displays, space)) else {
                return;
            };
            let Some(mut record) = target.take_window(window) else {
                return;
            };
            record.floating = floating;
            if floating {
                Arc::make_mut(&mut target.floating).push(record);
            } else {
                Arc::make_mut(&mut target.columns).push(ColumnSet::single(record, 0.5));
            }
        })
    }

    /// Sets the width of `window`'s column, as a fraction of the display.
    #[must_use]
    pub fn width(&self, window: WinID, ratio: f64) -> Self {
        self.with(LayoutOp::SetWidth { window, ratio }, |displays| {
            if !ratio.is_finite() || ratio <= 0.0 {
                return;
            }
            for_each_column(displays, |column| {
                if column.kind != ColumnKind::Fullscreen && column.item_of(window).is_some() {
                    column.width_ratio = ratio;
                }
            });
        })
    }

    /// Puts `window` into `onto`'s column as a stack entry.
    #[must_use]
    pub fn stack(&self, window: WinID, onto: WinID) -> Self {
        self.stack_as(window, onto, false)
    }

    /// Puts `window` into `onto`'s column as a tab.
    #[must_use]
    pub fn tab(&self, window: WinID, onto: WinID) -> Self {
        self.stack_as(window, onto, true)
    }

    fn stack_as(&self, window: WinID, onto: WinID, tabs: bool) -> Self {
        self.with(LayoutOp::Stack { window, onto, tabs }, |displays| {
            let Some((workspace, source, target)) = tiled_pair_mut(displays, window, onto) else {
                return;
            };
            if source == target {
                return;
            }
            let columns = Arc::make_mut(&mut workspace.columns);
            let Some(item) = columns[source].take_item(window) else {
                return;
            };
            Arc::make_mut(&mut columns[target].items).push(item);
            if tabs {
                let windows = columns[target].windows().cloned().collect();
                columns[target].items = Arc::new(vec![StackItemSet::Tabs(Arc::new(windows))]);
            }
            columns[target].normalize();
            if columns[source].items.is_empty() {
                columns.remove(source);
            }
        })
    }

    /// Gives `window` a column of its own again.
    #[must_use]
    pub fn unstack(&self, window: WinID) -> Self {
        self.with(LayoutOp::Unstack(window), |displays| {
            for display in displays {
                for workspace in Arc::make_mut(&mut display.workspaces) {
                    let Some(index) = workspace.columns.iter().position(|column| {
                        column.kind == ColumnKind::Stack
                            && column.items.len() > 1
                            && column.item_of(window).is_some()
                    }) else {
                        continue;
                    };
                    let columns = Arc::make_mut(&mut workspace.columns);
                    let width_ratio = columns[index].width_ratio;
                    let Some(item) = columns[index].take_item(window) else {
                        return;
                    };
                    let column = ColumnSet::from_item(item, width_ratio);
                    columns.insert(index + 1, column);
                    return;
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tree helpers
// ---------------------------------------------------------------------------

fn unique<T>(mut items: impl Iterator<Item = T>) -> Option<T> {
    let item = items.next()?;
    items.next().is_none().then_some(item)
}

fn workspace_location(displays: &[DisplaySet], space_id: u64) -> Option<(usize, usize)> {
    unique(
        displays
            .iter()
            .enumerate()
            .flat_map(|(display_index, display)| {
                display
                    .workspaces
                    .iter()
                    .enumerate()
                    .filter_map(move |(space_index, workspace)| {
                        (workspace.space_id == space_id).then_some((display_index, space_index))
                    })
            }),
    )
}

// Clear visibility when hiding a Space, but never infer it from showing one.
fn activate_workspace(displays: &mut [DisplaySet], target: (usize, usize)) {
    for (index, display) in displays.iter_mut().enumerate() {
        display.active = index == target.0;
        if !display.active {
            continue;
        }
        for (index, workspace) in Arc::make_mut(&mut display.workspaces)
            .iter_mut()
            .enumerate()
        {
            workspace.active = index == target.1;
            if !workspace.active {
                for_each_workspace_window(workspace, |window| window.visible = false);
            }
        }
    }
}

fn for_each_workspace_window(workspace: &mut WorkspaceSet, mut visit: impl FnMut(&mut WindowRec)) {
    for column in Arc::make_mut(&mut workspace.columns) {
        for item in Arc::make_mut(&mut column.items) {
            for window in item.windows_mut() {
                visit(window);
            }
        }
    }
    for window in Arc::make_mut(&mut workspace.floating) {
        visit(window);
    }
}

/// Visits every window record in the tree, copying only what it reaches.
fn for_each_window(displays: &mut [DisplaySet], mut visit: impl FnMut(&mut WindowRec)) {
    for display in displays.iter_mut() {
        for workspace in Arc::make_mut(&mut display.workspaces) {
            for_each_workspace_window(workspace, &mut visit);
        }
    }
}

/// Visits every column in the tree.
fn for_each_column(displays: &mut [DisplaySet], mut visit: impl FnMut(&mut ColumnSet)) {
    for display in displays.iter_mut() {
        for workspace in Arc::make_mut(&mut display.workspaces) {
            for column in Arc::make_mut(&mut workspace.columns) {
                visit(column);
            }
        }
    }
}

/// Resolves both entries before a transform can remove either of them.
fn tiled_pair_mut(
    displays: &mut [DisplaySet],
    first: WinID,
    second: WinID,
) -> Option<(&mut WorkspaceSet, usize, usize)> {
    for display in displays {
        for workspace in Arc::make_mut(&mut display.workspaces) {
            let column_of = |id| {
                workspace.columns.iter().position(|column| {
                    column.kind != ColumnKind::Fullscreen && column.item_of(id).is_some()
                })
            };
            if let (Some(left), Some(right)) = (column_of(first), column_of(second)) {
                return Some((workspace, left, right));
            }
        }
    }
    None
}

/// Finds a native Space by ID, ready to be changed.
fn find_workspace_mut(displays: &mut [DisplaySet], space_id: u64) -> Option<&mut WorkspaceSet> {
    displays.iter_mut().find_map(|display| {
        Arc::make_mut(&mut display.workspaces)
            .iter_mut()
            .find(|workspace| workspace.space_id == space_id)
    })
}

/// Removes a window from wherever it is, leaving no empty column behind, and
/// hands it back for the caller to place somewhere else.
fn take_window(displays: &mut [DisplaySet], id: WinID) -> Option<WindowRec> {
    for display in displays.iter_mut() {
        for workspace in Arc::make_mut(&mut display.workspaces) {
            if let Some(taken) = workspace.take_window(id) {
                return Some(taken);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: WinID, name: &str) -> WindowRec {
        WindowRec {
            id,
            app_name: name.to_string(),
            bundle_id: format!("com.example.{name}"),
            title: format!("{name} window"),
            frame: None,
            floating: false,
            visible: true,
            focused: false,
        }
    }

    /// One display, two workspaces; workspace 1 is active and holds three
    /// single-window columns, workspace 2 is empty.
    fn fixture() -> WindowSet {
        let columns: Vec<ColumnSet> = [(1, "alpha"), (2, "beta"), (3, "gamma")]
            .into_iter()
            .map(|(id, name)| {
                let mut record = window(id, name);
                record.focused = id == 1;
                ColumnSet::single(record, 0.33)
            })
            .collect();

        WindowSet::new(
            vec![DisplaySet {
                id: 1,
                frame: Frame {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                active: true,
                workspaces: Arc::new(vec![
                    WorkspaceSet {
                        space_id: 1,
                        ordinal: 0,
                        active: true,
                        columns: Arc::new(columns),
                        floating: Arc::new(Vec::new()),
                    },
                    WorkspaceSet {
                        space_id: 2,
                        ordinal: 1,
                        active: false,
                        columns: Arc::new(Vec::new()),
                        floating: Arc::new(Vec::new()),
                    },
                ]),
            }],
            Some(1),
        )
    }

    fn multi_display_fixture() -> WindowSet {
        let mut base = fixture();
        let mut hidden = window(5, "remote-hidden");
        hidden.visible = false;
        Arc::make_mut(&mut base.displays).push(DisplaySet {
            id: 2,
            frame: Frame {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            active: false,
            workspaces: Arc::new(vec![
                WorkspaceSet {
                    space_id: 3,
                    ordinal: 0,
                    active: true,
                    columns: Arc::new(vec![ColumnSet::single(window(4, "remote"), 0.5)]),
                    floating: Arc::new(vec![]),
                },
                WorkspaceSet {
                    space_id: 4,
                    ordinal: 1,
                    active: false,
                    columns: Arc::new(vec![ColumnSet::single(hidden, 0.5)]),
                    floating: Arc::new(vec![]),
                },
            ]),
        });
        base
    }

    #[test]
    fn prediction_view_selects_the_target_display_without_hiding_the_other_display() {
        let base = multi_display_fixture();
        let next = base.view(4);
        assert_eq!(next.current().map(|space| space.space_id), Some(4));
        assert_eq!(
            next.displays()
                .iter()
                .filter(|display| display.active)
                .map(|display| display.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(next.workspace(1).unwrap().active);
        assert!(!next.workspace(3).unwrap().active);
        assert!(next.workspace(4).unwrap().active);
        assert_eq!(
            next.focused(),
            None,
            "native Space focus does not identify its selected window"
        );
        assert!(next.window(1).unwrap().visible);
        assert!(!next.window(4).unwrap().visible);
        assert!(
            !next.window(5).unwrap().visible,
            "visibility cannot be invented from a Space switch"
        );
        assert!(next.windows().all(|window| !window.focused));
        assert_eq!(base.current().map(|space| space.space_id), Some(1));
        assert_eq!(next.ops(), vec![LayoutOp::View { space_id: 4 }]);
    }

    #[test]
    fn prediction_focus_updates_its_display_and_space_without_hiding_other_displays() {
        let base = multi_display_fixture();
        let next = base.focus(4);
        assert_eq!(next.current().map(|space| space.space_id), Some(3));
        assert_eq!(next.focused(), Some(4));
        assert!(next.window(4).unwrap().focused);
        assert!(!next.window(1).unwrap().focused);
        assert!(next.workspace(1).unwrap().active);
        assert!(next.window(1).unwrap().visible);
        let hidden_target = next.focus(5);
        assert_eq!(hidden_target.current().map(|space| space.space_id), Some(4));
        assert_eq!(hidden_target.focused(), Some(5));
        assert!(!hidden_target.window(4).unwrap().visible);
        assert_eq!(base.focused(), Some(1));
    }

    #[test]
    fn prediction_following_shift_selects_the_destination_without_recording_extra_ops() {
        let base = fixture();
        let next = base.shift_following(2, 2, true);
        assert_eq!(next.current().map(|space| space.space_id), Some(2));
        assert_eq!(next.focused(), Some(2));
        assert!(next.window(2).unwrap().focused);
        assert!(!next.window(1).unwrap().focused);
        assert!(!next.window(1).unwrap().visible);
        assert_eq!(
            next.ops(),
            vec![LayoutOp::MoveToWorkspace {
                window: 2,
                space_id: 2,
                follow: true
            }]
        );
        assert_eq!(base.workspace_of(2).unwrap().space_id, 1);
    }

    #[test]
    fn prediction_nonfollowing_shift_does_not_keep_focus_on_the_departed_window() {
        let next = fixture().shift(1, 2);
        assert_eq!(next.current().map(|space| space.space_id), Some(1));
        assert_eq!(next.focused(), None);
        assert!(next.windows().all(|window| !window.focused));
        assert!(!next.window(1).unwrap().visible);
        assert_eq!(fixture().shift(2, 2).focused(), Some(1));
    }

    #[test]
    fn prediction_same_space_follow_preserves_order_but_selects_the_window() {
        let base = fixture().view(2);
        let ids = |set: &WindowSet| {
            set.workspace(1)
                .unwrap()
                .windows()
                .map(|window| window.id)
                .collect::<Vec<_>>()
        };
        let next = base.shift_following(2, 1, true);
        assert_eq!(ids(&next), ids(&base));
        assert_eq!(next.current().map(|space| space.space_id), Some(1));
        assert_eq!(next.focused(), Some(2));
    }

    #[test]
    fn prediction_missing_targets_preserve_the_tree_and_keep_only_the_intent() {
        let base = fixture();
        for next in [
            base.focus(99),
            base.view(99),
            base.shift_following(99, 2, true),
            base.shift_following(2, 99, true),
        ] {
            assert_eq!(next, base);
            assert_eq!(next.ops().len(), 1);
        }
    }

    #[test]
    fn prediction_current_does_not_guess_an_unknown_or_ambiguous_display() {
        for active_displays in [0, 2] {
            let mut base = multi_display_fixture();
            for display in Arc::make_mut(&mut base.displays) {
                display.active = active_displays == 2;
            }
            assert!(base.current().is_none());
        }
        let mut base = fixture();
        for workspace in Arc::make_mut(&mut Arc::make_mut(&mut base.displays)[0].workspaces) {
            workspace.active = true;
        }
        assert!(base.current().is_none());
    }

    #[test]
    fn prediction_focus_selects_a_native_tab_without_reordering_the_group() {
        let mut base = fixture();
        let first = base.window(1).unwrap().clone();
        let second = base.window(2).unwrap().clone();
        let third = base.window(3).unwrap().clone();
        Arc::make_mut(&mut Arc::make_mut(&mut base.displays)[0].workspaces)[0].columns =
            Arc::new(vec![
                ColumnSet::from_items(vec![StackItemSet::Tabs(Arc::new(vec![first, second]))], 0.5)
                    .unwrap(),
                ColumnSet::single(third, 0.5),
            ]);
        let next = base.focus(2);
        assert_eq!(next.west(3), Some(2));
        assert_eq!(
            next.workspace(1).unwrap().columns[0]
                .windows()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(base.west(3), Some(1));
    }

    #[test]
    fn prediction_following_a_float_keeps_its_mode_across_displays() {
        let base = multi_display_fixture().float(1);
        let next = base.shift_following(1, 4, true);
        assert_eq!(next.current().unwrap().space_id, 4);
        assert_eq!(next.focused(), Some(1));
        assert!(next.window(1).unwrap().floating);
        assert!(!next.window(1).unwrap().visible);
        assert!(
            next.workspace(4)
                .unwrap()
                .floating
                .iter()
                .any(|window| window.id == 1)
        );
        assert!(next.workspace(1).unwrap().active);
        assert!(next.window(2).unwrap().visible);
        assert!(!next.window(4).unwrap().visible);
        assert_eq!(base.workspace_of(1).unwrap().space_id, 1);
    }

    #[test]
    fn prediction_viewing_the_current_space_keeps_its_known_focus() {
        let base = fixture();
        assert_eq!(base.view(1), base);
        assert_eq!(base.shift_following(1, 1, true), base);
        assert_eq!(base.shift(2, 1), base);
    }

    #[test]
    fn prediction_ambiguous_space_targets_keep_only_the_intent() {
        let mut base = multi_display_fixture();
        let mut duplicate = base.workspace(2).unwrap().clone();
        duplicate.space_id = 4;
        Arc::make_mut(&mut Arc::make_mut(&mut base.displays)[0].workspaces).push(duplicate);
        for next in [
            base.view(4),
            base.focus(5),
            base.shift_following(1, 4, true),
        ] {
            assert_eq!(next, base);
            assert_eq!(next.ops().len(), 1);
        }
    }

    #[cfg(feature = "lua")]
    #[test]
    fn lua_prediction_focus_space_chains_keep_the_original_binding() {
        let identity = LayoutSnapshot {
            session: [19; 16],
            windows: [(
                1,
                WindowIdentity {
                    entity: 37,
                    incarnation: 41,
                },
            )]
            .into(),
        };
        let lua = mlua::Lua::new();
        lua.globals()
            .set(
                "ws",
                multi_display_fixture().with_snapshot(identity.clone()),
            )
            .unwrap();
        let result = lua
            .load(
                r"
            local viewed = ws:view(4)
            assert(viewed:current() == 4)
            assert(viewed:focused() == nil)
            assert(viewed:columns()[1][1] == 5)
            local followed = viewed:shift(1, 4, true)
            assert(followed:focused() == 1)
            assert(followed:window(1).focused)
            assert(followed:display_of(1).active)
            assert(followed:space_of(1) == 4)
            local focused = followed:focus(4)
            assert(focused:current() == 3)
            assert(focused:focused() == 4)
            assert(focused:focus(99):focused() == 4)
            assert(ws:current() == 1 and ws:focused() == 1)
            return focused
        ",
            )
            .eval()
            .unwrap();
        let plan = crate::windowset_lua::returned_plan(&result).unwrap();
        assert_eq!(*plan.snapshot, identity);
        assert_eq!(
            plan.ops,
            vec![
                LayoutOp::View { space_id: 4 },
                LayoutOp::MoveToWorkspace {
                    window: 1,
                    space_id: 4,
                    follow: true
                },
                LayoutOp::Focus(4),
            ]
        );
        let mut unknown = fixture();
        Arc::make_mut(&mut unknown.displays)[0].active = false;
        lua.globals().set("unknown", unknown).unwrap();
        lua.load("assert(unknown:current() == nil); assert(#unknown:columns() == 0)")
            .exec()
            .unwrap();
    }

    #[test]
    fn a_fresh_window_set_has_asked_for_nothing() {
        let set = fixture();
        assert!(set.ops().is_empty());
        assert!(!set.is_transformed());
    }

    #[test]
    fn transformations_and_branches_retain_the_original_snapshot_identity() {
        let identity = LayoutSnapshot {
            session: [73; 16],
            windows: [(
                1,
                WindowIdentity {
                    entity: 41,
                    incarnation: 99,
                },
            )]
            .into(),
        };
        let base = fixture().with_snapshot(identity.clone());
        let left = base.float(1).shift(1, 2).sink(1);
        let right = base.swap(1, 2).unstack(1).focus(3);
        assert_eq!(*left.plan().snapshot, identity);
        assert_eq!(*right.plan().snapshot, identity);
        assert!(Arc::ptr_eq(&left.plan().snapshot, &right.plan().snapshot));
        assert!(base.plan().is_empty());
        assert_eq!(left.plan().ops, left.ops());
        assert_eq!(right.plan().ops, right.ops());
    }

    #[cfg(feature = "lua")]
    #[test]
    fn lua_return_conversion_preserves_the_returned_values_snapshot() {
        let identity = LayoutSnapshot {
            session: [73; 16],
            windows: [(
                1,
                WindowIdentity {
                    entity: 41,
                    incarnation: 99,
                },
            )]
            .into(),
        };
        let lua = mlua::Lua::new();
        lua.globals()
            .set("old", fixture().with_snapshot(identity.clone()))
            .unwrap();
        lua.globals().set("current", fixture()).unwrap();
        let result = lua.load("return old:float(1):shift(1, 2)").eval().unwrap();
        let plan = crate::windowset_lua::returned_plan(&result).unwrap();
        assert_eq!(*plan.snapshot, identity);
        assert_eq!(plan.ops.len(), 2);
        assert!(
            crate::windowset_lua::returned_plan(&mlua::Value::Nil)
                .unwrap()
                .is_empty()
        );
        assert!(crate::windowset_lua::returned_plan(&mlua::Value::Boolean(true)).is_err());
    }

    #[test]
    fn transforms_leave_the_original_alone() {
        let before = fixture();
        let after = before.focus(3);

        assert_eq!(
            before.focused(),
            Some(1),
            "the original still has its focus"
        );
        assert!(
            before.ops().is_empty(),
            "and has still asked for nothing: {:?}",
            before.ops()
        );
        assert_eq!(after.focused(), Some(3));
        assert_eq!(after.ops(), vec![LayoutOp::Focus(3)]);
        assert!(after.window(3).unwrap().focused);
        assert!(!after.window(1).unwrap().focused);
    }

    #[test]
    fn chained_transforms_record_in_order() {
        let set = fixture().focus(2).width(2, 0.75).shift(2, 2);
        assert_eq!(
            set.ops(),
            vec![
                LayoutOp::Focus(2),
                LayoutOp::SetWidth {
                    window: 2,
                    ratio: 0.75
                },
                LayoutOp::MoveToWorkspace {
                    window: 2,
                    space_id: 2,
                    follow: false
                },
            ]
        );
    }

    #[test]
    fn branches_do_not_see_each_others_ops() {
        let base = fixture();
        let left = base.focus(2);
        let right = base.focus(3);

        assert_eq!(left.ops(), vec![LayoutOp::Focus(2)]);
        assert_eq!(right.ops(), vec![LayoutOp::Focus(3)]);
        assert!(base.ops().is_empty());
        assert_eq!(base.focused(), Some(1));
    }

    #[test]
    fn untouched_subtrees_are_shared_not_copied() {
        let base = fixture();
        let clone = base.clone();
        assert!(
            Arc::ptr_eq(&base.displays, &clone.displays),
            "cloning should share, not copy"
        );

        let edited = base.width(1, 0.9);
        let before = &base.displays()[0].workspaces[1];
        let after = &edited.displays()[0].workspaces[1];
        assert_eq!(before, after, "the untouched workspace keeps its contents");
    }

    #[test]
    fn shift_moves_the_window_between_workspaces() {
        let set = fixture().shift(2, 2);
        assert!(
            set.workspace(1).unwrap().windows().all(|w| w.id != 2),
            "the window has left its old workspace"
        );
        assert!(
            set.workspace(2).unwrap().windows().any(|w| w.id == 2),
            "...and arrived at the new one"
        );
        assert_eq!(
            set.workspace(1).unwrap().columns.len(),
            2,
            "its emptied column is gone"
        );
    }

    #[test]
    fn swap_exchanges_positions_not_focus() {
        let set = fixture().swap(1, 3);
        let workspace = set.workspace(1).unwrap();
        let order: Vec<WinID> = workspace
            .columns
            .iter()
            .filter_map(|column| column.top().map(|window| window.id))
            .collect();
        assert_eq!(order, vec![3, 2, 1]);
        assert_eq!(set.focused(), Some(1), "swapping does not move the focus");
    }

    #[test]
    fn stacking_merges_columns_and_unstacking_splits_them() {
        let stacked = fixture().stack(2, 1);
        let workspace = stacked.workspace(1).unwrap();
        assert_eq!(workspace.columns.len(), 2, "two columns became one");
        assert_eq!(workspace.columns[0].kind, ColumnKind::Stack);
        assert_eq!(workspace.columns[0].windows().count(), 2);

        let split = stacked.unstack(2);
        let workspace = split.workspace(1).unwrap();
        assert_eq!(workspace.columns.len(), 3);
        assert_eq!(
            workspace.columns[0].kind,
            ColumnKind::Single,
            "the column it left is single again"
        );
    }

    #[test]
    fn stacking_onto_self_or_a_float_does_not_lose_the_source() {
        for base in [fixture(), fixture().float(1)] {
            for after in [base.stack(2, 2), base.tab(2, 2)] {
                assert_eq!(after, base, "self-target must preserve the tree");
                assert_eq!(after.ops().len(), base.ops().len() + 1);
            }
        }
        let base = fixture().float(1);
        assert_eq!(base.stack(2, 1), base, "a float has no destination column");
        assert_eq!(base.tab(2, 1), base, "a float has no destination column");
    }

    #[test]
    fn stacking_does_not_predict_a_native_space_move() {
        let base = fixture().shift(2, 2);
        assert_eq!(base.stack(2, 1), base);
        assert_eq!(base.tab(2, 1), base);
    }

    #[test]
    fn stacking_into_the_same_column_is_idempotent() {
        let base = fixture().stack(2, 1).stack(3, 1);
        assert_eq!(base.stack(2, 1), base);
    }

    #[test]
    fn unstack_stays_next_to_its_source_even_on_an_inactive_space() {
        let base = fixture().stack(2, 1).view(2);
        let split = base.unstack(2);
        let source = split.workspace(1).unwrap();
        assert!(!source.active);
        assert_eq!(
            source
                .columns
                .iter()
                .map(|col| col.windows().next().unwrap().id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            source.columns[1].width_ratio.to_bits(),
            source.columns[0].width_ratio.to_bits()
        );
        assert!(split.workspace(2).unwrap().columns.is_empty());
    }

    #[test]
    fn unstack_without_an_active_space_preserves_all_windows() {
        let mut base = fixture().stack(2, 1);
        let display = &mut Arc::make_mut(&mut base.displays)[0];
        for workspace in Arc::make_mut(&mut display.workspaces) {
            workspace.active = false;
        }
        let split = base.unstack(2);
        assert_eq!(split.windows().count(), base.windows().count());
        assert!(
            split
                .workspace(1)
                .unwrap()
                .windows()
                .any(|window| window.id == 2)
        );
    }

    #[test]
    fn unstack_single_and_floating_windows_is_a_noop() {
        let base = fixture();
        assert_eq!(base.unstack(1), base);
        let base = base.float(2);
        assert_eq!(base.unstack(2), base);
    }

    #[test]
    fn swap_keeps_record_focus_consistent_with_the_focused_id() {
        let base = fixture().swap(1, 3);
        assert_eq!(base.focused(), Some(1));
        for record in base.windows() {
            assert_eq!(record.focused, base.focused() == Some(record.id));
        }
    }

    #[test]
    fn swap_does_not_predict_native_moves_or_retile_floating_windows() {
        for base in [fixture().float(1), fixture().shift(1, 2)] {
            assert_eq!(base.swap(1, 2), base);
        }
    }

    #[test]
    fn invalid_width_ratios_preserve_the_predicted_columns() {
        let base = fixture();
        for ratio in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(base.width(1, ratio), base);
        }
    }

    #[test]
    fn removing_an_earlier_window_preserves_the_selected_record() {
        let mut base = fixture().tab(2, 1).tab(3, 1);
        let display = &mut Arc::make_mut(&mut base.displays)[0];
        let workspace = &mut Arc::make_mut(&mut display.workspaces)[0];
        Arc::make_mut(&mut workspace.columns)[0].selected = 1;
        let shifted = base.shift(1, 2);
        assert_eq!(
            shifted.workspace(1).unwrap().columns[0].top().unwrap().id,
            2
        );
        assert_eq!(base.workspace(1).unwrap().columns[0].top().unwrap().id, 2);
    }

    fn nested_fixture() -> WindowSet {
        let mut base = fixture();
        let column = ColumnSet::from_items(
            vec![
                StackItemSet::Tabs(Arc::new(vec![
                    base.window(1).unwrap().clone(),
                    base.window(2).unwrap().clone(),
                ])),
                StackItemSet::Single(base.window(3).unwrap().clone()),
            ],
            0.5,
        )
        .unwrap();
        let display = &mut Arc::make_mut(&mut base.displays)[0];
        Arc::make_mut(&mut display.workspaces)[0].columns = Arc::new(vec![column]);
        base
    }

    #[test]
    fn column_items_normalize_empty_and_single_member_groups() {
        assert!(StackItemSet::from_windows(vec![]).is_none());
        assert!(ColumnSet::from_items(vec![StackItemSet::Tabs(Arc::new(vec![]))], 0.5).is_none());
        let column = ColumnSet::from_items(
            vec![
                StackItemSet::Tabs(Arc::new(vec![])),
                StackItemSet::Tabs(Arc::new(vec![window(1, "one")])),
            ],
            0.5,
        )
        .unwrap();
        assert_eq!(column.kind, ColumnKind::Single);
        assert!(matches!(column.items[0], StackItemSet::Single(_)));
        assert_eq!(column.windows().count(), 1);
    }

    #[test]
    fn nested_tab_member_removal_normalizes_only_the_affected_entry() {
        let base = nested_fixture();
        let floated = base.float(1);
        let column = &floated.workspace(1).unwrap().columns[0];
        assert_eq!(column.kind, ColumnKind::Stack);
        assert!(matches!(column.items[0], StackItemSet::Single(_)));
        assert_eq!(
            column.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
        let floated = floated.float(2);
        assert_eq!(
            floated.workspace(1).unwrap().columns[0].kind,
            ColumnKind::Single
        );
        assert_eq!(
            base.workspace(1).unwrap().columns[0].items[0]
                .windows()
                .len(),
            2
        );
    }

    #[test]
    fn unequal_item_swaps_preserve_the_selected_unmoved_entry() {
        let mut base = nested_fixture();
        let display = &mut Arc::make_mut(&mut base.displays)[0];
        let columns = Arc::make_mut(&mut Arc::make_mut(&mut display.workspaces)[0].columns);
        columns[0].selected = 2;
        columns.push(ColumnSet::single(window(4, "four"), 0.75));
        let swapped = base.swap(1, 4);
        let columns = &swapped.workspace(1).unwrap().columns;
        assert_eq!(columns[0].selected, 1);
        assert_eq!(columns[0].top().unwrap().id, 3);
        assert_eq!(columns[0].width_ratio.to_bits(), 0.5_f64.to_bits());
        assert_eq!(columns[1].kind, ColumnKind::Tabs);
        assert_eq!(columns[1].width_ratio.to_bits(), 0.75_f64.to_bits());
        assert_eq!(
            columns[1]
                .windows()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn nested_tab_branches_share_records_until_they_change() {
        let base = nested_fixture();
        let split = base.unstack(2);
        let StackItemSet::Tabs(original) = &base.workspace(1).unwrap().columns[0].items[0] else {
            panic!("native tabs");
        };
        let StackItemSet::Tabs(moved) = &split.workspace(1).unwrap().columns[1].items[0] else {
            panic!("native tabs");
        };
        assert!(Arc::ptr_eq(original, moved));
        let focused = split.focus(2);
        assert!(focused.window(2).unwrap().focused);
        assert!(!base.window(2).unwrap().focused);
        assert!(!split.window(2).unwrap().focused);
    }

    #[test]
    fn nested_tab_entries_survive_binary_and_json_snapshots() {
        let base = nested_fixture();
        let bytes = postcard::to_allocvec(&base).unwrap();
        let binary: WindowSet = postcard::from_bytes(&bytes).unwrap();
        let json: WindowSet = serde_json::from_str(&serde_json::to_string(&base).unwrap()).unwrap();
        for decoded in [binary, json] {
            assert_eq!(decoded, base);
            assert_eq!(decoded.unstack(2), base.unstack(2));
            assert_eq!(decoded.swap(1, 3), base.swap(1, 3));
        }
    }

    #[test]
    fn relative_rect_saturates_global_coordinate_overflow() {
        for (origin, offset, expected) in [
            (100, f64::MAX / 2.0, i32::MAX),
            (-100, -f64::MAX / 2.0, i32::MIN),
        ] {
            let placed = RelativeRect {
                x: offset,
                y: offset,
                width: 1.0,
                height: 1.0,
            }
            .resolve(Frame {
                x: origin,
                y: origin,
                width: 1,
                height: 1,
            });
            assert_eq!(placed.x, expected);
            assert_eq!(placed.y, expected);
        }
    }

    #[test]
    fn floating_moves_a_window_off_the_strip_and_back() {
        let floated = fixture().float(2);
        let workspace = floated.workspace(1).unwrap();
        assert_eq!(workspace.columns.len(), 2);
        assert_eq!(workspace.floating.len(), 1);
        assert!(workspace.floating[0].floating);

        let sunk = floated.sink(2);
        let workspace = sunk.workspace(1).unwrap();
        assert_eq!(workspace.columns.len(), 3);
        assert!(workspace.floating.is_empty());
    }

    #[test]
    fn floating_on_an_inactive_space_keeps_its_membership() {
        let base = fixture().shift(2, 2);
        let floated = base.float(2);
        assert_eq!(floated.workspace_of(2).unwrap().space_id, 2);
        assert_eq!(floated.current().unwrap().space_id, 1);
        assert_eq!(floated.focused(), base.focused());
        let sunk = floated.sink(2);
        assert_eq!(sunk.workspace_of(2).unwrap().space_id, 2);
        assert_eq!(sunk.windows().count(), base.windows().count());
    }

    #[test]
    fn floating_without_an_active_space_preserves_every_record() {
        let mut base = fixture();
        let display = &mut Arc::make_mut(&mut base.displays)[0];
        display.active = false;
        for workspace in Arc::make_mut(&mut display.workspaces) {
            workspace.active = false;
        }
        let floated = base.float(2);
        assert_eq!(floated.windows().count(), base.windows().count());
        assert_eq!(floated.workspace_of(2).unwrap().space_id, 1);
        assert_eq!(floated.sink(2).windows().count(), base.windows().count());
    }

    #[test]
    fn repeated_mode_requests_preserve_predicted_order_and_column_shape() {
        let tiled = fixture().stack(2, 1).width(1, 0.75);
        assert_eq!(tiled.sink(2), tiled);
        let floated = fixture().float(2).float(3);
        assert_eq!(floated.float(2), floated);
    }

    #[test]
    fn floating_and_sinking_use_the_source_display_not_the_first_active_space() {
        let mut base = fixture().shift(2, 2);
        let displays = Arc::make_mut(&mut base.displays);
        let mut second = displays[0].clone();
        second.id = 2;
        second.active = false;
        let mut workspace = Arc::make_mut(&mut displays[0].workspaces).remove(1);
        workspace.active = true;
        second.workspaces = Arc::new(vec![workspace]);
        displays.push(second);
        let floated = base.float(2);
        assert_eq!(floated.display_of(2).unwrap().id, 2);
        assert_eq!(floated.sink(2).display_of(2).unwrap().id, 2);
        assert_eq!(
            floated.displays()[0].workspaces,
            base.displays()[0].workspaces
        );
    }

    #[test]
    fn shifting_a_float_keeps_it_out_of_tiled_columns() {
        let shifted = fixture().float(2).shift(2, 2);
        let target = shifted.workspace(2).unwrap();
        assert!(target.columns.is_empty());
        assert_eq!(target.floating.len(), 1);
        assert_eq!(target.floating[0].id, 2);
        assert!(target.floating[0].floating);
    }

    #[test]
    fn shifting_to_the_same_space_preserves_order_and_column_width() {
        let base = fixture().stack(2, 1).width(1, 0.75);
        assert_eq!(base.shift(2, 1), base);
        let floated = fixture().float(2).float(3);
        assert_eq!(floated.shift(2, 1), floated);
    }

    #[test]
    fn float_at_resolves_fractions_against_the_display() {
        // The fixture's display is 1920x1080 at the origin.
        let set = fixture().float_at(
            2,
            RelativeRect {
                x: 0.25,
                y: 0.5,
                width: 0.5,
                height: 0.25,
            },
        );
        let placed = Frame {
            x: 480,
            y: 540,
            width: 960,
            height: 270,
        };
        assert_eq!(
            set.ops(),
            vec![
                LayoutOp::SetFloating {
                    window: 2,
                    floating: true
                },
                LayoutOp::SetFrame {
                    window: 2,
                    frame: placed
                },
            ],
            "customFloating is float-then-place"
        );
        let window = set.window(2).expect("the window is still there");
        assert_eq!(window.frame, Some(placed));
        assert!(window.floating);
    }

    #[test]
    fn a_relative_rect_is_offset_by_the_display_origin() {
        let second = Frame {
            x: 1920,
            y: -200,
            width: 1000,
            height: 800,
        };
        let full = RelativeRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert_eq!(full.resolve(second), second, "a full rect is the display");

        let half = RelativeRect {
            x: 0.5,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        };
        assert_eq!(
            half.resolve(second),
            Frame {
                x: 2420,
                y: -200,
                width: 500,
                height: 800
            }
        );
    }

    #[test]
    fn a_degenerate_rect_still_yields_a_usable_frame() {
        // Scripts can pass anything; a zero-size or non-finite rect must not
        // produce a window that cannot be seen or a panicking cast.
        let display = Frame {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let zero = RelativeRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        let resolved = zero.resolve(display);
        assert_eq!((resolved.width, resolved.height), (1, 1));

        let nonsense = RelativeRect {
            x: f64::NAN,
            y: f64::INFINITY,
            width: 1e30,
            height: -1.0,
        };
        let resolved = nonsense.resolve(display);
        assert!(resolved.width >= 1 && resolved.height >= 1);
    }

    #[test]
    fn view_switches_which_workspace_is_active() {
        let set = fixture().view(2);
        assert!(!set.workspace(1).unwrap().active);
        assert!(set.workspace(2).unwrap().active);
        assert_eq!(set.ops(), vec![LayoutOp::View { space_id: 2 }]);
    }

    #[test]
    fn navigation_follows_the_strip_and_wraps() {
        let set = fixture();
        assert_eq!(set.east(1), Some(2));
        assert_eq!(set.west(2), Some(1));
        assert_eq!(set.west(1), None, "nothing west of the first column");
        assert_eq!(set.next(3), Some(1), "next wraps around");
        assert_eq!(set.prev(1), Some(3), "and so does prev");
    }

    #[test]
    fn lookups_find_where_a_window_lives() {
        let set = fixture();
        assert_eq!(set.column_of(2), Some(1));
        assert_eq!(set.workspace_of(2).map(|w| w.space_id), Some(1));
        assert_eq!(set.display_of(2).map(|d| d.id), Some(1));
        assert_eq!(set.current().map(|w| w.space_id), Some(1));
        assert_eq!(set.window(2).map(|w| w.app_name.as_str()), Some("beta"));
        assert_eq!(set.window(99), None);
    }

    #[test]
    fn ops_naming_a_window_expose_it_for_resolution() {
        assert_eq!(LayoutOp::Focus(4).target(), Some(4));
        assert_eq!(
            LayoutOp::Stack {
                window: 4,
                onto: 5,
                tabs: true
            }
            .target(),
            Some(4)
        );
        assert_eq!(LayoutOp::View { space_id: 2 }.target(), None);
    }

    #[test]
    fn shifting_to_a_workspace_the_snapshot_lacks_keeps_the_window() {
        let set = fixture().shift(2, 42);
        assert!(
            set.window(2).is_some(),
            "the window should still be somewhere"
        );
        assert_eq!(set.workspace_of(2).map(|w| w.space_id), Some(1));
        assert_eq!(
            set.ops(),
            vec![LayoutOp::MoveToWorkspace {
                window: 2,
                space_id: 42,
                follow: false
            }]
        );
    }

    #[test]
    fn stacking_onto_a_window_the_snapshot_lacks_keeps_the_window() {
        let set = fixture().stack(2, 99);
        assert!(
            set.window(2).is_some(),
            "the window should still be somewhere"
        );
        assert_eq!(set.workspace(1).unwrap().columns.len(), 3);
    }

    #[test]
    fn transforming_a_missing_window_still_records_the_intent() {
        let set = fixture().focus(99);
        assert_eq!(set.ops(), vec![LayoutOp::Focus(99)]);
    }
}

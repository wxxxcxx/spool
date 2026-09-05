use std::collections::HashMap;

use spool_shared_types::state::{QueryState, SpaceKind};
use spool_shared_types::windowset::{ColumnKind, WindowSet};

#[derive(Clone, Debug, PartialEq)]
pub struct BarSnapshot {
    pub can_focus_spaces: bool,
    pub can_move_windows: bool,
    pub displays: Vec<BarDisplay>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarDisplay {
    pub id: u32,
    pub frame: spool_shared_types::state::Frame,
    pub active: bool,
    pub spaces: Vec<BarSpace>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarSpace {
    pub id: u64,
    pub ordinal: u32,
    pub kind: SpaceKind,
    pub visible: bool,
    pub focused: bool,
    pub columns: Vec<BarColumn>,
    pub floating: Vec<BarWindow>,
}

impl BarSpace {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floating.is_empty()
    }

    pub fn windows(&self) -> impl Iterator<Item = &BarWindow> {
        self.columns
            .iter()
            .flat_map(|column| column.windows.iter())
            .chain(self.floating.iter())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarColumn {
    pub kind: ColumnKind,
    pub selected: usize,
    pub windows: Vec<BarWindow>,
}

impl BarColumn {
    #[must_use]
    pub fn anchor_window_id(&self) -> Option<i32> {
        self.windows
            .get(self.selected)
            .or_else(|| self.windows.first())
            .map(|window| window.id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarWindow {
    pub id: i32,
    pub bundle_id: String,
    pub app_name: String,
    pub title: String,
    pub focused: bool,
    pub visible: bool,
}

impl BarSnapshot {
    #[must_use]
    pub fn from_state(state: &QueryState, layout: &WindowSet) -> Self {
        let spaces = state
            .spaces
            .iter()
            .map(|space| (space.space_id, space))
            .collect::<HashMap<_, _>>();

        let mut displays = layout
            .displays()
            .iter()
            .map(|display| {
                let mut display_spaces = display
                    .workspaces
                    .iter()
                    .map(|workspace| {
                        let state_space = spaces.get(&workspace.space_id).copied();
                        let columns = workspace
                            .columns
                            .iter()
                            .map(|column| BarColumn {
                                kind: column.kind,
                                selected: column.selected,
                                windows: column.windows.iter().map(BarWindow::from).collect(),
                            })
                            .collect();
                        BarSpace {
                            id: workspace.space_id,
                            ordinal: workspace.ordinal,
                            kind: state_space.map_or(SpaceKind::User, |space| space.kind),
                            visible: state_space.is_some_and(|space| space.visible),
                            focused: state_space.is_some_and(|space| space.focused),
                            columns,
                            floating: workspace.floating.iter().map(BarWindow::from).collect(),
                        }
                    })
                    .collect::<Vec<_>>();
                display_spaces.sort_by_key(|space| space.ordinal);
                BarDisplay {
                    id: display.id,
                    frame: display.frame,
                    active: display.active,
                    spaces: display_spaces,
                }
            })
            .collect::<Vec<_>>();
        displays.sort_by_key(|display| (display.frame.x, display.frame.y));

        Self {
            can_focus_spaces: state.capabilities.focus,
            can_move_windows: state.capabilities.move_windows,
            displays,
        }
    }
}

impl From<&spool_shared_types::windowset::WindowRec> for BarWindow {
    fn from(window: &spool_shared_types::windowset::WindowRec) -> Self {
        Self {
            id: window.id,
            bundle_id: window.bundle_id.clone(),
            app_name: window.app_name.clone(),
            title: window.title.clone(),
            focused: window.focused,
            visible: window.visible,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spool_shared_types::state::{
        ActiveState, DisplayState, QueryState, SpaceCapabilities, SpaceState,
    };
    use spool_shared_types::windowset::{
        ColumnKind, ColumnSet, DisplaySet, WindowRec, WorkspaceSet,
    };

    use super::*;

    fn window(id: i32, focused: bool) -> WindowRec {
        WindowRec {
            id,
            app_name: format!("App {id}"),
            bundle_id: format!("com.example.{id}"),
            title: format!("Window {id}"),
            frame: None,
            floating: false,
            visible: true,
            focused,
        }
    }

    #[test]
    fn snapshot_combines_native_space_state_with_structured_columns() {
        let layout = WindowSet::new(
            vec![DisplaySet {
                id: 7,
                frame: spool_shared_types::state::Frame {
                    x: 0,
                    y: 0,
                    width: 1440,
                    height: 900,
                },
                active: true,
                workspaces: Arc::new(vec![WorkspaceSet {
                    space_id: 42,
                    ordinal: 1,
                    active: true,
                    columns: Arc::new(vec![ColumnSet {
                        kind: ColumnKind::Stack,
                        width_ratio: 0.5,
                        selected: 1,
                        windows: Arc::new(vec![window(10, false), window(11, true)]),
                    }]),
                    floating: Arc::new(Vec::new()),
                }]),
            }],
            Some(11),
        );
        let state = QueryState {
            version: 3,
            timestamp: 0,
            active: ActiveState {
                display_id: Some(7),
                space_id: Some(42),
                focused_window_id: Some(11),
                focused_bundle_id: None,
                focused_app_name: None,
                focused_window_title: None,
            },
            capabilities: SpaceCapabilities {
                move_windows: true,
                focus: true,
                create: false,
                delete: false,
            },
            displays: vec![DisplayState {
                display_id: 7,
                active: true,
                visible_space_id: Some(42),
            }],
            spaces: vec![SpaceState {
                space_id: 42,
                display_id: 7,
                ordinal: 1,
                kind: SpaceKind::Fullscreen,
                visible: true,
                focused: true,
                windows: Vec::new(),
            }],
        };

        let snapshot = BarSnapshot::from_state(&state, &layout);
        let space = &snapshot.displays[0].spaces[0];
        assert_eq!(space.kind, SpaceKind::Fullscreen);
        assert!(space.visible);
        assert_eq!(
            space.columns[0]
                .windows
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert_eq!(space.columns[0].anchor_window_id(), Some(11));
    }
}

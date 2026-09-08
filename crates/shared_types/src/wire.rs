//! What the daemon and its clients say to each other.
//!
//! Every request is a [`Request`] and every answer a [`Response`]; these two
//! enums are the whole protocol and the wire encoding is generated from them.
//!
//! Values travel as postcard — compact, binary, and not self-describing, which
//! is why [`crate::script_value::ScriptValue`] exists in place of
//! `serde_json::Value`. JSON is still what `spool query` prints to a terminal,
//! but it is not what the two processes speak to each other.

use serde::{Deserialize, Serialize};

/// The logical name used to derive the daemon's Unix socket and lock paths.
pub const SERVICE_NAME: &str = "com.wxxxcxx.spool";

/// The one production instance name. It is deliberately not configurable:
/// launchd and foreground launches must contend for the same singleton lock.
#[must_use]
pub fn service_name() -> String {
    SERVICE_NAME.to_string()
}

use crate::commands::Action;
pub use crate::script_state::WriteOutcome;

use crate::script_state::ScriptStateWrite;
use crate::script_value::ScriptValue;
use crate::state::{ActiveState, QueryState, SpaceState, StateQueryKind, WindowState};
use crate::windowset::{LayoutPlan, WindowSet};

/// Something a client asks the daemon to do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Dispatch an action — the same one a hotkey binds to. Fire-and-forget: the
    /// daemon applies it best-effort against the live world and a client that
    /// wants the result queries for it.
    Dispatch(Action),
    /// Read part of the state document.
    Query(StateQueryKind),
    /// Read the window set — the same layout tree a `spool.windows` handler is
    /// given inside the daemon, so a client script transforms an identical tree.
    WindowSet,
    /// Replay a transform's recorded operations against the live world.
    /// Fire-and-forget, for the same reason [`Request::Dispatch`] is.
    WindowSetApply(LayoutPlan),
    /// Read or write the script-state store.
    ScriptState(ScriptStateRequest),
    /// Ask for state events to be pushed as they happen. `raw` adds the
    /// uncoalesced source events used to derive stable notifications.
    Subscribe { raw: bool },
}

/// What a client wants of the script-state store.
///
/// Lives here rather than in `script_state` because it is a *protocol* shape —
/// the store itself has no notion of a request, only of a write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScriptStateRequest {
    Get { key: String },
    Write(ScriptStateWrite),
}

/// What the daemon says back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Query(QueryPayload),
    WindowSet(Box<WindowSet>),
    ScriptState(ScriptStateResponse),
    /// The request could not be answered. Carries the message a client should
    /// show.
    Error(String),
}

/// The answer to a [`Request::Query`], one variant per [`StateQueryKind`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum QueryPayload {
    State(Box<QueryState>),
    Spaces(Vec<SpaceState>),
    Active(Box<ActiveState>),
    OnScreen(Vec<WindowState>),
}

impl QueryPayload {
    /// Renders as JSON, for the CLI and for a Lua client that wants a table.
    ///
    /// # Errors
    ///
    /// If serialization fails, which should not happen barring a bug in one of
    /// these types' `Serialize` impls.
    pub fn to_json(&self) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::State(state) => serde_json::to_value(state),
            Self::Spaces(spaces) => serde_json::to_value(spaces),
            Self::Active(active) => serde_json::to_value(active),
            Self::OnScreen(windows) => serde_json::to_value(windows),
        }
    }
}

/// The answer to a [`ScriptStateRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScriptStateResponse {
    /// What the key holds, `None` when it holds nothing.
    Value(Option<ScriptValue>),
    /// What became of a write.
    Write(WriteOutcome),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Action, Direction, Operation};
    use crate::state::Frame;
    use std::sync::Arc;

    fn round_trip<T>(value: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = postcard::to_allocvec(value).expect("encodes");
        let decoded: T = postcard::from_bytes(&bytes).expect("decodes");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn every_request_survives_the_wire() {
        round_trip(&Request::Dispatch(Action::Window(Operation::Focus(
            Direction::East,
        ))));
        round_trip(&Request::Query(StateQueryKind::Active));
        round_trip(&Request::WindowSet);
        round_trip(&Request::WindowSetApply(
            WindowSet::default().focus(7).plan(),
        ));
        round_trip(&Request::Subscribe { raw: false });
        round_trip(&Request::Subscribe { raw: true });
        round_trip(&Request::ScriptState(ScriptStateRequest::Get {
            key: "pads.term".to_string(),
        }));
        round_trip(&Request::ScriptState(ScriptStateRequest::Write(
            ScriptStateWrite::set("count".to_string(), ScriptValue::Int(7)),
        )));
    }

    #[test]
    fn snapshot_bindings_survive_both_postcard_round_trips() {
        use crate::windowset::{LayoutSnapshot, WindowIdentity};
        let identity = LayoutSnapshot {
            session: [73; 16],
            windows: [(
                7,
                WindowIdentity {
                    entity: 41,
                    incarnation: 99,
                },
            )]
            .into(),
        };
        let set = WindowSet::default().with_snapshot(identity.clone());
        let bytes = postcard::to_allocvec(&Response::WindowSet(Box::new(set))).unwrap();
        let Response::WindowSet(set) = postcard::from_bytes(&bytes).unwrap() else {
            panic!("window set response")
        };
        let plan = set.float(7).shift(7, 2).plan();
        assert_eq!(*plan.snapshot, identity);
        round_trip(&Request::WindowSetApply(plan.clone()));
        round_trip(&Request::Dispatch(Action::Layout(plan)));
    }

    #[test]
    fn overview_actions_append_wire_tags_without_renumbering_existing_actions() {
        use crate::commands::{MoveFocus, Placement};

        for (action, tag) in [
            (Action::Lua(7), 11),
            (Action::Layout(LayoutPlan::default()), 12),
            (
                Action::ReorderColumn {
                    window_id: 1,
                    anchor_window_id: 2,
                    placement: Placement::After,
                },
                13,
            ),
            (
                Action::MoveColumnToSpace {
                    window_id: 1,
                    space_id: 2,
                    move_focus: MoveFocus::Stay,
                },
                14,
            ),
            (Action::MissionControl, 15),
            (Action::ShowDesktop, 16),
        ] {
            let request = Request::Dispatch(action);
            let bytes = postcard::to_allocvec(&request).unwrap();
            assert_eq!(&bytes[..2], &[0, tag]);
            round_trip(&request);
        }
    }

    #[test]
    fn every_response_survives_the_wire() {
        round_trip(&Response::Query(QueryPayload::Active(Box::default())));
        round_trip(&Response::Query(QueryPayload::Spaces(Vec::new())));
        round_trip(&Response::Query(QueryPayload::OnScreen(Vec::new())));
        round_trip(&Response::ScriptState(ScriptStateResponse::Value(Some(
            ScriptValue::Str("hello".to_string()),
        ))));
        round_trip(&Response::ScriptState(ScriptStateResponse::Write(
            WriteOutcome::Applied { changed: true },
        )));
        round_trip(&Response::Error("no such window".to_string()));
    }

    /// The layout tree is the largest thing that crosses the wire, and the one
    /// a client actually transforms, so it gets its own round trip.
    #[test]
    fn the_window_set_survives_the_wire() {
        use crate::windowset::{ColumnSet, DisplaySet, StackItemSet, WindowRec, WorkspaceSet};

        let window = |id| WindowRec {
            id,
            app_name: "Test App".to_string(),
            bundle_id: "com.example.test".to_string(),
            title: format!("Window {id}"),
            frame: Some(Frame {
                x: 0,
                y: 0,
                width: 400,
                height: 600,
            }),
            floating: false,
            visible: true,
            focused: id == 1,
        };
        let set = WindowSet::new(
            vec![DisplaySet {
                id: 1,
                frame: Frame {
                    x: 0,
                    y: 0,
                    width: 1024,
                    height: 768,
                },
                active: true,
                workspaces: Arc::new(vec![WorkspaceSet {
                    space_id: 10,
                    ordinal: 0,
                    active: true,
                    columns: Arc::new(vec![
                        ColumnSet::single(window(1), 0.5),
                        ColumnSet::single(window(2), 0.5),
                        ColumnSet::from_items(
                            vec![
                                StackItemSet::Single(window(3)),
                                StackItemSet::Tabs(Arc::new(vec![window(4), window(5)])),
                            ],
                            0.5,
                        )
                        .expect("nested tab column"),
                    ]),
                    floating: Arc::new(Vec::new()),
                }]),
            }],
            Some(1),
        );

        let bytes =
            postcard::to_allocvec(&Response::WindowSet(Box::new(set.clone()))).expect("encodes");
        let Response::WindowSet(decoded) = postcard::from_bytes(&bytes).expect("decodes") else {
            panic!("expected a window set");
        };

        assert_eq!(decoded.focused(), Some(1));
        assert_eq!(decoded.east(1), Some(2));
        assert_eq!(*decoded, set);
        assert_eq!(decoded.unstack(5), set.unstack(5));
        // Ops are deliberately not carried: a set off the wire is one nothing
        // has been asked of yet.
        assert!(decoded.ops().is_empty());
    }

    #[test]
    fn a_request_is_small() {
        let bytes =
            postcard::to_allocvec(&Request::Query(StateQueryKind::Active)).expect("encodes");
        assert!(
            bytes.len() <= 4,
            "a query request took {} bytes",
            bytes.len()
        );
    }
}

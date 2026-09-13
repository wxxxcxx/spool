//! What the daemon and its clients say to each other.
//!
//! Every request is a [`Request`] and every answer a [`Response`]; these two
//! enums are the whole protocol and the wire encoding is generated from them.
//!
//! Values travel as postcard — compact, binary, and not self-describing, which
//! is why [`crate::script_value::ScriptValue`] exists in place of
//! `serde_json::Value`. JSON is still what resource reads print to a terminal,
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
    Dispatch(#[serde(with = "named_action")] Action),
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
    /// Ordered execution admission, distinct from transport acknowledgement.
    Command(CheckedAction),
    /// Pure projection of retained daemon state; native collection is local.
    Inspect(crate::inspection::ReadRequest),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckedAction {
    pub request_id: String,
    #[serde(with = "named_action")]
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionStatus {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionReceipt {
    pub request_id: String,
    pub status: AdmissionStatus,
    pub code: Option<String>,
    pub message: Option<String>,
}

impl AdmissionReceipt {
    #[must_use]
    pub fn from_result(request_id: String, result: Result<(), String>) -> Self {
        match result {
            Ok(()) => Self {
                request_id,
                status: AdmissionStatus::Accepted,
                code: None,
                message: None,
            },
            Err(reason) => Self {
                request_id,
                status: AdmissionStatus::Rejected,
                code: Some(reason.clone()),
                message: Some(reason.replace('_', " ")),
            },
        }
    }
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

/// Actions carry their serde names, including nested operations, rather than
/// postcard declaration-order discriminants. Only this payload uses JSON; the
/// enclosing request and replies remain postcard. Keep human-readable serde
/// unchanged for tools that already inspect requests as JSON.
mod named_action {
    use super::Action;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(action: &Action, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return action.serialize(serializer);
        }
        let text = serde_json::to_string(action).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Action, D::Error> {
        if deserializer.is_human_readable() {
            return Action::deserialize(deserializer);
        }
        let text = String::deserialize(deserializer)?;
        serde_json::from_str(&text).map_err(serde::de::Error::custom)
    }
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
    Admission(AdmissionReceipt),
    Inspection(Box<crate::inspection::Report>),
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
            structures: std::collections::BTreeMap::new(),
            columns: [(7, (101, 4))].into(),
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
    fn actions_use_stable_names_instead_of_enum_positions() {
        for (action, name) in [
            (Action::Quit, "\"quit\""),
            (Action::Restart, "\"restart\""),
            (Action::ToggleBarCollapse, "\"toggle_bar_collapse\""),
            (
                Action::FocusWindowInSpace {
                    window_id: 7,
                    space_id: 9,
                },
                "{\"focus_window_in_space\":{\"window_id\":7,\"space_id\":9}}",
            ),
        ] {
            // Independent fixture: Dispatch's envelope tag plus a string,
            // with no Action serialization involved in constructing the bytes.
            let fixture = postcard::to_allocvec(&(0u8, name)).unwrap();
            let request = Request::Dispatch(action);
            assert_eq!(postcard::to_allocvec(&request).unwrap(), fixture);
            assert_eq!(postcard::from_bytes::<Request>(&fixture).unwrap(), request);
        }
        let unknown = postcard::to_allocvec(&(0u8, "\"future_action\"")).unwrap();
        assert!(postcard::from_bytes::<Request>(&unknown).is_err());
        // A v4 Restart must never become a v5 Quit (or any other action).
        assert!(postcard::from_bytes::<Request>(&[0, 8]).is_err());
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

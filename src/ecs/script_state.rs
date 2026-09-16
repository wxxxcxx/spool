//! The script-owned key-value store, as the daemon holds it.
//!
//! [`ScriptState`] is the data; this is where it lives while Spool runs and how
//! it gets to disk. Both the embedded Lua runtime and a socket client write
//! through this single-authority resource; the Lua worker caches a copy and
//! checks [`ScriptStateStore::revision_handle`] to know when to re-read.
//!
//! Kept separate from retained layout intent (which lives only in the running
//! world and is rebuilt from rules and observations at each start, ADR 0010):
//! a script's own store is data the script owns, so it is written to disk and
//! read back.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::app::AppExit;
use bevy::ecs::message::MessageReader;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::ResMut;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use crate::events::Event;
use spool_shared_types::script_state::{ScriptState, ScriptStateWrite, WriteOutcome};
use spool_shared_types::wire::{Response, ScriptStateRequest, ScriptStateResponse};

pub const SCRIPT_STATE_FILE_NAME: &str = "script-state.json";
const SUPPORTED_SCRIPT_STATE_VERSION: u32 = 1;

/// The on-disk shape. Versioned separately from the layout state file, since
/// the two have nothing to say to each other.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SavedScriptState {
    version: u32,
    state: ScriptState,
}

/// The live store.
#[derive(Debug, Default, Resource)]
pub struct ScriptStateStore {
    state: ScriptState,
    /// Bumped on every applied mutation. The Lua worker compares this against
    /// the stamp its cached copy was hydrated at to know when to re-read.
    revision: Arc<AtomicU64>,
    dirty: bool,
}

impl ScriptStateStore {
    /// The store as saved by a previous run, or an empty one if there is no
    /// file, it cannot be read, or it was written by an incompatible version.
    #[must_use]
    pub fn load() -> Self {
        let path = Self::default_file_path();
        let Some(saved) = Self::read_file(&path) else {
            return Self::default();
        };
        debug!("Loaded script state from {}", path.display());
        Self {
            state: saved,
            ..Self::default()
        }
    }

    fn read_file(path: &Path) -> Option<ScriptState> {
        let data = fs::read_to_string(path).ok()?;
        match serde_json::from_str::<SavedScriptState>(&data) {
            Ok(saved) if saved.version == SUPPORTED_SCRIPT_STATE_VERSION => Some(saved.state),
            Ok(saved) => {
                warn!(
                    "Ignoring script state at {}: version {}, expected {SUPPORTED_SCRIPT_STATE_VERSION}",
                    path.display(),
                    saved.version
                );
                None
            }
            Err(err) => {
                warn!(
                    "Ignoring unreadable script state at {}: {err}",
                    path.display()
                );
                None
            }
        }
    }

    /// A handle on the revision stamp, for the Lua worker to watch.
    ///
    /// Only the worker wants it, so without the `lua` feature there is no
    /// caller — which is not the same as the method being dead.
    #[cfg_attr(not(feature = "lua"), allow(dead_code))]
    #[must_use]
    pub fn revision_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.revision)
    }

    /// The whole store, for answering a read.
    ///
    /// As with [`Self::revision_handle`], the only caller is behind the `lua`
    /// feature.
    #[cfg_attr(not(feature = "lua"), allow(dead_code))]
    #[must_use]
    pub fn snapshot(&self) -> ScriptState {
        self.state.clone()
    }

    /// Read-only access, for a caller that wants one key rather than a copy of
    /// everything.
    #[must_use]
    pub fn state(&self) -> &ScriptState {
        &self.state
    }

    /// Applies `write`, bumping the revision and marking the store dirty only
    /// if it actually changed something. This is the one place the store is
    /// written, whether the caller is a Lua handler or a socket client, which
    /// is what makes a compare-and-set write race-free.
    ///
    /// # Errors
    ///
    /// If the key is unacceptable, a value cannot be stored as JSON, or the
    /// write would exceed the size or depth limit. A write that merely lost a
    /// race is not an error — it comes back as [`WriteOutcome::Conflict`].
    pub fn apply(&mut self, write: &ScriptStateWrite) -> Result<WriteOutcome, String> {
        let outcome = self.state.apply(write)?;
        if matches!(outcome, WriteOutcome::Applied { changed: true }) {
            self.revision.fetch_add(1, Ordering::Release);
            self.dirty = true;
        }
        Ok(outcome)
    }

    /// Writes the store out if anything has changed since the last save. Same
    /// write-to-temp-then-rename as the layout state file, so a crash mid-save
    /// leaves the previous file intact rather than a truncated one.
    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let path = Self::default_file_path();
        match self.write_file(&path) {
            Ok(()) => {
                self.dirty = false;
                debug!("Script state saved to {}", path.display());
            }
            Err(err) => error!("Failed to save script state to {}: {err}", path.display()),
        }
    }

    fn write_file(&self, path: &Path) -> Result<(), std::io::Error> {
        let saved = SavedScriptState {
            version: SUPPORTED_SCRIPT_STATE_VERSION,
            state: self.state.clone(),
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
            .get_state_file(SCRIPT_STATE_FILE_NAME)
            .expect("XDG state directory should be available")
    }
}

/// Answers the script-state requests a socket client made — the client half
/// of `spool.state`, backed by the same store a script writes through.
pub fn script_state_handler(
    mut messages: MessageReader<Event>,
    store: Option<ResMut<ScriptStateStore>>,
) {
    let mut store = store;
    for message in messages.read() {
        let Event::ScriptState {
            request,
            respond_to,
        } = message
        else {
            continue;
        };
        let Some(store) = store.as_mut() else {
            let _ = respond_to.try_send(error_reply("the script state store is not available"));
            continue;
        };
        // `try_send`, never `send`: the reply channel holds one message and
        // exactly one is sent, so this cannot fill, and the main thread must
        // never wait on a socket client to collect its answer.
        let _ = respond_to.try_send(answer(store, request.clone()));
    }
}

fn answer(store: &mut ScriptStateStore, request: ScriptStateRequest) -> Response {
    match request {
        ScriptStateRequest::Get { key } => {
            Response::ScriptState(ScriptStateResponse::Value(store.state().get(&key).cloned()))
        }
        // A conflict is not an error: the caller retries against the current
        // value, so it travels as an outcome rather than a failure.
        ScriptStateRequest::Write(write) => match store.apply(&write) {
            Ok(outcome) => Response::ScriptState(ScriptStateResponse::Write(outcome)),
            Err(err) => Response::Error(err),
        },
    }
}

fn error_reply(message: &str) -> Response {
    Response::Error(message.to_string())
}

/// Saves the store on the same timer as the layout state, and costs nothing on
/// a run where no script ever wrote to it.
pub fn periodic_script_state_save(store: Option<ResMut<ScriptStateStore>>) {
    if let Some(mut store) = store {
        store.save_if_dirty();
    }
}

/// Saves the store on the way out, so the last write of a session is not the
/// one that gets lost.
pub fn script_state_cleanup_on_exit(
    mut exit_events: MessageReader<AppExit>,
    store: Option<ResMut<ScriptStateStore>>,
) {
    if exit_events.read().next().is_some()
        && let Some(mut store) = store
    {
        store.save_if_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spool_shared_types::script_value::ScriptValue;

    fn set(key: &str, value: serde_json::Value) -> ScriptStateWrite {
        ScriptStateWrite::set(key.to_string(), ScriptValue::from(value))
    }

    fn applied(store: &mut ScriptStateStore, write: &ScriptStateWrite) -> bool {
        match store.apply(write).expect("accepted") {
            WriteOutcome::Applied { changed } => changed,
            WriteOutcome::Conflict { .. } => panic!("unexpected conflict"),
        }
    }

    #[test]
    fn apply_bumps_the_revision_only_on_a_real_change() {
        let mut store = ScriptStateStore::default();
        let revision = store.revision_handle();

        assert!(applied(&mut store, &set("a", json!(1))));
        assert_eq!(revision.load(Ordering::Acquire), 1);

        assert!(!applied(&mut store, &set("a", json!(1))));
        assert_eq!(revision.load(Ordering::Acquire), 1);

        assert!(applied(&mut store, &set("a", json!(2))));
        assert_eq!(revision.load(Ordering::Acquire), 2);
    }

    #[test]
    fn remove_takes_a_key_out() {
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("pad.a", json!(1)));
        applied(&mut store, &set("pad.b", json!(2)));

        let removed = ScriptStateWrite::remove("pad.a".to_string());
        assert!(applied(&mut store, &removed));
        assert!(store.state().get("pad.a").is_none());
        assert_eq!(
            store.state().get("pad.b"),
            Some(&ScriptValue::from(json!(2)))
        );

        assert!(!applied(&mut store, &removed));
    }

    #[test]
    fn a_compare_and_set_lands_only_against_the_value_it_read() {
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(1)));

        let stale = ScriptStateWrite::compare_and_set(
            "counter".to_string(),
            Some(ScriptValue::from(json!(0))),
            Some(ScriptValue::from(json!(1))),
        );
        assert_eq!(
            store.apply(&stale).expect("accepted"),
            WriteOutcome::Conflict {
                current: Some(ScriptValue::from(json!(1)))
            }
        );

        let fresh = ScriptStateWrite::compare_and_set(
            "counter".to_string(),
            Some(ScriptValue::from(json!(1))),
            Some(ScriptValue::from(json!(2))),
        );
        assert_eq!(
            store.apply(&fresh).expect("accepted"),
            WriteOutcome::Applied { changed: true }
        );
        assert_eq!(
            store.state().get("counter"),
            Some(&ScriptValue::from(json!(2)))
        );
    }

    #[test]
    fn a_compare_and_set_can_expect_an_absent_key() {
        let mut store = ScriptStateStore::default();

        let first = ScriptStateWrite::compare_and_set(
            "fresh".to_string(),
            None,
            Some(ScriptValue::from(json!("a"))),
        );
        assert_eq!(
            store.apply(&first).expect("accepted"),
            WriteOutcome::Applied { changed: true }
        );

        assert_eq!(
            store.apply(&first).expect("accepted"),
            WriteOutcome::Conflict {
                current: Some(ScriptValue::from(json!("a")))
            }
        );
    }

    #[test]
    fn an_empty_key_is_refused() {
        let mut store = ScriptStateStore::default();
        assert!(store.apply(&set("", json!(1))).is_err());
    }

    #[test]
    fn an_oversized_value_is_refused_and_leaves_the_store_alone() {
        let mut store = ScriptStateStore::default();
        let huge = "x".repeat(spool_shared_types::script_state::MAX_SERIALISED_BYTES + 1);
        assert!(store.apply(&set("big", json!(huge))).is_err());
        assert!(store.state().is_empty());
    }

    #[test]
    fn round_trips_through_a_file() {
        let path = unique_path("round-trip");

        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        store.write_file(&path).expect("written");

        let loaded = ScriptStateStore::read_file(&path).expect("read back");
        assert_eq!(loaded.get("counter"), Some(&ScriptValue::from(json!(7))));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn loading_rejects_keys_that_live_writes_cannot_accept() {
        let path = unique_path("invalid-key");
        for key in [
            String::new(),
            "x".repeat(spool_shared_types::script_state::MAX_KEY_BYTES + 1),
        ] {
            let saved = json!({
                "version": SUPPORTED_SCRIPT_STATE_VERSION,
                "state": {key: {"Int": 1}},
            });
            fs::write(&path, saved.to_string()).expect("written");
            let loaded = ScriptStateStore::read_file(&path);
            fs::remove_file(&path).expect("removed");
            assert!(
                loaded.is_none(),
                "invalid key must not enter the live store"
            );
        }
    }

    #[test]
    fn loading_rejects_a_store_over_the_live_capacity_limit() {
        let path = unique_path("over-capacity");
        let saved = json!({
            "version": SUPPORTED_SCRIPT_STATE_VERSION,
            "state": {"large": {"Str": "x".repeat(
                spool_shared_types::script_state::MAX_SERIALISED_BYTES
            )}},
        });
        fs::write(&path, saved.to_string()).expect("written");
        let loaded = ScriptStateStore::read_file(&path);
        fs::remove_file(&path).expect("removed");
        assert!(
            loaded.is_none(),
            "oversized state must not enter the live store"
        );
    }

    #[test]
    fn an_accepted_non_finite_write_does_not_destroy_saved_state() {
        let path = unique_path("non-finite-round-trip");
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        store.write_file(&path).expect("initial state saved");

        let write = ScriptStateWrite::set("bad".to_string(), ScriptValue::Float(f64::INFINITY));
        if store.apply(&write).is_ok() {
            store.write_file(&path).expect("accepted state saved");
        }
        let loaded = ScriptStateStore::read_file(&path);
        fs::remove_file(&path).expect("removed");
        assert_eq!(
            loaded.as_ref().and_then(|state| state.get("counter")),
            Some(&ScriptValue::Int(7)),
            "an accepted write must not make the entire saved store unreadable"
        );
    }

    #[test]
    fn non_finite_writes_are_rejected_without_mutation() {
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        store.dirty = false;
        let before = store.snapshot();
        let revision = store.revision_handle();
        let stamp = revision.load(Ordering::Acquire);

        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for value in [
                ScriptValue::Float(number),
                ScriptValue::Map(std::collections::BTreeMap::from([(
                    "nested".to_string(),
                    ScriptValue::List(vec![ScriptValue::Float(number)]),
                )])),
            ] {
                let write = ScriptStateWrite::set("counter".to_string(), value.clone());
                assert!(store.apply(&write).is_err());
                let cas = ScriptStateWrite::compare_and_set(
                    "counter".to_string(),
                    Some(ScriptValue::Int(7)),
                    Some(value),
                );
                assert!(store.apply(&cas).is_err());
                assert_eq!(store.state(), &before);
                assert_eq!(revision.load(Ordering::Acquire), stamp);
                assert!(!store.dirty);
            }
        }
    }

    fn nested_value(depth: usize, maps: bool, leaf: ScriptValue) -> ScriptValue {
        (0..depth).fold(leaf, |value, _| {
            if maps {
                ScriptValue::Map(std::collections::BTreeMap::from([(
                    "child".to_string(),
                    value,
                )]))
            } else {
                ScriptValue::List(vec![value])
            }
        })
    }

    fn assert_accepted_deep_write_preserves_saved_state(maps: bool) {
        let path = unique_path("deep-round-trip");
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        store.write_file(&path).expect("initial state saved");

        let value = nested_value(64, maps, ScriptValue::Int(1));
        let write = ScriptStateWrite::set("deep".to_string(), value);
        if store.apply(&write).is_ok() {
            store.write_file(&path).expect("accepted state saved");
        }
        let bytes = fs::read(&path).expect("read saved file");
        let loaded = ScriptStateStore::read_file(&path);
        let decoded = serde_json::from_slice::<SavedScriptState>(&bytes);
        fs::remove_file(&path).expect("removed");
        assert!(bytes.len() < spool_shared_types::script_state::MAX_SERIALISED_BYTES);
        assert_eq!(
            loaded.as_ref().and_then(|state| state.get("counter")),
            Some(&ScriptValue::Int(7)),
            "saved {} bytes (maps={maps}), decode error: {:?}",
            bytes.len(),
            decoded.err()
        );
    }

    #[test]
    fn an_accepted_deep_list_does_not_destroy_saved_state() {
        assert_accepted_deep_write_preserves_saved_state(false);
    }

    #[test]
    fn an_accepted_deep_map_does_not_destroy_saved_state() {
        assert_accepted_deep_write_preserves_saved_state(true);
    }

    #[test]
    fn persistent_depth_boundary_matches_the_json_loader() {
        for maps in [false, true] {
            for leaf in [
                ScriptValue::Null,
                ScriptValue::Bool(true),
                ScriptValue::Int(1),
                ScriptValue::Float(1.5),
                ScriptValue::Str("leaf".to_string()),
                ScriptValue::List(Vec::new()),
                ScriptValue::Map(std::collections::BTreeMap::new()),
            ] {
                for depth in [61, 62, 63] {
                    let value = nested_value(depth, maps, leaf.clone());
                    let leaf_depth =
                        usize::from(matches!(leaf, ScriptValue::List(_) | ScriptValue::Map(_)));
                    let expected = depth + leaf_depth <= 62;
                    let saved = json!({
                        "version": SUPPORTED_SCRIPT_STATE_VERSION,
                        "state": {"deep": value},
                    });
                    let path = unique_path("depth-boundary");
                    let bytes = serde_json::to_vec_pretty(&saved).expect("encodes");
                    fs::write(&path, &bytes).expect("fixture saved");
                    let loaded = ScriptStateStore::read_file(&path);
                    fs::remove_file(&path).expect("removed");
                    // This decoder has no ScriptState validation: it checks
                    // serde_json's own limit against the persistent file shape.
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
                        expected,
                        "depth={depth}, maps={maps}, leaf={leaf:?}"
                    );
                    assert_eq!(loaded.is_some(), expected);

                    let mut store = ScriptStateStore::default();
                    let write = ScriptStateWrite::set("deep".to_string(), value.clone());
                    assert_eq!(
                        store.apply(&write).is_ok(),
                        expected,
                        "write and load must agree: depth={depth}, maps={maps}, leaf={leaf:?}"
                    );
                    if expected {
                        store.write_file(&path).expect("accepted state saved");
                        let reloaded = ScriptStateStore::read_file(&path);
                        fs::remove_file(&path).expect("removed");
                        assert_eq!(
                            reloaded.as_ref().and_then(|state| state.get("deep")),
                            Some(&value)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn over_deep_writes_are_rejected_without_mutation() {
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        let before = store.snapshot();
        let revision = store.revision_handle();
        let stamp = revision.load(Ordering::Acquire);

        for dirty in [false, true] {
            store.dirty = dirty;
            for maps in [false, true] {
                let value = nested_value(63, maps, ScriptValue::Null);
                for write in [
                    ScriptStateWrite::set("counter".to_string(), value.clone()),
                    ScriptStateWrite::set("new".to_string(), value.clone()),
                    ScriptStateWrite::compare_and_set(
                        "counter".to_string(),
                        Some(ScriptValue::Int(7)),
                        Some(value),
                    ),
                ] {
                    let error = store.apply(&write).expect_err("too deep to persist");
                    assert!(error.contains("depth"), "{error}");
                    assert_eq!(store.state(), &before);
                    assert_eq!(revision.load(Ordering::Acquire), stamp);
                    assert_eq!(store.dirty, dirty);
                }
            }
        }
    }

    #[test]
    fn a_failed_temp_write_preserves_the_last_saved_file() {
        let path = unique_path("temp-write-failure");
        let tmp_path = path.with_extension("json.tmp");
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));
        store.write_file(&path).expect("initial state saved");
        let before = fs::read(&path).expect("read initial state");

        applied(&mut store, &set("counter", json!(8)));
        fs::create_dir(&tmp_path).expect("block the temporary file");
        let result = store.write_file(&path);
        let after = fs::read(&path).expect("read preserved state");
        fs::remove_dir(&tmp_path).expect("unblock the temporary file");

        assert!(result.is_err());
        assert_eq!(after, before);
        assert!(store.dirty);
        store.write_file(&path).expect("retry succeeds");
        let loaded = ScriptStateStore::read_file(&path).expect("read retried state");
        fs::remove_file(&path).expect("removed");
        assert_eq!(loaded.get("counter"), Some(&ScriptValue::Int(8)));
    }

    #[test]
    fn a_failed_rename_leaves_the_destination_untouched() {
        let path = unique_path("rename-failure");
        let marker_path = path.join("marker");
        fs::create_dir(&path).expect("block the destination with a directory");
        fs::write(&marker_path, b"preserve me").expect("marker written");
        let mut store = ScriptStateStore::default();
        applied(&mut store, &set("counter", json!(7)));

        let result = store.write_file(&path);
        let marker = fs::read(&marker_path).expect("destination preserved");
        let pending = ScriptStateStore::read_file(&path.with_extension("json.tmp"));
        fs::remove_file(&marker_path).expect("marker removed");
        fs::remove_dir(&path).expect("directory removed");
        fs::remove_file(path.with_extension("json.tmp")).expect("temporary file removed");

        assert!(result.is_err());
        assert_eq!(marker, b"preserve me");
        assert!(store.dirty);
        assert_eq!(
            pending.as_ref().and_then(|state| state.get("counter")),
            Some(&ScriptValue::Int(7))
        );
    }

    #[test]
    fn a_file_from_another_version_is_ignored() {
        let path = unique_path("version-mismatch");
        let stale = json!({ "version": SUPPORTED_SCRIPT_STATE_VERSION + 1, "state": {} });
        fs::write(&path, stale.to_string()).expect("written");

        assert!(ScriptStateStore::read_file(&path).is_none());

        let _ = fs::remove_file(path);
    }

    fn unique_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spool-script-state-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        ))
    }
}

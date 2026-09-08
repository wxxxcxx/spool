//! Embedded Lua scripting runtime (mlua).
//!
//! Lets a user's `init.lua` hook into window-manager events (`spool.on`),
//! bind keys to Lua callbacks or actions (`spool.bind`), read state via
//! `spool.query*`, and dispatch actions via `spool.run`.
//!
//! The interpreter runs on its own thread (see [`worker`]), not the main
//! thread: a handler is arbitrary user code of unbounded duration, and the
//! main thread hosts the Cocoa event pump, which a slow handler must not
//! freeze. As a result, `spool.query*` results are up to about a frame
//! stale, and actions a handler issues reach the action bus a frame later
//! than synchronous dispatch would. Handlers in a batch run concurrently;
//! actions from any one handler stay in the order it queued them, but
//! ordering across handlers follows completion, not registration.
//!
//! Every system takes the worker as `Option<Res<LuaWorker>>` so the mock test
//! harness (which never starts one) keeps compiling and no-ops gracefully.

mod api;
mod convert;
mod runtime;
mod worker;
mod world;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::app::{App, Plugin, PostUpdate, PreUpdate, Update};
use bevy::ecs::change_detection::DetectChangesMut as _;
use bevy::ecs::message::MessageReader;
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, NonSendMut, Query, Res, ResMut};
use notify::Watcher;

use crate::commands::Action;
use crate::config::Config;
use crate::ecs::params::Windows;
use crate::ecs::script_state::ScriptStateStore;
use crate::ecs::state::QueryStateParams;
use crate::ecs::{SendMessageTrigger, SpawnCommandsExt, apply_config_side_effects};
use crate::events::Event;
use crate::manager::{Application, Display, WindowManager};

use worker::FromLua;
pub use worker::{LuaSource, LuaWorker};

/// The Lua init-script path, kept as a resource so the reload system knows which
/// watched file to react to.
#[derive(Resource, Debug, Clone)]
pub struct LuaScriptPath(pub PathBuf);

/// What a `spool.state` call is told when there is no store in the world at
/// all. Only reachable in a harness that never inserted one.
const MISSING_STORE: &str = "the script state store is not available";

/// Registers the Lua runtime systems. Added only in the real app, not the mock
/// test harness.
pub struct LuaPlugin {}

impl Plugin for LuaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PreUpdate,
            (
                // Before the pump so a read left outstanding from last frame is
                // answered before the main thread goes back to sleep waiting on
                // Cocoa.
                serve_lua_queries.before(crate::ecs::systems::pump_events),
                serve_lua_store.before(crate::ecs::systems::pump_events),
                drain_lua_outbox,
                command_lua_handler,
            ),
        );
        // ...and again after everything, for reads made during this frame's
        // `Update` dispatch.
        app.add_systems(PostUpdate, (serve_lua_queries, serve_lua_store));
        app.add_systems(Update, (dispatch_lua_events, lua_reload_system));
    }
}

/// Forwards window-manager events to the worker for dispatch to `spool.on`
/// callbacks.
pub fn dispatch_lua_events(worker: Option<Res<LuaWorker>>, mut reader: MessageReader<Event>) {
    let Some(worker) = worker else {
        return;
    };
    // No `spool.on` handlers means nothing consumes these events; just
    // advance past them.
    if !worker.has_event_handlers() {
        for _ in reader.read() {}
        return;
    }
    let events: Vec<convert::LuaEvent> = reader
        .read()
        .filter_map(|event| convert::LuaEvent::try_from(event).ok())
        .collect();
    if events.is_empty() {
        return;
    }
    worker.send_events(events);
}

/// Handles `Action::Lua(id)` by handing the bound callback to the worker.
pub fn command_lua_handler(worker: Option<Res<LuaWorker>>, mut reader: MessageReader<Event>) {
    let Some(worker) = worker else {
        return;
    };
    let ids: Vec<u32> = reader
        .read()
        .filter_map(|event| match event {
            Event::ActionRequested {
                action: Action::Lua(id),
            } => Some(*id),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return;
    }
    worker.send_binds(ids);
}

/// Answers pending `spool.query*` and window-set reads from the worker.
///
/// Runs in both `PreUpdate` and `PostUpdate`. Each request kind is extracted
/// at most once per pass and shared among all waiters, since building the
/// query document reads every window's title over the accessibility API.
///
/// Deliberately does *not* take the script state store: Bevy derives a
/// system's access statically for the whole run, so asking for the store here
/// would make every pass hold it exclusively even when no script has
/// mentioned `spool.state`. See [`serve_lua_store`].
pub fn serve_lua_queries(worker: Option<Res<LuaWorker>>, state: QueryStateParams) {
    let Some(worker) = worker else {
        return;
    };
    // The worker freezes and bounds this batch. Consume it lazily so query
    // extraction also counts toward the time budget between requests.
    let requests = worker.pending_world_queries();

    // Filled on the first waiter that asks for that kind, reused by the rest.
    let mut extracted_state = None;
    let mut extracted_set = None;

    for request in requests {
        match request {
            worker::WorldRequest::State { reply } => {
                let _ = reply.try_send(extract_once(&mut extracted_state, || state.extract()));
            }
            worker::WorldRequest::WindowSet { reply } => {
                let _ = reply.try_send(extract_once(&mut extracted_set, || {
                    Ok(state.extract_window_set())
                }));
            }
        }
    }
}

/// Answers pending `spool.state` calls waiting on the store.
///
/// Separate from [`serve_lua_queries`] because this needs the store
/// *mutably*; keeping it in its own system limits that exclusivity to store
/// traffic only, rather than blocking every system that touches the store.
pub fn serve_lua_store(
    worker: Option<Res<LuaWorker>>,
    mut script_state: Option<ResMut<ScriptStateStore>>,
) {
    let Some(worker) = worker else {
        return;
    };
    for request in worker.pending_store_queries() {
        match request {
            worker::StoreRequest::Read { reply } => {
                let answer = script_state
                    .as_ref()
                    .map(|store| store.snapshot())
                    .ok_or_else(|| MISSING_STORE.to_string());
                let _ = reply.try_send(answer);
            }
            // Unlike a read, a write's reply is acted on: `spool.state.mutate`
            // retries when this reports the value was overtaken.
            worker::StoreRequest::Write { write, reply } => {
                let answer = script_state.as_mut().map_or_else(
                    || Err(MISSING_STORE.to_string()),
                    |store| store.apply(&write),
                );
                let _ = reply.try_send(answer);
            }
        }
    }
}

/// Reads the world once per pass, however many waiters ask for it.
///
/// `slot` holds what the first asker got — including a failure, which is
/// shared rather than retried per waiter.
fn extract_once<T>(
    slot: &mut Option<worker::Shared<T>>,
    extract: impl FnOnce() -> crate::errors::Result<T>,
) -> worker::Shared<T> {
    slot.get_or_insert_with(|| extract().map(Arc::new).map_err(|err| err.to_string()))
        .clone()
}

/// Puts what the callbacks queued onto the action bus.
///
/// Also the landing point for a reloaded `spool.setup{...}`: the worker
/// performs the rebuild, so the resulting config arrives here and is swapped
/// into the shared handle.
pub fn drain_lua_outbox(
    worker: Option<Res<LuaWorker>>,
    mut config: Option<ResMut<Config>>,
    mut displays: Query<&mut Display>,
    windows: Windows,
    applications: Query<&Application>,
    mut commands: Commands,
) {
    let Some(worker) = worker else {
        return;
    };
    for effect in worker.drain_outbox() {
        match effect {
            FromLua::Action(action) => {
                commands.trigger(SendMessageTrigger(Event::action_requested(action)));
            }
            FromLua::Flash { message, duration } => commands.flash_message(message, duration),
            FromLua::ConfigChanged => {
                // Swap into the shared handle, then re-apply the same side
                // effects a configuration reload does.
                if let (Some(config), Some(built)) = (config.as_mut(), worker.built_config()) {
                    config.replace_inner_from(&built);
                    config.set_changed();
                    apply_config_side_effects(config, &mut displays, &windows, &applications);
                }
            }
        }
    }
}

/// Rebuilds the Lua runtime when the init script changes, committing atomically
/// only on success so a broken edit never tears down the working setup.
pub fn lua_reload_system(
    worker: Option<Res<LuaWorker>>,
    script_path: Option<Res<LuaScriptPath>>,
    mut reader: MessageReader<Event>,
    window_manager: Res<WindowManager>,
    mut watcher: Option<NonSendMut<Box<dyn Watcher>>>,
) {
    let (Some(worker), Some(script_path)) = (worker, script_path) else {
        return;
    };
    let path = &script_path.0;

    let mut should_reload = false;
    for event in reader.read() {
        let Event::ConfigRefresh(event) = event else {
            continue;
        };
        if config_event_changes_script(event, path) {
            // Editors that atomically replace files (write-new-then-rename)
            // break the original watch; re-establish it on the configured path.
            if let Some(watcher) = watcher.as_mut()
                && let Some(new_watcher) = crate::ecs::rewatch_configs(&window_manager, path)
            {
                **watcher = new_watcher;
            }
            should_reload = true;
        }
    }
    if !should_reload {
        return;
    }

    worker.send_reload(path.clone());
}

/// Atomic rename events contain both paths; only the configured path matters.
fn paths_match(changed: &Path, script: &Path) -> bool {
    changed == script
}

fn config_event_changes_script(event: &notify::Event, script: &Path) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(_)
            | notify::EventKind::Any
    ) && event.paths.iter().any(|changed| {
        paths_match(changed, script)
            || crate::util::symlink_target(script).as_deref() == Some(changed.as_path())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn lua_watcher_handles_replacement_and_recreation_but_ignores_reads() {
        use notify::{
            Event, EventKind,
            event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode},
        };
        let script = Path::new("/tmp/spool-config-test/init.lua");
        for kind in [
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            EventKind::Remove(RemoveKind::File),
            EventKind::Create(CreateKind::File),
        ] {
            let event = Event::new(kind)
                .add_path(script.with_extension("lua.tmp"))
                .add_path(script.to_owned());
            assert!(config_event_changes_script(&event, script));
        }
        let read = Event::new(EventKind::Access(AccessKind::Any)).add_path(script.to_owned());
        assert!(!config_event_changes_script(&read, script));
        for other in [
            "/tmp/spool-config-test/bar.toml",
            "/tmp/spool-config-test/spool.toml",
            "/tmp/other/init.lua",
        ] {
            let event = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(other.into());
            assert!(!config_event_changes_script(&event, script));
        }
    }

    // Many concurrent waiters should share a single read of the world.
    #[test]
    fn one_extraction_answers_every_waiter() {
        let reads = Cell::new(0);
        let mut slot = None;
        let answers: Vec<_> = (0..5)
            .map(|_| {
                extract_once(&mut slot, || {
                    reads.set(reads.get() + 1);
                    Ok(7_u32)
                })
            })
            .collect();

        assert_eq!(reads.get(), 1, "the world should be read once for all five");
        for answer in &answers {
            assert_eq!(*answer.as_ref().expect("a successful read"), Arc::new(7));
        }
        let first = answers[0].as_ref().expect("a successful read");
        assert!(
            answers[1..]
                .iter()
                .all(|other| Arc::ptr_eq(first, other.as_ref().expect("a successful read"))),
            "every waiter should hold the same extraction"
        );
    }

    // A failed read is shared rather than retried per waiter.
    #[test]
    fn a_failed_extraction_is_shared_not_retried() {
        let reads = Cell::new(0);
        let mut slot = None;
        let answers: Vec<_> = (0..3)
            .map(|_| {
                extract_once(&mut slot, || -> crate::errors::Result<u32> {
                    reads.set(reads.get() + 1);
                    Err(crate::errors::Error::InvalidInput("no world".to_string()))
                })
            })
            .collect();

        assert_eq!(reads.get(), 1, "a failure should not be retried per waiter");
        assert!(answers.iter().all(std::result::Result::is_err));
    }
}

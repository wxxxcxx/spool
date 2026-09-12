//! The embedded Lua interpreter and everything a script registers into it.
//!
//! This module knows nothing about Bevy: it takes source in, hands dispatched
//! side effects back out as plain values ([`Action`]s and flash messages),
//! and reaches the world only through [`DispatchWorld`]. `mlua::Lua` is
//! `!Send`, so the interpreter lives on a thread of its own with the ECS on
//! the other side of a channel.
//!
//! Dispatch is `async` and handlers overlap — a handler that reads the world
//! suspends rather than blocking — so nothing here may hold a `RefCell`
//! borrow across an await.

use spool_shared_types::script_value::ScriptValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use mlua::prelude::{FromLua, IntoLua};
use mlua::{AnyUserData, Function, Lua, Table, Value};
use spool_shared_types::windowset::WindowSet;
use tracing::{error, warn};

use super::api;
use super::world::{DispatchBatch, DispatchWorld};
use crate::commands::Action;
use crate::config::Config;
use crate::platform::Modifiers;

/// A Lua-registered keybind: `(keycode, modifiers, handler_id)`.
pub type LuaKeybind = (u8, Modifiers, u32);

/// A store failure, spelled the way the script called it.
pub(super) fn store_error(call: &str, message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(format!("spool.state.{call}: {message}"))
}

/// A stored value as Lua sees it. Nothing stored is `nil`, which is how a
/// missing key and a removed one read the same.
///
/// Goes through [`ScriptValue`]'s own Lua conversion rather than mlua's serde
/// bridge, so a script sees the value it stored — serde would spell the enum as
/// its discriminant and hand back `{ Int = 5 }` instead of `5`.
pub(super) fn to_lua_value(lua: &Lua, value: Option<&ScriptValue>) -> mlua::Result<Value> {
    match value {
        Some(value) => value.clone().into_lua(lua),
        None => Ok(Value::Nil),
    }
}

/// A Lua value on its way into the store. Functions, coroutines and userdata
/// have no JSON to be, and say so at the call site rather than being quietly
/// dropped.
pub(super) fn from_lua_value(lua: &Lua, value: Value, call: &str) -> mlua::Result<ScriptValue> {
    ScriptValue::from_lua(value, lua)
        .map_err(|err| store_error(call, &format!("value cannot be stored: {err}")))
}

/// Prepends `SPOOL_LUA_PATH`/`SPOOL_LUA_CPATH` (if set) onto `package.path`/
/// `package.cpath`, so `require("sbar")` (or any module supplied via the Nix
/// `extraLuaPackages` option) resolves. Each var is itself a `;`-separated
/// list of Lua path templates - the same shape `package.path` already uses -
/// so nixpkgs' `getLuaPath`/`getLuaCPath` output passes straight through with
/// no reformatting.
fn extend_lua_search_paths(lua: &Lua) -> mlua::Result<()> {
    let package: Table = lua.globals().get("package")?;
    prepend_env_path(&package, "path", "SPOOL_LUA_PATH")?;
    prepend_env_path(&package, "cpath", "SPOOL_LUA_CPATH")?;
    Ok(())
}

/// Prepends `$env_var`'s value onto `package[field]`, so caller-supplied
/// modules are found before Lua's compiled-in defaults.
fn prepend_env_path(package: &Table, field: &str, env_var: &str) -> mlua::Result<()> {
    let Ok(extra) = std::env::var(env_var) else {
        return Ok(());
    };
    let extra = extra.trim_matches(';');
    if extra.is_empty() {
        return Ok(());
    }
    let existing: String = package.get(field)?;
    package.set(field, format!("{extra};{existing}"))?;
    Ok(())
}

#[derive(Clone)]
pub(super) struct HandlerEntry {
    pub(super) filter: Option<Function>,
    pub(super) handler: Function,
}

#[derive(Clone)]
pub(super) enum BindHandler {
    Action(Action),
    Function(Function),
}

/// Everything the script registered, kept on the Rust side so dispatch never
/// has to reach back into Lua globals to find a callback.
#[derive(Default)]
pub(super) struct Registry {
    /// `spool.on` handlers, in registration order per event name.
    pub(super) handlers: HashMap<String, Vec<HandlerEntry>>,
    /// `spool.bind` callbacks keyed by process-unique ids, never reused on reload.
    pub(super) binds: HashMap<u32, BindHandler>,
    /// Chords in registration order, for publishing to the event-tap registry.
    pub(super) keybinds: Vec<LuaKeybind>,
}

/// Shared handle to the [`Registry`], captured by the registration closures and
/// read back when dispatching.
pub(super) type SharedRegistry = Rc<RefCell<Registry>>;

/// Pending side effects produced by Lua callbacks, drained after each dispatch.
#[derive(Default)]
pub(super) struct Outbox {
    /// Actions queued via `spool.run` / `spool.action` / compatibility aliases.
    pub(super) actions: Vec<Action>,
    /// Flash messages queued via `spool.flash` with validated durations.
    pub(super) flashes: Vec<(String, Duration)>,
}

/// The side effects one dispatch produced: actions to put on the bus and
/// flash messages to show.
///
/// Script state writes are not among them: they are the one thing a handler
/// has to see the result of, so they go and come back while the handler
/// waits rather than being queued for later.
pub(super) type Effects = (Vec<Action>, Vec<(String, Duration)>);

/// The embedded Lua runtime and its shared registration state.
pub struct LuaRuntime {
    lua: Lua,
    outbox: Rc<RefCell<Outbox>>,
    registry: SharedRegistry,
    /// World access shared with the `spool.*` functions. Each dispatch supplies
    /// its input batch; only the revision-keyed script store spans batches.
    world: Rc<DispatchWorld>,
    /// The `Config` a script declared via `spool.setup{...}`, if it called it.
    /// `None` means the script uses built-in defaults.
    built_config: Option<Config>,
}

impl LuaRuntime {
    /// Builds a runtime by reading and executing the script at `path`.
    pub fn from_file(path: &Path, world: &Rc<DispatchWorld>) -> mlua::Result<Self> {
        let source = std::fs::read_to_string(path).map_err(mlua::Error::external)?;
        Self::from_source(&source, world)
    }

    /// Builds a runtime from Lua source, installing the `spool` API and
    /// executing the script. Registered keybinds are collected for publishing.
    pub fn from_source(source: &str, world: &Rc<DispatchWorld>) -> mlua::Result<Self> {
        // SAFETY: the runtime is confined to the thread that built it, and
        // unsafe libs are what let a script `require` a C module such as
        // sketchybar's.
        let lua = unsafe { Lua::unsafe_new() };
        extend_lua_search_paths(&lua)?;
        let outbox = Rc::new(RefCell::new(Outbox::default()));
        let registry = SharedRegistry::default();
        let config_cell: Rc<RefCell<Option<Config>>> = Rc::new(RefCell::new(None));
        api::install(&lua, &outbox, &registry, &config_cell, world)?;
        lua.load(source).exec()?;
        // The cell is only written by `spool.setup`; take the built config out so
        // the runtime owns it directly rather than keeping the shared cell alive.
        let built_config = config_cell.borrow_mut().take();
        Ok(Self {
            lua,
            outbox,
            registry,
            world: Rc::clone(world),
            built_config,
        })
    }

    pub(super) fn lua(&self) -> &Lua {
        &self.lua
    }

    /// The `(keycode, modifiers, id)` keybinds registered by the script, for
    /// publishing to the event-tap registry.
    pub fn published_keybinds(&self) -> Vec<LuaKeybind> {
        self.registry.borrow().keybinds.clone()
    }

    /// The `Config` the script declared via `spool.setup{...}`, or `None` if it
    /// never called it (in which case built-in defaults apply).
    pub fn built_config(&self) -> Option<&Config> {
        self.built_config.as_ref()
    }

    /// Whether the script registered any `spool.on` handler. Building the Lua
    /// table for an event is the costly step, so a script that only binds keys
    /// pays nothing for events it can never observe.
    pub(super) fn has_event_handlers(&self) -> bool {
        !self.registry.borrow().handlers.is_empty()
    }

    /// The handlers registered for `name`, in registration order.
    ///
    /// Cloned out of the registry rather than borrowed: they are dispatched
    /// concurrently and one may register another, so nothing may be holding the
    /// borrow while they run.
    pub(super) fn event_handlers(&self, name: &str) -> Vec<HandlerEntry> {
        self.registry
            .borrow()
            .handlers
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// Runs one `spool.on` handler.
    ///
    /// `call_async`, so a handler that reads the world suspends here instead of
    /// holding the interpreter — which is what lets the next handler run rather
    /// than queue behind this one.
    pub(super) async fn dispatch_event(
        &self,
        name: &str,
        event: &Table,
        handler: &Function,
        batch: &Rc<DispatchBatch>,
    ) {
        let context = format!("event handler '{name}'");
        self.world
            .with_batch(batch, async {
                let Some(window_set) = self.window_set_arg(&context).await else {
                    return;
                };
                match handler
                    .call_async::<Value>((event.clone(), window_set))
                    .await
                {
                    Ok(returned) => self.commit(&returned, &context),
                    Err(err) => error!("lua {context}: {err}"),
                }
            })
            .await;
    }

    /// Runs the action function or callback bound to keybind `id`.
    pub(super) async fn dispatch_bind(&self, id: u32, batch: &Rc<DispatchBatch>) {
        let handler = self.registry.borrow().binds.get(&id).cloned();
        match handler {
            Some(BindHandler::Action(action)) => {
                self.outbox.borrow_mut().actions.push(action);
            }
            Some(BindHandler::Function(handler)) => {
                let context = format!("keybind handler {id}");
                self.world
                    .with_batch(batch, async {
                        let Some(window_set) = self.window_set_arg(&context).await else {
                            return;
                        };
                        match handler.call_async::<Value>(window_set).await {
                            Ok(returned) => self.commit(&returned, &context),
                            Err(err) => error!("lua {context}: {err}"),
                        }
                    })
                    .await;
            }
            None => warn!("lua keybind {id} has no handler"),
        }
    }

    /// The window set a handler is handed, materialised up front since
    /// fetching it lazily would need a synchronous call that cannot suspend.
    /// Each handler gets its own copy to transform.
    async fn window_set_arg(&self, context: &str) -> Option<AnyUserData> {
        let set = match self.world.layout().await {
            Ok(set) => set,
            Err(err) => {
                error!("lua {context}: {err}");
                return None;
            }
        };
        self.lua
            .create_userdata((*set).clone())
            .inspect_err(|err| error!("lua {context}: {err}"))
            .ok()
    }

    /// Queues whatever a handler returned. Returning a window set replays the
    /// operations recorded on it against the live world; returning nothing
    /// commits nothing, so computing a set without returning it is free of
    /// consequences.
    fn commit(&self, returned: &Value, context: &str) {
        match returned {
            Value::Nil => {}
            Value::UserData(data) => {
                if let Ok(window_set) = data.borrow::<WindowSet>() {
                    let ops = window_set.plan();
                    if !ops.is_empty() {
                        self.outbox.borrow_mut().actions.push(Action::Layout(ops));
                    }
                } else {
                    warn!("lua {context} returned userdata that is not a window set");
                }
            }
            other => warn!(
                "lua {context} returned {}; a handler returns a window set, or nothing",
                other.type_name()
            ),
        }
    }

    /// Takes everything the callbacks queued, leaving the outbox empty.
    pub(super) fn drain_outbox(&self) -> Effects {
        let mut outbox = self.outbox.borrow_mut();
        (
            outbox.actions.drain(..).collect(),
            outbox.flashes.drain(..).collect(),
        )
    }

    /// An empty runtime (no handlers, no binds). Used as a fallback when the
    /// init script fails to load, so the resource always exists and a later
    /// hot reload can install a fixed script.
    pub fn empty(world: &Rc<DispatchWorld>) -> Self {
        Self::from_source("", world).expect("empty Lua runtime should always build")
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_channel::{Receiver, unbounded};
    use futures_lite::future::{block_on, poll_once};
    use spool_shared_types::script_state::{ScriptState, ScriptStateWrite, WriteOutcome};
    use spool_shared_types::windowset::WindowSet;

    use super::super::convert;
    use super::super::worker::{Shared, StoreRequest, WorldRequest};
    use super::super::world::WorldAccess;
    use super::*;
    use crate::ecs::state::SpoolQueryState;
    use crate::events::Event;

    /// A competing writer, run just before a write lands.
    type Interjection = Box<dyn FnMut(&mut ScriptState)>;

    /// The main thread's half of the script state store: what a read answers
    /// from and what a write lands in, with the revision the worker's cache
    /// watches.
    struct TestWorld {
        store: RefCell<ScriptState>,
        revision: Arc<AtomicU64>,
        /// For the tests that need someone else to get there first.
        interject: RefCell<Option<Interjection>>,
        /// The main thread's ends of the two request channels, drained by
        /// [`TestWorld::drive`].
        world_queries: Receiver<WorldRequest>,
        store_queries: Receiver<StoreRequest>,
        dispatch: Rc<DispatchWorld>,
    }

    impl Default for TestWorld {
        fn default() -> Self {
            let (world_tx, world_queries) = unbounded();
            let (store_tx, store_queries) = unbounded();
            let revision = Arc::new(AtomicU64::new(0));
            Self {
                store: RefCell::new(ScriptState::default()),
                revision: Arc::clone(&revision),
                interject: RefCell::new(None),
                world_queries,
                store_queries,
                dispatch: DispatchWorld::new(WorldAccess::new(world_tx, store_tx, revision)),
            }
        }
    }

    impl TestWorld {
        /// A runtime whose world reads come back through this world.
        fn runtime(&self, source: &str) -> mlua::Result<LuaRuntime> {
            LuaRuntime::from_source(source, &self.dispatch)
        }

        /// Runs one dispatch to completion, answering its reads the way the
        /// main thread's `serve_lua_queries` does. A dispatch suspends
        /// whenever it reads the world, so this polls the future and drains
        /// the request queue in turn rather than simply blocking on it.
        fn drive<T>(
            &self,
            extract: &dyn Fn() -> Shared<SpoolQueryState>,
            future: impl Future<Output = T>,
        ) -> T {
            /// Ample for any dispatch here; only reached if one is wedged.
            const TURNS: usize = 1000;

            let mut future = Box::pin(future);
            for _ in 0..TURNS {
                if let Some(done) = block_on(poll_once(future.as_mut())) {
                    return done;
                }
                while let Ok(request) = self.world_queries.try_recv() {
                    match request {
                        WorldRequest::State { reply } => {
                            let _ = reply.try_send(extract());
                        }
                        WorldRequest::WindowSet { reply } => {
                            let _ = reply.try_send(Ok(Arc::new(WindowSet::default())));
                        }
                    }
                }
                while let Ok(request) = self.store_queries.try_recv() {
                    match request {
                        StoreRequest::Read { reply } => {
                            let _ = reply.try_send(self.read());
                        }
                        StoreRequest::Write { write, reply } => {
                            let _ = reply.try_send(self.write(&write));
                        }
                    }
                }
            }
            panic!("the dispatch never finished");
        }

        /// [`Self::drive`], but waiting between turns instead of spinning: for
        /// a dispatch parked on `spool.exec`, which is waiting on a process
        /// rather than on this thread.
        fn drive_patiently<T>(
            &self,
            extract: &dyn Fn() -> Shared<SpoolQueryState>,
            future: impl Future<Output = T>,
        ) -> T {
            const TURNS: usize = 2_000;

            let mut future = Box::pin(future);
            for _ in 0..TURNS {
                if let Some(done) = block_on(poll_once(future.as_mut())) {
                    return done;
                }
                while let Ok(request) = self.world_queries.try_recv() {
                    match request {
                        WorldRequest::State { reply } => {
                            let _ = reply.try_send(extract());
                        }
                        WorldRequest::WindowSet { reply } => {
                            let _ = reply.try_send(Ok(Arc::new(WindowSet::default())));
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("the dispatch never finished");
        }

        // Fallible to match what the worker hands the runtime, which reads over
        // a channel that can be gone.
        #[allow(clippy::unnecessary_wraps)]
        fn read(&self) -> Result<ScriptState, String> {
            Ok(self.store.borrow().clone())
        }

        fn write(&self, write: &ScriptStateWrite) -> Result<WriteOutcome, String> {
            if let Some(interject) = self.interject.borrow_mut().as_mut() {
                interject(&mut self.store.borrow_mut());
                self.revision.fetch_add(1, Ordering::Release);
            }
            let outcome = self.store.borrow_mut().apply(write)?;
            if matches!(outcome, WriteOutcome::Applied { changed: true }) {
                self.revision.fetch_add(1, Ordering::Release);
            }
            Ok(outcome)
        }

        fn get(&self, key: &str) -> Option<ScriptValue> {
            self.store.borrow().get(key).cloned()
        }
    }

    /// Drains the outbox commands for assertions.
    fn drained_commands(runtime: &LuaRuntime) -> Vec<Action> {
        runtime.outbox.borrow_mut().actions.drain(..).collect()
    }

    fn bind_id(runtime: &LuaRuntime, position: usize) -> u32 {
        runtime.published_keybinds()[position - 1].2
    }

    #[test]
    fn bind_registers_keybind_and_stores_handler() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(r#"spool.bind("alt+j", spool.action.window.focus_east)"#)
            .unwrap();
        let binds = runtime.published_keybinds();
        assert_eq!(binds.len(), 1);
        let (_, modifiers, id) = binds[0];
        assert_eq!(modifiers, Modifiers::ALT);
        assert_ne!(id, 0);
    }

    #[test]
    fn a_runtime_rejects_binding_ids_from_another_runtime() {
        let world = TestWorld::default();
        let old = world
            .runtime(r#"spool.bind("alt+a", spool.action.window.focus_west)"#)
            .unwrap();
        let current = world
            .runtime(r#"spool.bind("alt+b", spool.action.window.balance)"#)
            .unwrap();
        let extract = || Err("action bindings must not query the world".to_string());
        for stale in [0, bind_id(&old, 1), u32::MAX] {
            world.drive(
                &extract,
                current.dispatch_bind(stale, &DispatchBatch::new()),
            );
        }
        assert!(drained_commands(&current).is_empty());
        world.drive(
            &extract,
            current.dispatch_bind(bind_id(&current, 1), &DispatchBatch::new()),
        );
        assert!(matches!(
            drained_commands(&current).as_slice(),
            [Action::Window(crate::commands::Operation::Balance)]
        ));
    }

    #[test]
    fn bind_accepts_a_chord_array_for_one_action_function() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(r#"spool.bind({ "alt+k", "alt+pageup" }, spool.action.window.focus_north)"#)
            .unwrap();
        let binds = runtime.published_keybinds();
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[0].1, Modifiers::ALT);
        assert_eq!(binds[1].1, Modifiers::ALT);
        assert_ne!(binds[0].2, binds[1].2);

        let extract = || Err("action functions must not query the window set".to_string());
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 2), &DispatchBatch::new()),
        );
        let actions = drained_commands(&runtime);
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|action| matches!(
            action,
            Action::Window(crate::commands::Operation::Focus(
                crate::commands::Direction::North
            ))
        )));
    }

    #[test]
    fn setup_builds_config_without_bindings() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r"spool.setup{
                options = { sliver_width = 7 },
            }",
            )
            .unwrap();
        assert!(runtime.published_keybinds().is_empty());
        let config = runtime.built_config().expect("setup should build a config");
        assert_eq!(config.sliver_width(), 7);
    }

    #[test]
    fn no_setup_call_uses_builtin_defaults() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(r#"spool.bind("alt+b", spool.action.window.balance)"#)
            .unwrap();
        assert!(runtime.built_config().is_none());
    }

    #[test]
    fn action_function_keybind_dispatches_without_world_access() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(r#"spool.bind("alt+b", spool.action.window.balance)"#)
            .unwrap();
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        let commands = drained_commands(&runtime);
        assert!(
            matches!(
                commands.as_slice(),
                [Action::Window(crate::commands::Operation::Balance)]
            ),
            "expected a balance action, got {commands:?}"
        );
    }

    #[test]
    fn function_keybind_can_run_commands() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(r#"spool.bind("alt+j", function(state) spool.run("window focus east") end)"#)
            .unwrap();
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        assert_eq!(drained_commands(&runtime).len(), 1);
    }

    #[test]
    fn event_handler_receives_event_and_queues_command() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"spool.on("space_changed", function(e) spool.run("space layout balance") end)"#,
            )
            .unwrap();
        let (name, table) = convert::event_to_lua(runtime.lua(), &Event::SpaceChanged).unwrap();
        let extract = || Ok(Arc::new(test_state()));
        for handler in runtime.event_handlers(&name) {
            world.drive(
                &extract,
                runtime.dispatch_event(&name, &table, &handler.handler, &DispatchBatch::new()),
            );
        }
        assert_eq!(drained_commands(&runtime).len(), 1);
    }

    #[test]
    fn invalid_run_action_string_is_reported_not_panicking() {
        let world = TestWorld::default();
        // A bad action string surfaces as a Lua runtime error at bind time.
        let result = world.runtime(r#"spool.run("definitely not an action")"#);
        assert!(result.is_err());
    }

    #[test]
    fn bind_rejects_command_strings() {
        let world = TestWorld::default();
        let result = world.runtime(r#"spool.bind("alt+j", "window focus east")"#);
        assert!(result.is_err());
    }

    /// A canned state document to answer queries with.
    fn test_state() -> SpoolQueryState {
        use crate::ecs::state::{SpaceKind, SpoolActiveState, SpoolSpaceState, SpoolWindowState};

        SpoolQueryState {
            version: 3,
            timestamp: 0,
            active: SpoolActiveState {
                focused_window_id: Some(7),
                focused_app_name: Some("Test App".to_string()),
                ..SpoolActiveState::default()
            },
            capabilities: spool_shared_types::state::SpaceCapabilities::default(),
            displays: Vec::new(),
            spaces: vec![SpoolSpaceState {
                space_id: 10,
                display_id: 1,
                ordinal: 0,
                kind: SpaceKind::User,
                visible: true,
                focused: true,
                windows: vec![SpoolWindowState {
                    window_id: 7,
                    bundle_id: "com.example.app".to_string(),
                    app_name: "Test App".to_string(),
                    title: "window".to_string(),
                    focused: true,
                    floating: false,
                    display_id: Some(1),
                    frame: None,
                    visible: true,
                }],
            }],
        }
    }

    #[test]
    fn query_is_answered_from_the_provided_state() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
            spool.bind("alt+q", function()
              local active = spool.query_active()
              spool.flash(active.focused_app_name)
              spool.flash(tostring(#spool.query_spaces()))
              spool.flash(spool.query("active"))
            end)
            "#,
            )
            .unwrap();
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );

        let flashes: Vec<String> = runtime
            .outbox
            .borrow_mut()
            .flashes
            .drain(..)
            .map(|(message, _)| message)
            .collect();
        assert_eq!(flashes.len(), 3, "every query should have returned");
        assert_eq!(flashes[0], "Test App");
        assert_eq!(flashes[1], "1");
        assert!(
            flashes[2].starts_with('{') && flashes[2].contains("\"focused_app_name\":\"Test App\""),
            "spool.query should return raw JSON, got {}",
            flashes[2]
        );
    }

    #[test]
    fn overlapping_callbacks_read_their_own_state_snapshots() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
                spool.bind("alt+a", function()
                    before = spool.query_active().focused_app_name
                    spool.state.get("pause")
                    after = spool.query_active().focused_app_name
                end)
                spool.bind("alt+b", function()
                    later = spool.query_active().focused_app_name
                end)
                "#,
            )
            .unwrap();
        let older_batch = DispatchBatch::new();
        let mut older = Box::pin(runtime.dispatch_bind(bind_id(&runtime, 1), &older_batch));

        assert!(block_on(poll_once(older.as_mut())).is_none());
        let WorldRequest::WindowSet { reply } = world.world_queries.try_recv().unwrap() else {
            panic!("expected the older callback's layout");
        };
        reply.try_send(Ok(Arc::new(WindowSet::default()))).unwrap();
        assert!(block_on(poll_once(older.as_mut())).is_none());
        let WorldRequest::State { reply } = world.world_queries.try_recv().unwrap() else {
            panic!("expected the older callback's state");
        };
        reply.try_send(Ok(Arc::new(test_state()))).unwrap();
        assert!(block_on(poll_once(older.as_mut())).is_none());
        let StoreRequest::Read { reply: paused } = world.store_queries.try_recv().unwrap() else {
            panic!("expected a store read to park the older callback");
        };

        let fresh = || {
            let mut state = test_state();
            state.active.focused_app_name = Some("New App".to_string());
            Ok(Arc::new(state))
        };
        world.drive(
            &fresh,
            runtime.dispatch_bind(bind_id(&runtime, 2), &DispatchBatch::new()),
        );
        assert_eq!(
            runtime.lua().globals().get::<String>("later").unwrap(),
            "New App",
            "a later callback must not inherit a suspended callback's snapshot"
        );

        paused.try_send(world.read()).unwrap();
        world.drive(&fresh, older);
        let globals = runtime.lua().globals();
        assert_eq!(globals.get::<String>("before").unwrap(), "Test App");
        assert_eq!(
            globals.get::<String>("after").unwrap(),
            "Test App",
            "resuming an older callback must preserve its original snapshot"
        );
    }

    #[test]
    fn the_state_is_extracted_once_per_dispatch_and_only_on_demand() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
            spool.bind("alt+q", function()
              spool.query_active()
              spool.query_on_screen()
            end)
            spool.bind("alt+w", function() spool.flash("no query here") end)
            "#,
            )
            .unwrap();

        let extractions = RefCell::new(0);
        let extract = || {
            *extractions.borrow_mut() += 1;
            Ok(Arc::new(test_state()))
        };
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 2), &DispatchBatch::new()),
        );
        assert_eq!(
            *extractions.borrow(),
            0,
            "a handler that never queries pays nothing"
        );

        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        assert_eq!(*extractions.borrow(), 1, "two queries share one extraction");
    }

    #[test]
    fn query_outside_a_callback_explains_itself() {
        let world = TestWorld::default();
        let Err(error) = world.runtime("spool.query_state()") else {
            panic!("there is no world to query at script top level");
        };
        let error = error.to_string();
        assert!(
            error.contains("only available inside"),
            "expected an explanation, got {error}"
        );

        // ...and the provider does not outlive the dispatch that installed it.
        let runtime = world
            .runtime(
                r#"
            escaped = nil
            spool.bind("alt+q", function() escaped = spool.query_state end)
            "#,
            )
            .unwrap();
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        assert!(
            runtime.lua().load("escaped()").exec().is_err(),
            "a query captured during dispatch should not answer afterwards"
        );
    }

    #[test]
    fn a_window_set_outside_a_callback_explains_itself() {
        let world = TestWorld::default();
        // Same contract as `spool.query`: there is no world to read at script
        // top level, so say so rather than answering from nothing.
        let runtime = world.runtime("").unwrap();
        let error = runtime
            .lua()
            // The set has to actually be *used*: it is lazy, so merely
            // being handed one costs nothing and cannot fail.
            .load("return spool.windows(function(ws) return ws:focus(1) end)")
            .exec()
            .expect_err("there is no window set at script top level")
            .to_string();
        assert!(
            error.contains("only available inside"),
            "expected an explanation, got {error}"
        );
    }

    #[test]
    fn unknown_query_kinds_are_rejected() {
        let world = TestWorld::default();
        let runtime = world.runtime("").unwrap();
        let extract = || Ok(Arc::new(test_state()));
        // Driven as a dispatch, because `spool.query` is async now: at top
        // level there is no coroutine to suspend in.
        let error = world
            .drive(&extract, async {
                runtime
                    .lua()
                    .load(r#"return spool.query("windows")"#)
                    .exec_async()
                    .await
            })
            .expect_err("'windows' is not a query kind")
            .to_string();
        assert!(
            error.contains("unknown kind 'windows'") && error.contains("on-screen"),
            "the error should list the valid kinds, got {error}"
        );
    }

    #[test]
    fn dispatch_hands_back_what_the_callbacks_queued() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
            spool.bind("alt+b", function()
              spool.run("space layout balance")
              spool.flash("done", 3.0)
            end)
            "#,
            )
            .unwrap();
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
        let (commands, flashes) = runtime.drain_outbox();
        assert_eq!(commands.len(), 1);
        assert_eq!(flashes, vec![("done".to_string(), Duration::from_secs(3))]);
        // ...and the outbox is empty afterwards, so nothing is delivered twice.
        assert!(runtime.drain_outbox().0.is_empty());
    }

    #[test]
    fn flash_reports_invalid_durations_and_preserves_valid_seconds() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
                for _, duration in ipairs({ -1, 0/0, math.huge, -math.huge, 1e30 }) do
                    local ok, err = pcall(spool.flash, "invalid", duration)
                    assert(not ok and tostring(err):find("spool.flash: invalid duration", 1, true))
                end
                spool.flash("default")
                spool.flash("fractional", 0.125)
                spool.flash("zero", 0)
                "#,
            )
            .unwrap();
        assert_eq!(
            runtime.drain_outbox().1,
            vec![
                ("default".to_string(), Duration::from_secs(2)),
                ("fractional".to_string(), Duration::from_millis(125)),
                ("zero".to_string(), Duration::ZERO),
            ]
        );
    }

    /// Runs `source` as a keybind handler against `world`'s store.
    fn run_with_store(source: &str, world: &TestWorld) {
        let runtime = world
            .runtime(&format!(r#"spool.bind("alt+z", function() {source} end)"#))
            .expect("script should load");
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );
    }

    #[test]
    fn state_round_trips_every_kind_of_value() {
        let world = TestWorld::default();
        run_with_store(
            r#"
            spool.state.set("string", "hello")
            spool.state.set("number", 42)
            spool.state.set("bool", true)
            spool.state.set("table", { a = 1, nested = { "x", "y" } })
            "#,
            &world,
        );

        assert_eq!(
            world.get("string"),
            Some(ScriptValue::from(serde_json::json!("hello")))
        );
        assert_eq!(
            world.get("number"),
            Some(ScriptValue::from(serde_json::json!(42)))
        );
        assert_eq!(
            world.get("bool"),
            Some(ScriptValue::from(serde_json::json!(true)))
        );
        assert_eq!(
            world.get("table"),
            Some(ScriptValue::from(
                serde_json::json!({ "a": 1, "nested": ["x", "y"] })
            ))
        );
    }

    #[test]
    fn a_stored_value_reads_back_as_the_same_lua_value() {
        let world = TestWorld::default();
        run_with_store(
            r#"
            spool.state.set("pad", { id = 7, name = "term" })
            local pad = spool.state.get("pad")
            spool.state.set("echo", pad.id .. "/" .. pad.name)
            "#,
            &world,
        );
        assert_eq!(
            world.get("echo"),
            Some(ScriptValue::from(serde_json::json!("7/term")))
        );
    }

    #[test]
    fn setting_nil_removes_the_key() {
        let world = TestWorld::default();
        run_with_store(
            r#"
            spool.state.set("gone", 1)
            spool.state.set("gone", nil)
            spool.state.set("was_nil", spool.state.get("gone") == nil)
            "#,
            &world,
        );
        assert_eq!(world.get("gone"), None);
        assert_eq!(
            world.get("was_nil"),
            Some(ScriptValue::from(serde_json::json!(true)))
        );
    }

    #[test]
    fn mutate_hands_the_current_value_in_and_stores_what_comes_back() {
        let world = TestWorld::default();
        run_with_store(
            r#"
            spool.state.mutate("count", function(n) return (n or 0) + 1 end)
            spool.state.mutate("count", function(n) return n + 1 end)
            spool.state.set("returned", spool.state.mutate("count", function(n) return n + 1 end))
            "#,
            &world,
        );
        assert_eq!(
            world.get("count"),
            Some(ScriptValue::from(serde_json::json!(3)))
        );
        assert_eq!(
            world.get("returned"),
            Some(ScriptValue::from(serde_json::json!(3)))
        );
    }

    #[test]
    fn mutate_returning_nil_removes_the_key() {
        let world = TestWorld::default();
        run_with_store(
            r#"
            spool.state.set("pad", 1)
            spool.state.mutate("pad", function() return nil end)
            "#,
            &world,
        );
        assert_eq!(world.get("pad"), None);
    }

    /// `spool.exec` suspends the handler instead of holding the interpreter,
    /// so a slow child process does not stall every other handler.
    ///
    /// Uses `drive_patiently` rather than `drive`, which spins a fixed number
    /// of turns and would give up before a real process finished.
    #[test]
    fn exec_runs_a_program_and_hands_back_its_output() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"spool.bind("alt+z", function()
                       local result = spool.exec("/bin/echo", {"hello"})
                       code, out = result.code, result.stdout
                   end)"#,
            )
            .expect("script should load");

        let extract = || Ok(Arc::new(test_state()));
        world.drive_patiently(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );

        let globals = runtime.lua().globals();
        assert_eq!(globals.get::<i32>("code").expect("an exit code"), 0);
        assert_eq!(
            globals
                .get::<String>("out")
                .expect("captured stdout")
                .trim(),
            "hello"
        );
    }

    #[test]
    fn exec_reports_a_program_that_is_not_there_as_an_error() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"spool.bind("alt+z", function()
                       local ok, err = pcall(function()
                           spool.exec("/nonexistent/spool-test-binary")
                       end)
                       succeeded, message = ok, tostring(err)
                   end)"#,
            )
            .expect("script should load");

        let extract = || Ok(Arc::new(test_state()));
        world.drive_patiently(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );

        let globals = runtime.lua().globals();
        assert!(
            !globals.get::<bool>("succeeded").expect("a pcall result"),
            "a missing program must surface to the script, not be swallowed"
        );
        assert!(
            globals
                .get::<String>("message")
                .expect("an error message")
                .contains("exec:"),
            "the error should say which layer failed"
        );
    }

    #[test]
    fn mutate_retries_against_a_writer_that_got_there_first() {
        let world = TestWorld::default();
        world.store.borrow_mut().apply(&set("count", 10)).unwrap();

        // Someone else — another handler, or a client over the socket — writes
        // the same key between this mutate's read and its write. Exactly once,
        // so the retry is the thing that succeeds.
        let mut interjected = false;
        *world.interject.borrow_mut() = Some(Box::new(move |state| {
            if !interjected {
                interjected = true;
                state.apply(&set("count", 100)).expect("accepted");
            }
        }));

        run_with_store(
            r#"spool.state.mutate("count", function(n) return n + 1 end)"#,
            &world,
        );

        // 101, not 11: the increment was re-run against the value that landed
        // first, so nothing was lost. A get-then-set would have stored 11 and
        // silently thrown the other write away.
        assert_eq!(
            world.get("count"),
            Some(ScriptValue::from(serde_json::json!(101)))
        );
    }

    #[test]
    fn a_value_that_is_not_storable_is_refused_at_the_call_site() {
        let world = TestWorld::default();
        let runtime = world
            .runtime(
                r#"
            errored = false
            spool.bind("alt+z", function()
              local ok = pcall(function() spool.state.set("fn", function() end) end)
              errored = not ok
            end)
            "#,
            )
            .expect("script should load");
        let extract = || Ok(Arc::new(test_state()));
        world.drive(
            &extract,
            runtime.dispatch_bind(bind_id(&runtime, 1), &DispatchBatch::new()),
        );

        assert!(
            runtime.lua.globals().get::<bool>("errored").unwrap(),
            "storing a function should raise, not be silently dropped"
        );
        assert!(world.store.borrow().is_empty());
    }

    #[test]
    fn the_store_is_out_of_reach_at_script_top_level() {
        let world = TestWorld::default();
        // There is no world outside a callback, so there is nothing to read
        // from or write to, and it says so rather than inventing a value.
        let Err(error) = world.runtime(r#"spool.state.get("anything")"#) else {
            panic!("top-level access should fail");
        };
        assert!(
            error.to_string().contains("spool.state is only available"),
            "unexpected error: {error}"
        );
    }

    fn set(key: &str, value: i64) -> ScriptStateWrite {
        ScriptStateWrite::set(key.to_string(), ScriptValue::Int(value))
    }

    #[test]
    fn empty_runtime_has_no_binds() {
        let world = TestWorld::default();
        let runtime = LuaRuntime::empty(&world.dispatch);
        assert!(runtime.published_keybinds().is_empty());
    }

    #[test]
    fn extra_lua_path_env_var_is_prepended() {
        let world = TestWorld::default();
        // Unique var name so this doesn't race other tests' env state.
        // SAFETY: no other test reads or writes this variable.
        unsafe { std::env::set_var("SPOOL_LUA_PATH", "/tmp/spool-test/?.lua") };
        let runtime = world.runtime("").unwrap();
        let package: Table = runtime.lua().globals().get("package").unwrap();
        let path: String = package.get("path").unwrap();
        // SAFETY: no other test reads or writes this variable.
        unsafe { std::env::remove_var("SPOOL_LUA_PATH") };
        assert!(
            path.starts_with("/tmp/spool-test/?.lua;"),
            "expected the extra path to be prepended, got {path}"
        );
    }
}

//! The `spool` Lua API, in both the shapes it is used in.
//!
//! [`install`] puts the typed, host-agnostic interface onto a table —
//! `spool.run` and `spool.action.*` — built on a
//! caller-supplied dispatcher. The daemon's embedded runtime (`src/lua`) hands
//! it one that queues the [`Action`] onto the action bus; [`client`] hands it
//! one that dispatches the action to a running daemon's Unix socket, and adds the
//! client-only `query_*` / `subscribe` helpers on top.
//!
//! With the `module` feature the crate additionally builds as a loadable Lua C
//! extension (`spool.so`) exposing that client table:
//!
//! ```lua
//! local spool = require("spool")
//!
//! spool.action.window.focus_east()
//! spool.action.window.focus_next()
//! spool.action.window.move_west()
//! spool.action.window.grow_width()
//! spool.action.window.toggle_floating()
//! spool.action.space.focus(12345)
//! spool.action.space.move_window({ window_id = 42, space_id = 12345 })
//! spool.action.quit()
//!
//! for _, window in ipairs(spool.query_on_screen()) do  -- actually visible
//!   print(window.app_name, window.title)
//! end
//!
//! spool.state.set("pads.term", 4213)          -- outlives reloads and restarts
//! spool.state.mutate("count", function(n) return (n or 0) + 1 end)
//!
//! spool.subscribe("window_focused", function(evt)  -- blocking; run in a helper process
//!   print(evt.title)
//! end)
//!
//! spool.subscribe(nil, function(evt) ... end)  -- nil event = every event
//! spool.subscribe({ "window_focused", "window_title_changed" }, function(evt) ... end)
//! spool.subscribe("raw_event", function(evt) print(evt.name, evt.details) end, { raw = true })
//! ```
//!
//! Every verb builds a real [`Action`]: option tables are deserialized into
//! the actual enums with mlua's serde support, so `"east"` becomes
//! [`Direction::East`] and an unknown value fails at the call site. Only the
//! host-specific extras differ (`spool.on` / `spool.bind` are embedded-only;
//! `subscribe` and the socket-path helpers are client-only).

#![allow(
    clippy::needless_pass_by_value,
    reason = "mlua callback signatures are by-value by contract"
)]

pub mod client;

use std::rc::Rc;

use mlua::{Function, Lua, LuaSerdeExt, Result, Table, Value};
use regex::Regex;
use spool_shared_types::commands::{
    Action, Direction, FocusStep, MouseMove, MoveFocus, Operation, ResizeAxis, ResizeDirection,
    parse_action,
};

/// Dispatches an [`Action`]. The only thing the two hosts differ by.
pub type Dispatch = Rc<dyn Fn(&Lua, Action) -> Result<bool>>;

const ACTION_FUNCTIONS_REGISTRY: &str = "spool.action.functions";

/// Installs the shared API onto the `spool` table, building every verb on
/// `dispatch`.
///
/// # Errors
///
/// Returns an error if any Lua table/function creation or assignment fails.
pub fn install(lua: &Lua, spool: &Table, dispatch: &Dispatch) -> Result<()> {
    // spool.run(value) — the escape hatch: an action
    // string, an argv table, or a structured action table.
    let run = {
        let dispatch = Rc::clone(dispatch);
        lua.create_function(move |lua, action: Value| dispatch(lua, to_action(lua, &action)?))?
    };
    spool.set("run", run.clone())?;

    let action = lua.create_table()?;
    action.set("window", window_table(lua, dispatch)?)?;
    action.set("space", space_table(lua, dispatch)?)?;

    let mouse = lua.create_table()?;
    mouse.set(
        "next_display",
        verb(lua, dispatch, Action::Mouse(MouseMove::ToNextDisplay))?,
    )?;
    action.set("mouse", mouse)?;

    action.set("quit", verb(lua, dispatch, Action::Quit)?)?;
    action.set("restart", verb(lua, dispatch, Action::Restart)?)?;
    action.set("print_state", verb(lua, dispatch, Action::PrintState)?)?;
    action.set(
        "mission_control",
        verb(lua, dispatch, Action::MissionControl)?,
    )?;
    action.set("show_desktop", verb(lua, dispatch, Action::ShowDesktop)?)?;
    spool.set("action", action)?;

    // spool.match{ app = …, bundle = …, title = …, floating = … }
    // builds a predicate over window records, for `ws:find`/`ws:filter`.
    // `app`, `bundle` and `title` are regexes, compiled here so a bad pattern
    // errors at the call site rather than silently matching nothing.
    spool.set("match", lua.create_function(matcher)?)?;

    Ok(())
}

/// One `spool.match{…}` call: compiles the spec into a Lua predicate.
///
/// # Errors
///
/// Returns a Lua runtime error if regex compilation fails or an unknown field is present.
pub fn matcher(lua: &Lua, spec: Table) -> Result<Function> {
    let pattern = |field: &str| -> Result<Option<Regex>> {
        let Some(pattern) = spec.get::<Option<String>>(field)? else {
            return Ok(None);
        };
        Regex::new(&pattern)
            .map(Some)
            .map_err(|err| mlua::Error::RuntimeError(format!("spool.match: {field}: {err}")))
    };
    let (app, bundle, title) = (pattern("app")?, pattern("bundle")?, pattern("title")?);
    let floating: Option<bool> = spec.get("floating")?;

    for entry in spec.pairs::<String, Value>() {
        let (key, _) = entry?;
        if !matches!(key.as_str(), "app" | "bundle" | "title" | "floating") {
            return Err(mlua::Error::RuntimeError(format!(
                "spool.match: unknown field '{key}'"
            )));
        }
    }

    lua.create_function(move |_, window: Table| {
        let matches = |regex: &Option<Regex>, fields: &[&str]| -> Result<bool> {
            let Some(regex) = regex else {
                return Ok(true);
            };
            for field in fields {
                if let Ok(val) = window.get::<String>(*field) {
                    return Ok(regex.is_match(&val));
                }
            }
            Ok(false)
        };
        let flag = |want: Option<bool>, field: &str| -> Result<bool> {
            match want {
                Some(want) => Ok(window.get::<bool>(field)? == want),
                None => Ok(true),
            }
        };
        Ok(matches(&app, &["app_name", "app"])?
            && matches(&bundle, &["bundle_id", "bundle"])?
            && matches(&title, &["title"])?
            && flag(floating, "floating")?)
    })
}

/// Builds the `spool.action.window` sub-table.
fn window_table(lua: &Lua, dispatch: &Dispatch) -> Result<Table> {
    let window = lua.create_table()?;

    for (name, operation) in [
        ("focus_west", Operation::Focus(Direction::West)),
        ("focus_east", Operation::Focus(Direction::East)),
        ("focus_north", Operation::Focus(Direction::North)),
        ("focus_south", Operation::Focus(Direction::South)),
        ("focus_first", Operation::Focus(Direction::First)),
        ("focus_last", Operation::Focus(Direction::Last)),
        ("focus_next", Operation::FocusStep(FocusStep::Next)),
        ("focus_previous", Operation::FocusStep(FocusStep::Previous)),
        ("focus_tiled", Operation::FocusTiled),
        ("focus_floating", Operation::FocusFloating),
        ("focus_other_layer", Operation::FocusOtherLayer),
        ("move_west", Operation::Move(Direction::West)),
        ("move_east", Operation::Move(Direction::East)),
        ("move_north", Operation::Move(Direction::North)),
        ("move_south", Operation::Move(Direction::South)),
        ("move_first", Operation::Move(Direction::First)),
        ("move_last", Operation::Move(Direction::Last)),
        (
            "shrink_width",
            Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Shrink,
            },
        ),
        (
            "grow_width",
            Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Grow,
            },
        ),
        (
            "shrink_height",
            Operation::Resize {
                axis: ResizeAxis::Height,
                direction: ResizeDirection::Shrink,
            },
        ),
        (
            "grow_height",
            Operation::Resize {
                axis: ResizeAxis::Height,
                direction: ResizeDirection::Grow,
            },
        ),
        ("center", Operation::Center),
        ("maximize", Operation::Maximize),
        ("snap", Operation::Snap),
        ("toggle_floating", Operation::ToggleFloating),
        ("toggle_stack", Operation::ToggleStack),
        ("equalize", Operation::Equalize),
        ("balance", Operation::Balance),
        ("next_display", Operation::ToNextDisplay(MoveFocus::Follow)),
        (
            "next_display_send",
            Operation::ToNextDisplay(MoveFocus::Stay),
        ),
    ] {
        window.set(name, verb(lua, dispatch, Action::Window(operation))?)?;
    }

    Ok(window)
}

/// Builds the stable-ID `spool.action.space` sub-table.
fn space_table(lua: &Lua, dispatch: &Dispatch) -> Result<Table> {
    let space = lua.create_table()?;

    let focus = {
        let dispatch = Rc::clone(dispatch);
        lua.create_function(move |lua, space_id: u64| {
            dispatch(lua, Action::FocusSpace { space_id })
        })?
    };
    space.set("focus", focus)?;

    let move_window = {
        let dispatch = Rc::clone(dispatch);
        lua.create_function(move |lua, opts: Table| {
            let move_focus = if opts.get::<Option<bool>>("follow")?.unwrap_or(false) {
                MoveFocus::Follow
            } else {
                MoveFocus::Stay
            };
            dispatch(
                lua,
                Action::MoveWindowToSpace {
                    window_id: opts.get("window_id")?,
                    space_id: opts.get("space_id")?,
                    move_focus,
                },
            )
        })?
    };
    space.set("move_window", move_window)?;

    let create = {
        let dispatch = Rc::clone(dispatch);
        lua.create_function(move |lua, display_id: u32| {
            dispatch(lua, Action::CreateSpace { display_id })
        })?
    };
    space.set("create", create)?;

    let delete = {
        let dispatch = Rc::clone(dispatch);
        lua.create_function(move |lua, space_id: u64| {
            dispatch(lua, Action::DeleteSpace { space_id })
        })?
    };
    space.set("delete", delete)?;

    Ok(space)
}

/// A zero-argument verb dispatching a fixed action.
fn verb(lua: &Lua, dispatch: &Dispatch, action: Action) -> Result<Function> {
    let dispatch = Rc::clone(dispatch);
    // Embedded keybind handlers receive a WindowSet argument. Fixed action
    // functions deliberately ignore it, while direct calls simply pass nil.
    let dispatched = action.clone();
    let function = lua.create_function(move |lua, _: Value| dispatch(lua, dispatched.clone()))?;
    let functions = if let Ok(table) = lua.named_registry_value::<Table>(ACTION_FUNCTIONS_REGISTRY)
    {
        table
    } else {
        let table = lua.create_table()?;
        lua.set_named_registry_value(ACTION_FUNCTIONS_REGISTRY, table.clone())?;
        table
    };
    functions.set(function.clone(), lua.to_value(&action)?)?;
    Ok(function)
}

/// Returns the typed action represented by one of the fixed functions under
/// `spool.action`, or `None` for an ordinary user callback.
///
/// # Errors
///
/// Returns an error if the Lua registry entry cannot be read or deserialized.
pub fn action_for_function(lua: &Lua, function: &Function) -> Result<Option<Action>> {
    let Ok(functions) = lua.named_registry_value::<Table>(ACTION_FUNCTIONS_REGISTRY) else {
        return Ok(None);
    };
    let value = functions.get::<Value>(function.clone())?;
    match value {
        Value::Nil => Ok(None),
        value => lua.from_value(value).map(Some),
    }
}

/// Converts a `spool.run` argument into an [`Action`]: an action string, an
/// argv table, or a structured table deserialized straight into the enums.
fn to_action(lua: &Lua, value: &Value) -> Result<Action> {
    let parse = |argv: &[String]| {
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        parse_action(&borrowed).map_err(|err| mlua::Error::RuntimeError(err.to_string()))
    };

    match value {
        Value::String(action) => {
            let action = action.to_str()?;
            let argv: Vec<String> = action.split_whitespace().map(str::to_string).collect();
            if argv.is_empty() {
                return Err(mlua::Error::RuntimeError("empty action".into()));
            }
            parse(&argv)
        }
        // A sequence is argv (`{"window", "focus", "east"}`); any other table is
        // a structured action (`{ window = { focus = "east" } }`).
        Value::Table(table) if table.raw_len() > 0 => {
            let argv: Vec<String> = table
                .clone()
                .sequence_values::<Value>()
                .map(|entry| scalar_token(&entry?))
                .collect::<Result<_>>()?;
            parse(&argv)
        }
        Value::Table(_) => lua.from_value(value.clone()),
        other => Err(mlua::Error::RuntimeError(format!(
            "action must be a string, argv table or action table, got {}",
            other.type_name()
        ))),
    }
}

/// Stringifies an argv element (integers without a decimal point, so `3` becomes
/// `"3"` and not `"3.0"`).
fn scalar_token(value: &Value) -> Result<String> {
    match value {
        Value::String(string) => Ok(string.to_str()?.to_string()),
        Value::Integer(number) => Ok(number.to_string()),
        Value::Number(number) => Ok(format!("{number}")),
        other => Err(mlua::Error::RuntimeError(format!(
            "action arguments must be strings or numbers, got {}",
            other.type_name()
        ))),
    }
}

/// The loadable-module entry point: `require("spool")` calls `luaopen_spool`,
/// which returns the client table. Only compiled for the `module` feature —
/// the daemon depends on this crate as a plain library.
#[cfg(feature = "module")]
#[mlua::lua_module]
fn spool(lua: &Lua) -> Result<Table> {
    client::module(lua, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Installs the interface with a dispatcher that records actions instead of
    /// issuing them, and runs `source`.
    fn run(source: &str) -> mlua::Result<Vec<Action>> {
        let lua = Lua::new();
        let issued = Rc::new(RefCell::new(Vec::new()));
        let spool = lua.create_table()?;
        lua.globals().set("spool", spool.clone())?;

        let recorder = {
            let issued = Rc::clone(&issued);
            move |_: &Lua, action: Action| {
                issued.borrow_mut().push(action);
                Ok(true)
            }
        };
        install(&lua, &spool, &(Rc::new(recorder) as Dispatch))?;
        lua.load(source).exec()?;

        let commands = issued.borrow().clone();
        Ok(commands)
    }

    fn debug(commands: &[Action]) -> Vec<String> {
        commands.iter().map(|c| format!("{c:?}")).collect()
    }

    #[test]
    fn typed_verbs_build_typed_actions() {
        let commands = run(r"
            spool.action.window.focus_east()
            spool.action.window.focus_next()
            spool.action.window.shrink_height()
            spool.action.window.balance()
            spool.action.space.move_window({ window_id = 42, space_id = 123, follow = false })
        ")
        .unwrap();

        assert_eq!(
            debug(&commands),
            debug(&[
                Action::Window(Operation::Focus(Direction::East)),
                Action::Window(Operation::FocusStep(FocusStep::Next)),
                Action::Window(Operation::Resize {
                    axis: ResizeAxis::Height,
                    direction: ResizeDirection::Shrink,
                }),
                Action::Window(Operation::Balance),
                Action::MoveWindowToSpace {
                    window_id: 42,
                    space_id: 123,
                    move_focus: MoveFocus::Stay,
                },
            ])
        );
    }

    #[test]
    fn fixed_action_functions_are_first_class_values() {
        let actions = run(r"
            local action = spool.action.window.focus_previous
            action()
        ")
        .unwrap();

        assert_eq!(debug(&actions), vec!["Window(FocusStep(Previous))"]);
    }

    #[test]
    fn system_overview_verbs_and_strings_dispatch_the_same_actions() {
        let actions = run(r#"
            spool.action.mission_control()
            spool.action.show_desktop()
            spool.run("mission-control")
            spool.run({ "show-desktop" })
        "#)
        .unwrap();
        assert_eq!(
            actions,
            vec![
                Action::MissionControl,
                Action::ShowDesktop,
                Action::MissionControl,
                Action::ShowDesktop,
            ]
        );
    }

    #[test]
    fn run_accepts_strings_argv_and_action_tables() {
        let commands = run(r#"
            spool.run("window focus east")
            spool.run({ "window", "focus", 3 })
            spool.run({ window = { focus = "east" } })
        "#)
        .unwrap();

        assert_eq!(
            debug(&commands),
            debug(&[
                Action::Window(Operation::Focus(Direction::East)),
                Action::Window(Operation::Focus(Direction::Nth(2))),
                Action::Window(Operation::Focus(Direction::East)),
            ])
        );
    }

    #[test]
    fn bad_arguments_fail_at_the_call_site() {
        assert!(run("spool.window.focus_east()").is_err());
        assert!(run("spool.actions.window.focus_east()").is_err());
        assert!(run("spool.action.window.manage()").is_err());
        assert!(run("spool.match({ managed = true })").is_err());
        assert!(run(r#"spool.run("not an action")"#).is_err());
    }

    #[test]
    fn fixed_display_actions_encode_follow_behaviour() {
        let commands = run(r"
            spool.action.window.next_display()
            spool.action.window.next_display_send()
        ")
        .unwrap();

        assert_eq!(
            debug(&commands),
            debug(&[
                Action::Window(Operation::ToNextDisplay(MoveFocus::Follow)),
                Action::Window(Operation::ToNextDisplay(MoveFocus::Stay)),
            ])
        );
    }
}

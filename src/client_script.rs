//! Runs an on-demand Lua client in the invoking CLI process.
//!
//! This deliberately does not evaluate code in the daemon's configuration
//! runtime. A stuck or stateful client script therefore affects only its own
//! process, while every `spool.*` operation still crosses the authenticated
//! Unix socket and uses the daemon's typed interfaces.

use std::io::Read;
use std::path::PathBuf;

use mlua::{Lua, Table};

use crate::errors::{Error, Result};

/// Where an on-demand client script obtains its source code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptSource {
    Inline(String),
    File(PathBuf),
    Stdin,
}

impl ScriptSource {
    fn read(self) -> Result<(String, String)> {
        match self {
            Self::Inline(source) => Ok((source, "-e".to_string())),
            Self::File(path) => {
                let source = std::fs::read_to_string(&path)?;
                let name = path.display().to_string();
                Ok((source, name))
            }
            Self::Stdin => {
                let mut source = String::new();
                std::io::stdin().read_to_string(&mut source)?;
                Ok((source, "-".to_string()))
            }
        }
    }
}

/// Executes one isolated client script to completion.
///
/// # Errors
///
/// Returns file/stdin errors, Lua load or execution errors, and any error a
/// `spool.*` client call raises while talking to the daemon.
pub fn run(source: ScriptSource, args: Vec<String>) -> Result<()> {
    let (source, name) = source.read()?;
    execute(&source, &name, &args)
}

fn execute(source: &str, name: &str, args: &[String]) -> Result<()> {
    // SAFETY: this interpreter runs only code the invoking user explicitly
    // supplied, is confined to this short-lived CLI process, and must support
    // ordinary Lua modules. It never shares globals with the daemon runtime.
    let lua = unsafe { Lua::unsafe_new() };
    install_client_module(&lua)?;
    install_args(&lua, name, args)?;
    lua.load(source).set_name(name).exec().map_err(script_error)
}

fn install_client_module(lua: &Lua) -> Result<()> {
    let spool = spool_lua::client::module(lua, crate::VERSION_STRING).map_err(script_error)?;
    lua.globals()
        .set("spool", spool.clone())
        .map_err(script_error)?;

    // `spool` is available directly, like it is in init.lua, and through
    // `require("spool")`, like it is to an external client script.
    let package: Table = lua.globals().get("package").map_err(script_error)?;
    let loaded: Table = package.get("loaded").map_err(script_error)?;
    loaded.set("spool", spool).map_err(script_error)
}

fn install_args(lua: &Lua, name: &str, args: &[String]) -> Result<()> {
    let table = lua.create_table().map_err(script_error)?;
    table.raw_set(0, name).map_err(script_error)?;
    for (index, value) in args.iter().enumerate() {
        table
            .raw_set(index + 1, value.as_str())
            .map_err(script_error)?;
    }
    lua.globals().set("arg", table).map_err(script_error)
}

fn script_error(error: mlua::Error) -> Error {
    Error::Generic(format!("Lua script: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spool_local_ipc::Server;
    use spool_shared_types::script_value::ScriptValue;
    use spool_shared_types::wire::{Request, Response, ScriptStateRequest, ScriptStateResponse};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SERVICE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn client_script_gets_spool_module_and_conventional_args() {
        execute(
            r#"
                assert(spool == require("spool"))
                assert(type(spool.query_state) == "function")
                assert(type(spool.state.get) == "function")
                assert(type(spool.action.window.balance) == "function")
                assert(arg[0] == "-e")
                assert(arg[1] == "first")
                assert(arg[2] == "second")
                assert(arg[3] == nil)
            "#,
            "-e",
            &["first".into(), "second".into()],
        )
        .unwrap();
    }

    #[test]
    fn client_script_errors_include_the_chunk_name_and_traceback() {
        let error = execute(
            "local function fail() error('broken') end; fail()",
            "job.lua",
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("job.lua"), "unexpected error: {error}");
        assert!(error.contains("broken"), "unexpected error: {error}");
    }

    #[test]
    fn client_script_reads_script_state_through_the_daemon_protocol() {
        let service = format!(
            "com.wxxxcxx.spool.client-script.{}.{}",
            std::process::id(),
            NEXT_SERVICE.fetch_add(1, Ordering::Relaxed)
        );
        let server = Server::bind(&service).unwrap();
        let source = format!(
            r#"
                spool.set_service_name({service:?})
                assert(spool.state.get("answer") == 42)
            "#
        );
        let client = std::thread::spawn(move || execute(&source, "state.lua", &[]));

        let delivery = server.recv_blocking().unwrap();
        assert_eq!(
            delivery.request,
            Request::ScriptState(ScriptStateRequest::Get {
                key: "answer".into()
            })
        );
        delivery
            .reply
            .unwrap()
            .send(&Response::ScriptState(ScriptStateResponse::Value(Some(
                ScriptValue::Int(42),
            ))))
            .unwrap();

        client.join().unwrap().unwrap();
        execute(
            &format!(
                "spool.set_service_name({:?})",
                spool_shared_types::wire::SERVICE_NAME
            ),
            "reset-service.lua",
            &[],
        )
        .unwrap();
    }
}

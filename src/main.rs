#![allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system and mlua callback signatures are by-value by contract"
)]

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

#[cfg(feature = "lua")]
use clap::Args;
use clap::{Parser, Subcommand};
use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod accessibility_prompt;
mod client;
#[cfg(feature = "lua")]
mod client_script;
mod commands;
mod config;
mod ecs;
mod errors;
mod events;
#[cfg(feature = "lua")]
mod lua;
mod manager;
mod menubar;
mod overlay;
mod platform;
mod reader;
mod util;

#[cfg(test)]
mod tests;

embed_plist::embed_info_plist!("../assets/Info.plist");

use events::{Event, EventSender};

use crate::ecs::setup_bevy_app;
use crate::manager::{check_ax_privilege, request_ax_privilege};
use crate::menubar::MenuBarManager;
use crate::platform::PlatformCallbacks;
use accessibility_prompt::{AccessibilitySetupAction, show_accessibility_setup};
use client::ClientCommand;
use ecs::state::StateQueryKind;
use errors::Result;
use platform::service;
use reader::CommandReader;

#[cfg(feature = "lua")]
pub const VERSION_STRING: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("SPOOL_LUA_VERSION"),
    ")"
);
#[cfg(not(feature = "lua"))]
pub const VERSION_STRING: &str = concat!(env!("CARGO_PKG_VERSION"));

/// `Spool` is the main command-line interface structure for the window manager.
/// It defines the available subcommands for controlling the Spool daemon.
#[derive(Clone, Debug, Default, Parser)]
#[command(
    version = VERSION_STRING,
    author = clap::crate_authors!(),
    about = clap::crate_description!(),
)]
pub struct Spool {
    /// The subcommand to execute (e.g., `launch`, `install`, `send-cmd`).
    #[clap(subcommand)]
    subcmd: Option<SubCmd>,
}

/// `SubCmd` enumerates the available command-line subcommands for `spool`.
/// These subcommands allow users to launch the daemon, install/uninstall it as a service,
/// install/uninstall its app launcher, start/stop/restart the service, or send commands to
/// a running daemon.
#[derive(Clone, Debug, Default, Subcommand)]
pub enum SubCmd {
    /// Launches the `spool` daemon directly in the console (default behavior).
    #[default]
    Launch,

    /// Installs the `spool` daemon as a background service.
    Install,

    /// Uninstalls the `spool` background service.
    Uninstall,

    /// Reinstalls the `spool` background service.
    Reinstall,

    /// Installs a Spool app launcher to `~/Applications`.
    InstallApp,

    /// Uninstalls the Spool app launcher from `~/Applications`.
    UninstallApp,

    /// Starts the `spool` background service.
    Start,

    /// Stops the `spool` background service.
    Stop,

    /// Restarts the `spool` background service.
    Restart,

    /// Sends a command via a Unix socket to the running `spool` daemon.
    SendCmd {
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },

    /// Queries structured state from the running daemon.
    Query {
        #[clap(subcommand)]
        query: QueryCmd,
    },

    /// Subscribes to structured state events from the running daemon.
    Subscribe {
        /// Emits complete line-delimited JSON instead of the default TSV summary.
        #[arg(long)]
        json: bool,
    },

    /// Runs an isolated Lua client script against the running daemon.
    #[cfg(feature = "lua")]
    Script(ScriptCmd),

    /// Inspects or applies the one-time v2-to-v3 Space state migration.
    MigrateState {
        /// State file to inspect; defaults to Spool's normal state path.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Write the v3 file after creating the adjacent v2 backup.
        #[arg(long)]
        apply: bool,
    },
}

#[cfg(feature = "lua")]
#[derive(Clone, Debug, Args)]
pub struct ScriptCmd {
    /// Executes the supplied Lua source instead of reading a file.
    #[arg(short = 'e', long, value_name = "CODE", conflicts_with = "file")]
    eval: Option<String>,

    /// Lua file to execute; use `-` to read the script from standard input.
    #[arg(value_name = "FILE", required_unless_present = "eval")]
    file: Option<PathBuf>,

    /// Values exposed to Lua as `arg[1]`, `arg[2]`, and so on.
    #[arg(last = true, value_name = "ARG")]
    args: Vec<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum QueryCmd {
    /// Prints the complete state document.
    State {
        /// Emits complete JSON instead of the default TSV summary.
        #[arg(long)]
        json: bool,
    },
    /// Prints native macOS Spaces and their tracked windows.
    Spaces {
        /// Emits complete JSON instead of the default TSV summary.
        #[arg(long)]
        json: bool,
    },
    /// Prints the active focus/workspace state.
    Active {
        /// Emits complete JSON instead of the default TSV summary.
        #[arg(long)]
        json: bool,
    },
    /// Prints the windows currently visible on screen, slivers excluded.
    OnScreen {
        /// Emits complete JSON instead of the default TSV summary.
        #[arg(long)]
        json: bool,
    },
}

/// The main entry point of the `spool` application.
/// It sets up logging and dispatches commands accordingly.
///
/// # Returns
///
/// `Ok(())` if the application runs successfully, otherwise `Err(Error)`.
fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            fmt::layer()
                .with_level(true)
                .with_line_number(true)
                .with_file(true)
                .with_target(true)
                .with_thread_ids(false)
                .with_writer(std::io::stderr)
                .compact(),
        )
        .init();

    let service = || service::Service::try_new(service::ID);

    let subcmd = Spool::parse().subcmd.unwrap_or_default();
    maybe_warn_deprecated_options_for_service(&subcmd);

    match subcmd {
        SubCmd::Launch => {
            let (sender, receiver) = EventSender::new();
            let sender_c = sender.clone();
            // bevy's `TerminalCtrlCHandlerPlugin` was not fast enough. maybe because of its use of `Relaxed` atomic variable?
            ctrlc::set_handler(move || {
                let _ = sender_c.send(events::Event::Exit); // just drop the err. we are exiting anyway.
            })
            .expect("setting Ctrl-C handler should succeed");
            let _command_reader = CommandReader::new(sender.clone()).start()?;
            if !check_ax_privilege() && !wait_for_accessibility(sender.clone(), &receiver) {
                return Ok(());
            }
            match setup_bevy_app(sender, receiver) {
                Ok(mut app) => {
                    app.run();
                }
                Err(err) => {
                    error!(
                        "Error launching Spool: {err}.\nStopping the service for now. You can restart it again with 'spool restart'."
                    );
                    service()?.stop()?;
                }
            }
        }
        SubCmd::Install => service()?.install()?,
        SubCmd::Uninstall => service()?.uninstall()?,
        SubCmd::Reinstall => service()?.reinstall()?,
        SubCmd::InstallApp => platform::app_launcher::AppLauncher::try_new()?.install()?,
        SubCmd::UninstallApp => platform::app_launcher::AppLauncher::try_new()?.uninstall()?,
        SubCmd::Start => service()?.start()?,
        SubCmd::Stop => service()?.stop()?,
        SubCmd::Restart => service()?.restart()?,
        SubCmd::SendCmd { cmd } => client::run(ClientCommand::Send(cmd))?,
        SubCmd::Query { query } => {
            let (kind, format) = query.request();
            client::run(ClientCommand::Query { kind, format })?;
        }
        SubCmd::Subscribe { json } => client::run(ClientCommand::Subscribe(
            client::OutputFormat::from_json(json),
        ))?,
        #[cfg(feature = "lua")]
        SubCmd::Script(script) => {
            let (source, args) = script.request();
            client_script::run(source, args)?;
        }
        SubCmd::MigrateState { path, apply } => {
            let path = path.unwrap_or_else(ecs::state::SpoolState::default_state_file_path);
            let report = ecs::state::SpoolState::migrate_file(&path, apply)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn wait_for_accessibility(sender: EventSender, receiver: &Receiver<Event>) -> bool {
    let mut platform_callbacks = PlatformCallbacks::new(sender.clone());
    let _menu_bar =
        MenuBarManager::new_accessibility_required(platform_callbacks.main_thread_marker, sender);

    if show_accessibility_setup(platform_callbacks.main_thread_marker)
        == AccessibilitySetupAction::Continue
    {
        request_ax_privilege();
    }

    warn!(
        "Accessibility access is required. Spool will remain in the menu bar and start automatically once access is granted."
    );

    loop {
        platform_callbacks.pump_cocoa_event_loop(1.0);

        if check_ax_privilege() {
            return true;
        }

        match receiver.try_recv() {
            Ok(
                Event::Exit
                | Event::Command {
                    command: commands::Command::Quit,
                },
            )
            | Err(TryRecvError::Disconnected) => return false,
            Ok(event) => warn!(
                ?event,
                "ignoring event while waiting for Accessibility access"
            ),
            Err(TryRecvError::Empty) => {}
        }
    }
}

impl QueryCmd {
    fn request(&self) -> (StateQueryKind, client::OutputFormat) {
        match self {
            QueryCmd::State { json } => (
                StateQueryKind::State,
                client::OutputFormat::from_json(*json),
            ),
            QueryCmd::Spaces { json } => (
                StateQueryKind::Spaces,
                client::OutputFormat::from_json(*json),
            ),
            QueryCmd::Active { json } => (
                StateQueryKind::Active,
                client::OutputFormat::from_json(*json),
            ),
            QueryCmd::OnScreen { json } => (
                StateQueryKind::OnScreen,
                client::OutputFormat::from_json(*json),
            ),
        }
    }
}

#[cfg(feature = "lua")]
impl ScriptCmd {
    fn request(self) -> (client_script::ScriptSource, Vec<String>) {
        let source = match (self.eval, self.file) {
            (Some(source), None) => client_script::ScriptSource::Inline(source),
            (None, Some(path)) if path.as_os_str() == "-" => client_script::ScriptSource::Stdin,
            (None, Some(path)) => client_script::ScriptSource::File(path),
            // Clap enforces exactly one source before this point.
            _ => unreachable!("script source must be validated by clap"),
        };
        (source, self.args)
    }
}

fn should_check_deprecated_options(subcmd: &SubCmd) -> bool {
    matches!(
        subcmd,
        SubCmd::Install | SubCmd::Uninstall | SubCmd::Start | SubCmd::Stop | SubCmd::Restart
    )
}

fn maybe_warn_deprecated_options_for_service(subcmd: &SubCmd) {
    if !should_check_deprecated_options(subcmd) {
        return;
    }

    // An init.lua disables the TOML entirely, so its contents — deprecated keys
    // included — are never read. Warning about them would be noise.
    #[cfg(feature = "lua")]
    if config::discover_lua_file().is_some() {
        return;
    }

    let Some(path) = config::discover_configuration_file() else {
        return;
    };

    match config::deprecated_options_in_file(&path) {
        Ok(keys) if !keys.is_empty() => {
            warn!(
                "detected deprecated [options] keys in `{}` while running a service command: {}. \
                 Please migrate to `[padding]`, `[swipe]`, and `[decorations.*]`.",
                path.display(),
                keys.join(", ")
            );
        }
        Ok(_) => {}
        Err(err) => {
            warn!(
                "could not inspect `{}` for deprecated options: {err}",
                path.display()
            );
        }
    }
}

#[cfg(all(test, feature = "lua"))]
mod cli_tests {
    use super::*;

    #[test]
    fn script_accepts_inline_source_and_arguments_after_separator() {
        let cli = Spool::try_parse_from(["spool", "script", "-e", "print(arg[1])", "--", "hello"])
            .unwrap();
        let Some(SubCmd::Script(script)) = cli.subcmd else {
            panic!("expected script command");
        };
        assert_eq!(
            script.request(),
            (
                client_script::ScriptSource::Inline("print(arg[1])".into()),
                vec!["hello".into()]
            )
        );
    }

    #[test]
    fn script_dash_means_standard_input() {
        let cli = Spool::try_parse_from(["spool", "script", "-"]).unwrap();
        let Some(SubCmd::Script(script)) = cli.subcmd else {
            panic!("expected script command");
        };
        assert_eq!(
            script.request(),
            (client_script::ScriptSource::Stdin, Vec::new())
        );
    }

    #[test]
    fn removed_state_command_is_not_accepted() {
        assert!(Spool::try_parse_from(["spool", "state", "get", "key"]).is_err());
    }
}

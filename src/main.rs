#![allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system and mlua callback signatures are by-value by contract"
)]

#[cfg(feature = "lua")]
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

#[cfg(feature = "lua")]
use clap::Args;

use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod accessibility_prompt;
mod bar;
mod cli;
mod client;
#[cfg(feature = "lua")]
mod client_script;
mod commands;
mod config;
mod ecs;
mod errors;
mod events;
mod inspection;
mod lifecycle;
mod logs;
#[cfg(feature = "lua")]
mod lua;
mod manager;
mod menubar;
mod overlay;
mod platform;
mod reader;
mod util;
mod window_policy;

#[cfg(test)]
mod tests;

embed_plist::embed_info_plist!("../assets/Info.plist");

use events::{Event, EventSender};

use crate::ecs::setup_bevy_app;
use crate::manager::{check_ax_privilege, request_ax_privilege};
use crate::menubar::MenuBarManager;
use crate::platform::PlatformCallbacks;
use accessibility_prompt::{AccessibilitySetupAction, show_accessibility_setup};
use errors::Result;
use platform::service;
use reader::RequestReader;

#[cfg(feature = "lua")]
pub const VERSION_STRING: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("SPOOL_LUA_VERSION"),
    ")"
);
#[cfg(not(feature = "lua"))]
pub const VERSION_STRING: &str = concat!(env!("CARGO_PKG_VERSION"));

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

/// The main entry point of the `spool` application.
/// It sets up logging and handles the selected subcommand.
///
/// # Returns
///
/// `Ok(())` if the application runs successfully, otherwise `Err(Error)`.
fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
fn run() -> Result<u8> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == inspection::WORKER_ARGUMENT)
    {
        inspection::worker_main()?;
        return Ok(0);
    }
    let invocation = cli::parse_from(std::env::args_os()).unwrap_or_else(|error| error.exit());
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

    match invocation {
        cli::Invocation::Service(cli::ServiceCommand::Run) => {
            let (sender, receiver) = EventSender::new();
            let sender_c = sender.clone();
            // bevy's `TerminalCtrlCHandlerPlugin` was not fast enough. maybe because of its use of `Relaxed` atomic variable?
            ctrlc::set_handler(move || {
                let _ = sender_c.send(events::Event::Exit); // just drop the err. we are exiting anyway.
            })
            .expect("setting Ctrl-C handler should succeed");
            let lifecycle = lifecycle::Lifecycle::starting();
            let _command_reader =
                RequestReader::with_lifecycle(sender.clone(), lifecycle.clone()).start()?;
            if lifecycle.phase() == lifecycle::Phase::Stopping {
                return Ok(0);
            }
            if !check_ax_privilege() {
                lifecycle.set(lifecycle::Phase::WaitingForPermission);
                if !wait_for_accessibility(sender.clone(), &receiver) {
                    return Ok(0);
                }
            }
            match setup_bevy_app(sender, receiver) {
                Ok(mut app) => {
                    app.insert_resource(lifecycle.clone());
                    lifecycle.set(lifecycle::Phase::Running);
                    if lifecycle.phase() != lifecycle::Phase::Stopping {
                        app.run();
                    }
                    lifecycle.set(lifecycle::Phase::Stopping);
                }
                Err(err) => {
                    error!(
                        "Error launching Spool: {err}.\nStopping the service for now. You can restart it again with 'spool service restart'."
                    );
                    service()?.stop()?;
                }
            }
        }
        cli::Invocation::Help(help) => print!("{help}"),
        cli::Invocation::Service(command) => match command {
            cli::ServiceCommand::Run => unreachable!(),
            cli::ServiceCommand::Install => service()?.install()?,
            cli::ServiceCommand::Uninstall => service()?.uninstall()?,
            cli::ServiceCommand::Reinstall => service()?.reinstall()?,
            cli::ServiceCommand::Start => service()?.start()?,
            cli::ServiceCommand::Stop => service()?.stop()?,
            cli::ServiceCommand::Restart => service()?.restart()?,
            cli::ServiceCommand::Logs(args) => logs::run(&service()?, &args)?,
            cli::ServiceCommand::MigrateState { path, apply } => {
                let path = path.unwrap_or_else(ecs::state::SpoolState::default_state_file_path);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ecs::state::SpoolState::migrate_file(
                        &path, apply
                    )?)?
                );
            }
        },
        cli::Invocation::Launcher { install } => {
            let launcher = platform::app_launcher::AppLauncher::try_new()?;
            if install {
                launcher.install()?;
            } else {
                launcher.uninstall()?;
            }
        }
        cli::Invocation::Action {
            action,
            timeout_ms,
            json,
        } => return client::checked_action(action, timeout_ms, json),
        cli::Invocation::Read { request, json } => {
            let report = if request.source == spool_shared_types::inspection::Source::Native {
                let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let signal = cancelled.clone();
                ctrlc::set_handler(move || {
                    signal.store(true, std::sync::atomic::Ordering::Release);
                })
                .map_err(|error| errors::Error::Generic(error.to_string()))?;
                inspection::native::collect(request, &cancelled)
            } else {
                client::inspect(request)
            };
            client::print_report(&report, json)?;
            return Ok(report.exit_code());
        }
        cli::Invocation::Watch {
            json,
            raw,
            timeout_ms,
        } => client::subscribe(client::OutputFormat::from_json(json), raw, timeout_ms)?,
        #[cfg(feature = "lua")]
        cli::Invocation::Script(script) => {
            let (source, args) = script.request();
            client_script::run(source, args)?;
        }
    }
    Ok(0)
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
                | Event::ActionRequested {
                    action: commands::Action::Quit,
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

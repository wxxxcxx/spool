//! Resource-oriented public command grammar.

use clap::{Arg, ArgAction, ArgMatches, Args, Command, FromArgMatches};
use spool_shared_types::{
    commands::Action,
    inspection::{Filter, ReadMode, ReadRequest, Resource, Source},
};
use std::ffi::OsString;

#[derive(Debug)]
pub(crate) enum Invocation {
    Help(String),
    Service(ServiceCommand),
    Launcher {
        install: bool,
    },
    Action {
        action: Action,
        timeout_ms: u64,
        json: bool,
    },
    Read {
        request: ReadRequest,
        json: bool,
    },
    Watch {
        json: bool,
        raw: bool,
        timeout_ms: u64,
    },
    #[cfg(feature = "lua")]
    Script(crate::ScriptCmd),
}

#[derive(Debug)]
pub(crate) enum ServiceCommand {
    Run,
    Start,
    Stop,
    Restart,
    Install,
    Uninstall,
    Reinstall,
    Logs(crate::logs::LogArgs),
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}
fn option(name: &'static str) -> Arg {
    Arg::new(name).long(name).num_args(1)
}
fn positional(name: &'static str) -> Arg {
    Arg::new(name).required(true)
}
fn control(command: Command) -> Command {
    command
        .arg(flag("json"))
        .arg(option("timeout").default_value("5s").value_parser(timeout))
}
fn target(command: Command) -> Command {
    control(command.arg(option("window")))
}
fn read(command: Command, detail: bool) -> Command {
    let command = control(command).arg(
        option("source")
            .value_parser(["spool", "native"])
            .default_value("spool"),
    );
    if detail {
        command.arg(
            option("show")
                .action(ArgAction::Append)
                .value_delimiter(','),
        )
    } else {
        command
    }
}
fn inventory(name: &'static str, filters: &[&'static str]) -> Command {
    let mut list = read(
        Command::new("list").about("List resource summaries; repeated filter values are OR"),
        false,
    );
    for field in filters {
        list = list.arg(option(field).action(ArgAction::Append));
    }
    Command::new(name).subcommand(list).subcommand(read(
        Command::new("inspect")
            .about("Inspect a resource; --show replaces the default fields")
            .arg(positional("id").value_parser(clap::value_parser!(u64))),
        true,
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(crate) fn command() -> Command {
    let window = inventory(
        "window",
        &[
            "pid",
            "bundle-id",
            "space",
            "display",
            "title",
            "on-screen",
            "minimized",
        ],
    )
    .subcommand(control(
        Command::new("focus")
            .about("Focus a native window ID or direction; --nth uses navigable layout order")
            .arg(
                Arg::new("selector")
                    .required_unless_present("nth")
                    .conflicts_with("nth"),
            )
            .arg(option("nth"))
            .arg(option("space")),
    ))
    .subcommand(target(Command::new("move").arg(positional("direction"))))
    .subcommand(target(Command::new("center")))
    .subcommand(target(Command::new("grow").arg(positional("axis"))))
    .subcommand(target(Command::new("shrink").arg(positional("axis"))))
    .subcommand(target(Command::new("maximize")))
    .subcommand(target(Command::new("snap")))
    .subcommand(
        Command::new("toggle")
            .subcommand(target(Command::new("floating")))
            .subcommand(target(Command::new("stack"))),
    )
    .subcommand(target(
        Command::new("move-to-space")
            .arg(positional("destination"))
            .arg(flag("follow"))
            .arg(flag("stay")),
    ))
    .subcommand(target(
        Command::new("move-to-display")
            .arg(positional("destination"))
            .arg(flag("follow"))
            .arg(flag("stay")),
    ))
    .subcommand(control(Command::new("reconcile")));
    let layout = Command::new("layout")
        .about(
            "Spool layout owned by a Space; column ordinals include retained unavailable columns",
        )
        .subcommand(read(
            Command::new("inspect").arg(
                Arg::new("id")
                    .long("space")
                    .value_parser(clap::value_parser!(u64)),
            ),
            true,
        ))
        .subcommand(control(
            Command::new("width")
                .about("Set column slot width in points, a viewport percentage, or inherit")
                .arg(positional("value"))
                .arg(option("column").required(true))
                .arg(option("space")),
        ))
        .subcommand(control(
            Command::new("equalize")
                .arg(option("space"))
                .arg(option("column")),
        ))
        .subcommand(control(
            Command::new("balance")
                .arg(option("space"))
                .arg(option("reference-column")),
        ))
        .subcommand(Command::new("toggle").subcommand(control(
            Command::new("tiled-visibility").arg(option("space")),
        )));
    let space = inventory("space", &["display", "kind", "visible"])
        .subcommand(control(
            Command::new("prefer-focus")
                .about("Retain a Space focus preference without activating it")
                .arg(positional("id"))
                .arg(option("window").required(true)),
        ))
        .subcommand(control(Command::new("focus").arg(positional("id"))))
        .subcommand(control(
            Command::new("create").arg(option("display").required(true)),
        ))
        .subcommand(control(Command::new("delete").arg(positional("id"))))
        .subcommand(layout);
    let mut service = Command::new("service").about("Run and manage the Spool service");
    for verb in [
        "run",
        "start",
        "stop",
        "restart",
        "install",
        "uninstall",
        "reinstall",
    ] {
        service = service.subcommand(Command::new(verb));
    }
    service = service
        .subcommand(control(Command::new("quit")))
        .subcommand(control(Command::new("dump-state")))
        .subcommand(crate::logs::LogArgs::augment_args(Command::new("logs")));
    let root = Command::new("spool")
        .version(crate::VERSION_STRING)
        .about(clap::crate_description!())
        .subcommand(window)
        .subcommand(space)
        .subcommand(inventory("display", &["name", "main"]))
        .subcommand(inventory("app", &["pid", "bundle-id", "name", "hidden"]))
        .subcommand(
            Command::new("session")
                .subcommand(read(Command::new("inspect"), true))
                .subcommand(control(Command::new("watch").arg(flag("raw"))))
                .subcommand(control(Command::new("mission-control")))
                .subcommand(control(Command::new("show-desktop"))),
        )
        .subcommand(service)
        .subcommand(
            Command::new("launcher")
                .subcommand(Command::new("install"))
                .subcommand(Command::new("uninstall")),
        )
        .subcommand(Command::new("bar").subcommand(control(Command::new("toggle-collapse"))))
        .subcommand(Command::new("mouse").subcommand(control(Command::new("next-display"))));
    #[cfg(feature = "lua")]
    let root = root.subcommand(
        Command::new("script").subcommand(crate::ScriptCmd::augment_args(Command::new("run"))),
    );
    root
}

/// Parse a positive integral duration without floating-point rounding or overflow.
fn timeout(value: &str) -> Result<u64, String> {
    let (digits, multiplier) = if let Some(digits) = value.strip_suffix("ms") {
        (digits, 1)
    } else if let Some(digits) = value.strip_suffix('s') {
        (digits, 1000)
    } else {
        return Err("timeout requires ms or s (for example 250ms or 5s)".into());
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| *n > 0)
        .ok_or_else(|| "timeout must be a positive duration in range".into())
}

fn boolean(matches: &ArgMatches, name: &str) -> bool {
    matches
        .try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}
fn invalid(message: impl Into<String>) -> clap::Error {
    clap::Error::raw(clap::error::ErrorKind::ValueValidation, message.into())
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(crate) fn parse_from<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let tokens: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut command = command();
    let root_matches = command.try_get_matches_from_mut(tokens.clone())?;
    let mut matches = &root_matches;
    let mut path = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        path.push(name);
        matches = child;
    }
    let mut help = command.clone();
    for name in &path {
        if let Some(child) = help.find_subcommand(name) {
            help = child.clone();
        }
    }
    if help.get_subcommands().next().is_some() {
        help = help.bin_name(
            std::iter::once("spool")
                .chain(path.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
        );
        return Ok(Invocation::Help(help.render_long_help().to_string()));
    }
    let json = boolean(matches, "json");
    let timeout_ms = matches
        .try_get_one::<u64>("timeout")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(5000);
    if path
        .last()
        .is_some_and(|verb| matches!(*verb, "list" | "inspect"))
    {
        let resource = match path.as_slice() {
            ["window", _] => Resource::Window,
            ["space", _] => Resource::Space,
            ["space", "layout", "inspect"] => Resource::SpaceLayout,
            ["display", _] => Resource::Display,
            ["app", _] => Resource::App,
            ["session", "inspect"] => Resource::Session,
            _ => return Err(invalid("unknown read resource")),
        };
        let source = matches
            .get_one::<String>("source")
            .ok_or_else(|| invalid("missing source"))?
            .parse::<Source>()
            .map_err(invalid)?;
        let id = matches.try_get_one::<u64>("id").ok().flatten().copied();
        let mode = if path.last() == Some(&"list") {
            ReadMode::List
        } else {
            ReadMode::Inspect { id }
        };
        let show = matches
            .try_get_many::<String>("show")
            .ok()
            .flatten()
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut filters = Vec::new();
        for field in [
            "pid",
            "bundle-id",
            "space",
            "display",
            "title",
            "on-screen",
            "minimized",
            "kind",
            "visible",
            "name",
            "hidden",
            "main",
        ] {
            if let Some(values) = matches.try_get_many::<String>(field).ok().flatten() {
                filters.push(
                    Filter::new(resource, field, values.cloned().collect()).map_err(invalid)?,
                );
            }
        }
        let request = ReadRequest {
            resource,
            source,
            mode,
            show,
            filters,
            timeout_ms,
        };
        request.validate().map_err(invalid)?;
        return Ok(Invocation::Read { request, json });
    }
    match path.as_slice() {
        ["session", "watch"] => {
            return Ok(Invocation::Watch {
                json,
                raw: boolean(matches, "raw"),
                timeout_ms,
            });
        }
        ["launcher", verb] => {
            return Ok(Invocation::Launcher {
                install: *verb == "install",
            });
        }
        #[cfg(feature = "lua")]
        ["script", "run"] => {
            return Ok(Invocation::Script(crate::ScriptCmd::from_arg_matches(
                matches,
            )?));
        }
        ["service", verb] if !matches!(*verb, "quit" | "dump-state") => {
            let command = match *verb {
                "run" => ServiceCommand::Run,
                "start" => ServiceCommand::Start,
                "stop" => ServiceCommand::Stop,
                "restart" => ServiceCommand::Restart,
                "install" => ServiceCommand::Install,
                "uninstall" => ServiceCommand::Uninstall,
                "reinstall" => ServiceCommand::Reinstall,
                "logs" => ServiceCommand::Logs(crate::logs::LogArgs::from_arg_matches(matches)?),
                _ => return Err(invalid("unknown service command")),
            };
            return Ok(Invocation::Service(command));
        }
        _ => {}
    }
    // Shared action parsing owns semantics for both CLI and Lua spool.run.
    let mut action_args = Vec::new();
    let mut args = tokens.into_iter().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|_| invalid("action arguments must be UTF-8"))?;
        if arg == "--json" {
            continue;
        }
        if arg == "--timeout" {
            args.next();
            continue;
        }
        if arg.starts_with("--timeout=") {
            continue;
        }
        if let Some((option, value)) = arg.split_once('=')
            && option.starts_with("--")
        {
            action_args.push(option.to_owned());
            action_args.push(value.to_owned());
        } else {
            action_args.push(arg);
        }
    }
    let borrowed = action_args.iter().map(String::as_str).collect::<Vec<_>>();
    let action = spool_shared_types::argv::parse_action(&borrowed)
        .map_err(|error| invalid(error.to_string()))?;
    let _ = borrowed;
    Ok(Invocation::Action {
        action,
        timeout_ms,
        json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_id_and_layout_ordinal_remain_distinct_at_cli_boundary() {
        let Invocation::Action { action, .. } =
            parse_from(["spool", "window", "focus", "42"]).unwrap()
        else {
            panic!("action")
        };
        assert_eq!(
            action,
            spool_shared_types::commands::Action::FocusWindow { window_id: 42 }
        );
        let Invocation::Action { action, .. } =
            parse_from(["spool", "window", "focus", "--nth", "2"]).unwrap()
        else {
            panic!("action")
        };
        assert_eq!(
            action,
            spool_shared_types::commands::Action::Window(
                spool_shared_types::commands::Operation::Focus(
                    spool_shared_types::commands::Direction::Nth(1)
                )
            )
        );
        assert!(parse_from(["spool", "action", "window", "focus", "42"]).is_err());
    }

    #[test]
    fn list_filters_and_detail_selection_have_separate_scopes() {
        let Invocation::Read { request, json } = parse_from([
            "spool",
            "window",
            "list",
            "--source",
            "native",
            "--title",
            "one",
            "--title",
            "two",
            "--pid",
            "123",
            "--json",
            "--timeout",
            "250ms",
        ])
        .unwrap() else {
            panic!("read")
        };
        assert!(json);
        assert_eq!(request.source, Source::Native);
        assert_eq!(request.timeout_ms, 250);
        assert_eq!(
            request
                .filters
                .iter()
                .find(|filter| filter.field == "title")
                .unwrap()
                .values,
            ["one", "two"]
        );
        assert!(parse_from(["spool", "window", "list", "--show", "ax"]).is_err());
        assert!(parse_from(["spool", "space", "layout", "inspect", "--source", "native"]).is_err());
        assert!(
            parse_from([
                "spool", "window", "inspect", "42", "--source", "spool", "--show", "ax"
            ])
            .is_err()
        );
        let Invocation::Read { request, .. } = parse_from([
            "spool",
            "window",
            "inspect",
            "42",
            "--source",
            "native",
            "--show",
            "ax.AXTitle,cg",
        ])
        .unwrap() else {
            panic!("read")
        };
        assert_eq!(request.show, ["ax.AXTitle", "cg"]);
    }

    #[test]
    fn bare_resources_show_help_and_legacy_entries_are_removed() {
        for args in [
            vec!["spool"],
            vec!["spool", "window"],
            vec!["spool", "space", "layout"],
            vec!["spool", "space", "layout", "toggle"],
        ] {
            let Invocation::Help(help) = parse_from(args).unwrap() else {
                panic!("help")
            };
            assert!(help.contains("Usage:"));
        }
        for entry in [
            "launch",
            "query",
            "inspect",
            "subscribe",
            "send-cmd",
            "install-app",
            "daemon",
        ] {
            assert!(parse_from(["spool", entry]).is_err(), "{entry}");
        }
        assert!(matches!(
            parse_from(["spool", "service", "run"]),
            Ok(Invocation::Service(ServiceCommand::Run))
        ));
    }
    #[test]
    #[cfg(feature = "lua")]
    fn script_resource_preserves_eval_file_and_trailing_args() {
        let Invocation::Script(script) =
            parse_from(["spool", "script", "run", "-e", "return 1", "--", "hello"]).unwrap()
        else {
            panic!("script");
        };
        assert_eq!(script.eval.as_deref(), Some("return 1"));
        assert_eq!(script.args, ["hello"]);
        assert!(parse_from(["spool", "script", "run", "file.lua", "-e", "return 1"]).is_err());
    }
}

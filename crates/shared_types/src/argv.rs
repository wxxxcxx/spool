//! The argv encoding of a [`Command`]: `["window", "focus", "east"]`.
//!
//! This is the wire format of the `send-cmd` socket protocol and the shape the
//! TOML `[bindings]` keys are split into, so parsing and formatting live
//! together here and are checked against each other by round-trip tests.

use crate::commands::{Command, Direction, MouseMove, MoveFocus, Operation, ResizeDirection};

/// Why an argv vector is not a command. Consumers wrap this in their own error
/// type; the message is already user-facing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(String);

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn invalid(argv: &[&str]) -> Self {
        Self(format!("invalid command '{argv:?}'"))
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// Parses a command argument vector into a [`Command`] (e.g. `["window",
/// "focus", "east"]`).
///
/// # Errors
///
/// Returns [`ParseError`] if `argv` is not a recognized command encoding.
pub fn parse_command(argv: &[&str]) -> Result<Command> {
    let command = *argv.first().unwrap_or(&"");
    Ok(match command {
        "printstate" => Command::PrintState,
        "reconcile-windows" => Command::ReconcileWindows,
        "window" => parse_window_command(&argv[1..])?,
        "space" => parse_space_command(&argv[1..])?,
        "mouse" => Command::Mouse(parse_mouse_move(&argv[1..])?),
        "quit" => Command::Quit,
        "restart" => Command::Restart,
        _ => return Err(ParseError::new(format!("unhandled command '{argv:?}'"))),
    })
}

fn parse_i32(input: &str, what: &str) -> Result<i32> {
    input
        .parse()
        .map_err(|_| ParseError::new(format!("invalid {what} '{input}'")))
}

fn parse_u32(input: &str, what: &str) -> Result<u32> {
    input
        .parse()
        .map_err(|_| ParseError::new(format!("invalid {what} '{input}'")))
}

fn parse_u64(input: &str, what: &str) -> Result<u64> {
    input
        .parse()
        .map_err(|_| ParseError::new(format!("invalid {what} '{input}'")))
}

fn parse_window_command(argv: &[&str]) -> Result<Command> {
    match *argv.first().unwrap_or(&"") {
        "focusid" if argv.len() == 2 => Ok(Command::FocusWindow {
            window_id: parse_i32(argv[1], "window id")?,
        }),
        "move-to-space" if argv.len() == 4 => {
            let move_focus = match argv[3] {
                "follow" => MoveFocus::Follow,
                "stay" => MoveFocus::Stay,
                other => {
                    return Err(ParseError::new(format!(
                        "invalid move focus behavior '{other}'"
                    )));
                }
            };
            Ok(Command::MoveWindowToSpace {
                window_id: parse_i32(argv[1], "window id")?,
                space_id: parse_u64(argv[2], "space id")?,
                move_focus,
            })
        }
        _ => Ok(Command::Window(parse_operation(argv)?)),
    }
}

fn parse_space_command(argv: &[&str]) -> Result<Command> {
    match argv {
        ["focus", space_id] => Ok(Command::FocusSpace {
            space_id: parse_u64(space_id, "space id")?,
        }),
        ["create", display_id] => Ok(Command::CreateSpace {
            display_id: parse_u32(display_id, "display id")?,
        }),
        ["delete", space_id] => Ok(Command::DeleteSpace {
            space_id: parse_u64(space_id, "space id")?,
        }),
        _ => Err(ParseError::invalid(argv)),
    }
}

/// Parses a window operation (e.g. `["focus", "east"]`).
fn parse_operation(argv: &[&str]) -> Result<Operation> {
    let command = *argv.first().unwrap_or(&"");
    let err = || ParseError::invalid(argv);
    let argument = || argv.get(1).ok_or_else(err).copied();

    Ok(match command {
        "focus" => match argument()? {
            "floating" => Operation::FocusFloating,
            "tiled" => Operation::FocusTiled,
            direction => Operation::Focus(Direction::parse_positional(direction)?),
        },
        "raise" => match argument()? {
            "floating" => Operation::RaiseFloating,
            _ => return Err(err()),
        },
        "togglefloatlayer" => Operation::ToggleFloatingLayer,
        "swap" => Operation::Swap(Direction::parse(argument()?)?),
        "center" => Operation::Center,
        "resize" => Operation::Resize(
            argv.get(1)
                .map_or(Ok(ResizeDirection::Grow), |arg| ResizeDirection::parse(arg))?,
        ),
        "grow" => Operation::Resize(ResizeDirection::Grow),
        "shrink" => Operation::Resize(ResizeDirection::Shrink),
        "fullwidth" => Operation::FullWidth,
        "togglefloating" => Operation::ToggleFloating,
        "equalize" => Operation::Equalize,
        "balance" => Operation::Balance,
        "stack" => Operation::Stack(true),
        "unstack" => Operation::Stack(false),
        "nextdisplay" => Operation::ToNextDisplay(MoveFocus::Follow),
        "nextdisplaysend" => Operation::ToNextDisplay(MoveFocus::Stay),
        "snap" => Operation::Snap,
        _ => return Err(err()),
    })
}

/// Parses a mouse command (e.g. `["nextdisplay"]`).
fn parse_mouse_move(argv: &[&str]) -> Result<MouseMove> {
    match *argv.first().unwrap_or(&"") {
        "nextdisplay" => Ok(MouseMove::ToNextDisplay),
        _ => Err(ParseError::new(format!("invalid mouse command '{argv:?}'"))),
    }
}

impl Command {
    /// The argv encoding of this command, as understood by [`parse_command`].
    ///
    /// [`Command::Lua`] and [`Command::Layout`] have no encoding — they are
    /// only ever issued in-process — and yield `None`.
    #[must_use]
    pub fn to_argv(&self) -> Option<Vec<String>> {
        let argv = match self {
            Command::Window(operation) => {
                let mut argv = vec!["window".to_string()];
                argv.extend(operation.to_argv());
                argv
            }
            Command::Mouse(MouseMove::ToNextDisplay) => {
                vec!["mouse".to_string(), "nextdisplay".to_string()]
            }
            Command::FocusWindow { window_id } => {
                vec![
                    "window".to_string(),
                    "focusid".to_string(),
                    window_id.to_string(),
                ]
            }
            Command::FocusSpace { space_id } => vec![
                "space".to_string(),
                "focus".to_string(),
                space_id.to_string(),
            ],
            Command::MoveWindowToSpace {
                window_id,
                space_id,
                move_focus,
            } => vec![
                "window".to_string(),
                "move-to-space".to_string(),
                window_id.to_string(),
                space_id.to_string(),
                match move_focus {
                    MoveFocus::Follow => "follow",
                    MoveFocus::Stay => "stay",
                }
                .to_string(),
            ],
            Command::CreateSpace { display_id } => vec![
                "space".to_string(),
                "create".to_string(),
                display_id.to_string(),
            ],
            Command::DeleteSpace { space_id } => vec![
                "space".to_string(),
                "delete".to_string(),
                space_id.to_string(),
            ],
            Command::Quit => vec!["quit".to_string()],
            Command::Restart => vec!["restart".to_string()],
            Command::PrintState => vec!["printstate".to_string()],
            Command::ReconcileWindows => vec!["reconcile-windows".to_string()],
            Command::Lua(_) | Command::Layout(_) => return None,
        };
        Some(argv)
    }
}

impl Operation {
    /// The argv tail following `window`, e.g. `["focus", "east"]`.
    fn to_argv(&self) -> Vec<String> {
        let owned = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect();
        match self {
            Operation::Focus(direction) => vec!["focus".to_string(), direction.token()],
            Operation::Swap(direction) => vec!["swap".to_string(), direction.token()],
            Operation::Center => owned(&["center"]),
            Operation::Resize(direction) => owned(&["resize", direction.token()]),
            // `SetWidth` comes from window rules, not from a command line; it has
            // no argv verb, so encode it as the equivalent full-width toggle.
            Operation::SetWidth(_) | Operation::FullWidth => owned(&["fullwidth"]),
            Operation::ToNextDisplay(MoveFocus::Follow) => owned(&["nextdisplay"]),
            Operation::ToNextDisplay(MoveFocus::Stay) => owned(&["nextdisplaysend"]),
            Operation::Equalize => owned(&["equalize"]),
            Operation::Balance => owned(&["balance"]),
            Operation::ToggleFloating => owned(&["togglefloating"]),
            Operation::Stack(true) => owned(&["stack"]),
            Operation::Stack(false) => owned(&["unstack"]),
            Operation::Snap => owned(&["snap"]),
            Operation::FocusFloating => owned(&["focus", "floating"]),
            Operation::FocusTiled => owned(&["focus", "tiled"]),
            Operation::RaiseFloating => owned(&["raise", "floating"]),
            Operation::ToggleFloatingLayer => owned(&["togglefloatlayer"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(command: &Command) -> Command {
        let argv = command.to_argv().expect("command should encode to argv");
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        parse_command(&borrowed).unwrap_or_else(|err| panic!("re-parsing {argv:?}: {err}"))
    }

    #[test]
    fn every_operation_round_trips_through_argv() {
        let operations = [
            Operation::Focus(Direction::East),
            Operation::Focus(Direction::Nth(2)),
            Operation::Swap(Direction::West),
            Operation::Center,
            Operation::Resize(ResizeDirection::Shrink),
            Operation::FullWidth,
            Operation::ToNextDisplay(MoveFocus::Follow),
            Operation::ToNextDisplay(MoveFocus::Stay),
            Operation::Equalize,
            Operation::Balance,
            Operation::ToggleFloating,
            Operation::Stack(true),
            Operation::Stack(false),
            Operation::Snap,
            Operation::FocusFloating,
            Operation::FocusTiled,
            Operation::RaiseFloating,
            Operation::ToggleFloatingLayer,
        ];

        for operation in operations {
            let command = Command::Window(operation.clone());
            let reparsed = round_trip(&command);
            assert_eq!(
                format!("{reparsed:?}"),
                format!("{command:?}"),
                "argv round-trip changed {operation:?}"
            );
        }
    }

    #[test]
    fn global_commands_round_trip() {
        for command in [
            Command::Quit,
            Command::Restart,
            Command::PrintState,
            Command::ReconcileWindows,
            Command::Mouse(MouseMove::ToNextDisplay),
            Command::FocusWindow { window_id: 42 },
            Command::FocusSpace { space_id: 99 },
            Command::MoveWindowToSpace {
                window_id: 42,
                space_id: 99,
                move_focus: MoveFocus::Follow,
            },
            Command::CreateSpace { display_id: 7 },
            Command::DeleteSpace { space_id: 99 },
        ] {
            assert_eq!(
                format!("{:?}", round_trip(&command)),
                format!("{command:?}")
            );
        }
    }

    #[test]
    fn lua_commands_have_no_argv_encoding() {
        assert!(Command::Lua(1).to_argv().is_none());
    }

    #[test]
    fn window_numbers_are_one_based() {
        assert_eq!(
            Direction::parse_positional("1"),
            Ok(Direction::Nth(0)),
            "the first window is number 1"
        );
        assert!(Direction::parse_positional("0").is_err());
    }

    #[test]
    fn legacy_managed_command_names_are_rejected() {
        assert!(parse_command(&["window", "manage"]).is_err());
        assert!(parse_command(&["window", "focus", "unmanaged"]).is_err());
        assert!(parse_command(&["window", "focus", "managed"]).is_err());
    }

    #[test]
    fn invalid_commands_are_rejected() {
        assert!(parse_command(&["definitely", "not", "a", "command"]).is_err());
        assert!(parse_command(&["window", "focus"]).is_err());
        assert!(parse_command(&["window", "swap", "3"]).is_err());
    }
}

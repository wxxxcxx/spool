//! The argv encoding of an [`Action`]: `["window", "focus", "east"]`.
//!
//! This is the argument shape of `spool action` and Lua's `spool.run`, so parsing and formatting live
//! together here and are checked against each other by round-trip tests.

use crate::commands::{
    Action, Direction, FocusStep, MouseMove, MoveFocus, Operation, ResizeAxis, ResizeDirection,
};

/// Why an argv vector is not an action. Consumers wrap this in their own error
/// type; the message is already user-facing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(String);

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn invalid(argv: &[&str]) -> Self {
        Self(format!("invalid action '{argv:?}'"))
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// Parses an action argument vector into an [`Action`] (e.g. `["window",
/// "focus", "east"]`).
///
/// # Errors
///
/// Returns [`ParseError`] if `argv` is not a recognized action encoding.
pub fn parse_action(argv: &[&str]) -> Result<Action> {
    let action = *argv.first().unwrap_or(&"");
    Ok(match action {
        "printstate" => Action::PrintState,
        "reconcile-windows" => Action::ReconcileWindows,
        "mission-control" if argv.len() == 1 => Action::MissionControl,
        "show-desktop" if argv.len() == 1 => Action::ShowDesktop,
        "window" => parse_window_action(&argv[1..])?,
        "space" => parse_space_action(&argv[1..])?,
        "mouse" => Action::Mouse(parse_mouse_move(&argv[1..])?),
        "quit" => Action::Quit,
        "restart" => Action::Restart,
        _ => return Err(ParseError::new(format!("unhandled action '{argv:?}'"))),
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

fn parse_window_action(argv: &[&str]) -> Result<Action> {
    match *argv.first().unwrap_or(&"") {
        "focusid" if argv.len() == 2 => Ok(Action::FocusWindow {
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
            Ok(Action::MoveWindowToSpace {
                window_id: parse_i32(argv[1], "window id")?,
                space_id: parse_u64(argv[2], "space id")?,
                move_focus,
            })
        }
        _ => Ok(Action::Window(parse_operation(argv)?)),
    }
}

fn parse_space_action(argv: &[&str]) -> Result<Action> {
    match argv {
        ["focus", space_id] => Ok(Action::FocusSpace {
            space_id: parse_u64(space_id, "space id")?,
        }),
        ["create", display_id] => Ok(Action::CreateSpace {
            display_id: parse_u32(display_id, "display id")?,
        }),
        ["delete", space_id] => Ok(Action::DeleteSpace {
            space_id: parse_u64(space_id, "space id")?,
        }),
        _ => Err(ParseError::invalid(argv)),
    }
}

/// Parses a window operation (e.g. `["focus", "east"]`).
fn parse_operation(argv: &[&str]) -> Result<Operation> {
    if argv == ["focus", "other", "layer"] {
        return Ok(Operation::FocusOtherLayer);
    }

    let action = *argv.first().unwrap_or(&"");
    let err = || ParseError::invalid(argv);
    let argument = || argv.get(1).ok_or_else(err).copied();

    Ok(match action {
        "focus" => match argument()? {
            "floating" => Operation::FocusFloating,
            "tiled" => Operation::FocusTiled,
            "other-layer" => Operation::FocusOtherLayer,
            "next" => Operation::FocusStep(FocusStep::Next),
            "previous" => Operation::FocusStep(FocusStep::Previous),
            direction => Operation::Focus(Direction::parse_positional(direction)?),
        },
        "move" => Operation::Move(Direction::parse(argument()?)?),
        "center" => Operation::Center,
        "grow" | "shrink" => {
            if argv.len() != 2 {
                return Err(err());
            }
            Operation::Resize {
                axis: ResizeAxis::parse(argument()?)?,
                direction: ResizeDirection::parse(action)?,
            }
        }
        "maximize" => Operation::Maximize,
        "toggle" => match argument()? {
            "floating" => Operation::ToggleFloating,
            "stack" => Operation::ToggleStack,
            "tiled-visibility" => Operation::ToggleTiledVisibility,
            _ => return Err(err()),
        },
        "equalize" => Operation::Equalize,
        "balance" => Operation::Balance,
        "nextdisplay" => Operation::ToNextDisplay(MoveFocus::Follow),
        "nextdisplaysend" => Operation::ToNextDisplay(MoveFocus::Stay),
        "snap" => Operation::Snap,
        _ => return Err(err()),
    })
}

/// Parses a mouse action (e.g. `["nextdisplay"]`).
fn parse_mouse_move(argv: &[&str]) -> Result<MouseMove> {
    match *argv.first().unwrap_or(&"") {
        "nextdisplay" => Ok(MouseMove::ToNextDisplay),
        _ => Err(ParseError::new(format!("invalid mouse action '{argv:?}'"))),
    }
}

impl Action {
    /// The argv encoding of this action, as understood by [`parse_action`].
    ///
    /// Internal-only actions have no encoding and yield `None`.
    #[must_use]
    pub fn to_argv(&self) -> Option<Vec<String>> {
        let argv = match self {
            Action::Window(operation) => {
                let mut argv = vec!["window".to_string()];
                argv.extend(operation.to_argv());
                argv
            }
            Action::Mouse(MouseMove::ToNextDisplay) => {
                vec!["mouse".to_string(), "nextdisplay".to_string()]
            }
            Action::FocusWindow { window_id } => {
                vec![
                    "window".to_string(),
                    "focusid".to_string(),
                    window_id.to_string(),
                ]
            }
            Action::FocusSpace { space_id } => vec![
                "space".to_string(),
                "focus".to_string(),
                space_id.to_string(),
            ],
            Action::MoveWindowToSpace {
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
            Action::CreateSpace { display_id } => vec![
                "space".to_string(),
                "create".to_string(),
                display_id.to_string(),
            ],
            Action::DeleteSpace { space_id } => vec![
                "space".to_string(),
                "delete".to_string(),
                space_id.to_string(),
            ],
            Action::Quit => vec!["quit".to_string()],
            Action::Restart => vec!["restart".to_string()],
            Action::PrintState => vec!["printstate".to_string()],
            Action::ReconcileWindows => vec!["reconcile-windows".to_string()],
            Action::MissionControl => vec!["mission-control".to_string()],
            Action::ShowDesktop => vec!["show-desktop".to_string()],
            Action::Lua(_)
            | Action::Layout(_)
            | Action::ReorderColumn { .. }
            | Action::MoveColumnToSpace { .. } => return None,
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
            Operation::FocusStep(step) => owned(&["focus", step.token()]),
            Operation::FocusOtherLayer => owned(&["focus", "other-layer"]),
            Operation::Move(direction) => vec!["move".to_string(), direction.token()],
            Operation::Center => owned(&["center"]),
            Operation::Resize { axis, direction } => owned(&[direction.token(), axis.token()]),
            // `SetWidth` comes from window rules, not from the CLI action syntax; it has
            // no argv verb, so encode it as the equivalent full-width toggle.
            Operation::SetWidth(_) | Operation::Maximize => owned(&["maximize"]),
            Operation::ToNextDisplay(MoveFocus::Follow) => owned(&["nextdisplay"]),
            Operation::ToNextDisplay(MoveFocus::Stay) => owned(&["nextdisplaysend"]),
            Operation::Equalize => owned(&["equalize"]),
            Operation::Balance => owned(&["balance"]),
            Operation::ToggleFloating => owned(&["toggle", "floating"]),
            Operation::ToggleStack => owned(&["toggle", "stack"]),
            Operation::Snap => owned(&["snap"]),
            Operation::FocusFloating => owned(&["focus", "floating"]),
            Operation::FocusTiled => owned(&["focus", "tiled"]),
            Operation::ToggleTiledVisibility => owned(&["toggle", "tiled-visibility"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(action: &Action) -> Action {
        let argv = action.to_argv().expect("action should encode to argv");
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        parse_action(&borrowed).unwrap_or_else(|err| panic!("re-parsing {argv:?}: {err}"))
    }

    #[test]
    fn every_operation_round_trips_through_argv() {
        let operations = [
            Operation::Focus(Direction::East),
            Operation::Focus(Direction::Nth(2)),
            Operation::FocusStep(FocusStep::Next),
            Operation::FocusStep(FocusStep::Previous),
            Operation::Move(Direction::West),
            Operation::Center,
            Operation::Resize {
                axis: ResizeAxis::Width,
                direction: ResizeDirection::Shrink,
            },
            Operation::Resize {
                axis: ResizeAxis::Height,
                direction: ResizeDirection::Grow,
            },
            Operation::Maximize,
            Operation::ToNextDisplay(MoveFocus::Follow),
            Operation::ToNextDisplay(MoveFocus::Stay),
            Operation::Equalize,
            Operation::Balance,
            Operation::ToggleFloating,
            Operation::ToggleStack,
            Operation::Snap,
            Operation::FocusFloating,
            Operation::FocusTiled,
            Operation::FocusOtherLayer,
        ];

        for operation in operations {
            let action = Action::Window(operation.clone());
            let reparsed = round_trip(&action);
            assert_eq!(
                format!("{reparsed:?}"),
                format!("{action:?}"),
                "argv round-trip changed {operation:?}"
            );
        }
    }

    #[test]
    fn global_actions_round_trip() {
        for action in [
            Action::Quit,
            Action::Restart,
            Action::PrintState,
            Action::Window(Operation::ToggleTiledVisibility),
            Action::ReconcileWindows,
            Action::MissionControl,
            Action::ShowDesktop,
            Action::Mouse(MouseMove::ToNextDisplay),
            Action::FocusWindow { window_id: 42 },
            Action::FocusSpace { space_id: 99 },
            Action::MoveWindowToSpace {
                window_id: 42,
                space_id: 99,
                move_focus: MoveFocus::Follow,
            },
            Action::CreateSpace { display_id: 7 },
            Action::DeleteSpace { space_id: 99 },
        ] {
            assert_eq!(format!("{:?}", round_trip(&action)), format!("{action:?}"));
        }
    }

    #[test]
    fn system_overview_actions_parse_and_round_trip_without_extra_arguments() {
        for name in ["mission-control", "show-desktop"] {
            let action = parse_action(&[name]).expect("system overview action");
            assert_eq!(action.to_argv().unwrap(), vec![name]);
            assert!(parse_action(&[name, "unexpected"]).is_err());
        }
    }

    #[test]
    fn system_overview_actions_have_stable_wire_names() {
        for (action, name) in [
            (Action::MissionControl, "mission_control"),
            (Action::ShowDesktop, "show_desktop"),
        ] {
            let value = serde_json::to_value(&action).unwrap();
            assert_eq!(value, serde_json::json!(name));
            assert_eq!(serde_json::from_value::<Action>(value).unwrap(), action);
        }
    }

    #[test]
    fn lua_actions_have_no_argv_encoding() {
        assert!(Action::Lua(1).to_argv().is_none());
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
    fn focus_accepts_full_next_and_previous_names() {
        let next = parse_action(&["window", "focus", "next"])
            .expect("next should be a valid focus target");
        let previous = parse_action(&["window", "focus", "previous"])
            .expect("previous should be a valid focus target");

        assert_eq!(format!("{next:?}"), "Window(FocusStep(Next))");
        assert_eq!(format!("{previous:?}"), "Window(FocusStep(Previous))");
        assert!(parse_action(&["window", "focus", "prev"]).is_err());
        assert!(parse_action(&["window", "focus", "preview"]).is_err());
    }

    #[test]
    fn legacy_managed_action_names_are_rejected() {
        assert!(parse_action(&["window", "manage"]).is_err());
        assert!(parse_action(&["window", "focus", "unmanaged"]).is_err());
        assert!(parse_action(&["window", "focus", "managed"]).is_err());
    }

    #[test]
    fn removed_window_action_names_are_rejected() {
        for action in [
            &["window", "swap", "west"][..],
            &["window", "resize"][..],
            &["window", "grow"][..],
            &["window", "shrink"][..],
            &["window", "fullwidth"][..],
            &["window", "togglefloating"][..],
            &["window", "stack"][..],
            &["window", "unstack"][..],
            &["window", "raise", "floating"][..],
            &["window", "togglefloatlayer"][..],
        ] {
            assert!(
                parse_action(action).is_err(),
                "legacy action survived: {action:?}"
            );
        }
    }

    #[test]
    fn invalid_actions_are_rejected() {
        assert!(parse_action(&["definitely", "not", "a", "action"]).is_err());
        assert!(parse_action(&["window", "focus"]).is_err());
        assert!(parse_action(&["window", "move", "3"]).is_err());
    }
}

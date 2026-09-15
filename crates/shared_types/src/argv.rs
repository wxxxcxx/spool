//! The tokens encoding of an [`Action`]: `["window", "focus", "east"]`.
//!
//! This is the resource control argument shape shared by the CLI and Lua's `spool.run`, so parsing and formatting live
//! together here and are checked against each other by round-trip tests.

use crate::commands::{
    Action, ColumnWidth, Direction, FocusStep, MouseMove, MoveFocus, Operation, ResizeAxis,
    ResizeDirection, SpaceLayoutOperation,
};

/// Why an tokens vector is not an action. Consumers wrap this in their own error
/// type; the message is already user-facing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(String);

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn invalid(tokens: &[&str]) -> Self {
        Self(format!("invalid action '{tokens:?}'"))
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
/// Returns [`ParseError`] if `tokens` is not a recognized action encoding.
pub fn parse_action(tokens: &[&str]) -> Result<Action> {
    match tokens {
        ["service", "quit"] => Ok(Action::Quit),
        ["service", "restart"] => Ok(Action::Restart),
        ["service", "dump-state"] => Ok(Action::PrintState),
        ["window", "reconcile"] => Ok(Action::ReconcileWindows),
        ["session", "mission-control"] => Ok(Action::MissionControl),
        ["session", "show-desktop"] => Ok(Action::ShowDesktop),
        ["bar", "toggle-collapse"] => Ok(Action::ToggleBarCollapse),
        ["mouse", "next-display"] => Ok(Action::Mouse(MouseMove::ToNextDisplay)),
        ["window", rest @ ..] => parse_window_action(rest),
        ["space", "layout", rest @ ..] => parse_layout_action(rest),
        ["space", "prefer-focus", id, "--window", window] => Ok(Action::SetSpaceFocusPreference {
            space_id: positive(id, "Space ID")?,
            window_id: positive(window, "window ID")?,
        }),
        ["space", "focus", id] => Ok(Action::FocusSpace {
            space_id: positive(id, "Space ID")?,
        }),
        ["space", "delete", id] => Ok(Action::DeleteSpace {
            space_id: positive(id, "Space ID")?,
        }),
        ["space", "create", "--display", id] => Ok(Action::CreateSpace {
            display_id: positive(id, "display ID")?,
        }),
        _ => Err(ParseError::invalid(tokens)),
    }
}

fn positive<T: std::str::FromStr + PartialOrd + Default>(value: &str, name: &str) -> Result<T> {
    value
        .parse::<T>()
        .ok()
        .filter(|n| *n > T::default())
        .ok_or_else(|| ParseError::new(format!("{name} must be a positive integer: '{value}'")))
}

type Options<'a> = std::collections::BTreeMap<&'a str, &'a str>;

fn options<'a>(
    tokens: &[&'a str],
    values: &[&str],
    flags: &[&str],
) -> Result<(Vec<&'a str>, Options<'a>)> {
    let mut positional = Vec::new();
    let mut options = Options::new();
    let mut args = tokens.iter().copied();
    while let Some(arg) = args.next() {
        if values.contains(&arg) {
            let value = args
                .next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| ParseError::new(format!("missing value for {arg}")))?;
            if options.insert(arg, value).is_some() {
                return Err(ParseError::new(format!("duplicate {arg}")));
            }
        } else if flags.contains(&arg) {
            if options.insert(arg, "").is_some() {
                return Err(ParseError::new(format!("duplicate {arg}")));
            }
        } else if arg.starts_with('-') {
            return Err(ParseError::new(format!("unsupported option {arg}")));
        } else {
            positional.push(arg);
        }
    }
    Ok((positional, options))
}

fn move_focus(options: &Options<'_>) -> Result<MoveFocus> {
    match (
        options.contains_key("--follow"),
        options.contains_key("--stay"),
    ) {
        (true, false) => Ok(MoveFocus::Follow),
        (false, true) => Ok(MoveFocus::Stay),
        _ => Err(ParseError::new("specify exactly one of --follow or --stay")),
    }
}

fn parse_window_action(tokens: &[&str]) -> Result<Action> {
    let (args, opts) = options(
        tokens,
        &["--window", "--space", "--nth"],
        &["--follow", "--stay"],
    )?;
    let window_id = opts
        .get("--window")
        .map(|v| positive(v, "window ID"))
        .transpose()?;
    if args.first() == Some(&"focus") {
        if window_id.is_some() || opts.contains_key("--follow") || opts.contains_key("--stay") {
            return Err(ParseError::invalid(tokens));
        }
        if let Some(nth) = opts.get("--nth") {
            if args.len() != 1 || opts.len() != 1 {
                return Err(ParseError::invalid(tokens));
            }
            let ordinal: usize = positive(nth, "window ordinal")?;
            return Ok(Action::Window(Operation::Focus(Direction::Nth(
                ordinal - 1,
            ))));
        }
        let [_, target] = args.as_slice() else {
            return Err(ParseError::invalid(tokens));
        };
        if let Ok(window_id) = positive::<i32>(target, "window ID") {
            return match opts.get("--space") {
                Some(space) => Ok(Action::FocusWindowInSpace {
                    window_id,
                    space_id: positive(space, "Space ID")?,
                }),
                None => Ok(Action::FocusWindow { window_id }),
            };
        }
        if !opts.is_empty() {
            return Err(ParseError::invalid(tokens));
        }
        return Ok(Action::Window(match *target {
            "floating" => Operation::FocusFloating,
            "tiled" => Operation::FocusTiled,
            "other-layer" => Operation::FocusOtherLayer,
            "next" => Operation::FocusStep(FocusStep::Next),
            "previous" => Operation::FocusStep(FocusStep::Previous),
            other => Operation::Focus(Direction::parse(other)?),
        }));
    }
    if opts.contains_key("--space") || opts.contains_key("--nth") {
        return Err(ParseError::invalid(tokens));
    }
    let operation = match args.as_slice() {
        ["move-to-space", space] => {
            let space_id = positive(space, "Space ID")?;
            let move_focus = move_focus(&opts)?;
            return Ok(match window_id {
                Some(window_id) => Action::MoveWindowToSpace {
                    window_id,
                    space_id,
                    move_focus,
                },
                None => Action::MoveFocusedWindowToSpace {
                    space_id,
                    move_focus,
                },
            });
        }
        ["move-to-display", "next"] => Operation::ToNextDisplay(move_focus(&opts)?),
        _ => {
            if opts.contains_key("--follow") || opts.contains_key("--stay") {
                return Err(ParseError::invalid(tokens));
            }
            parse_operation(&args)?
        }
    };
    Ok(match window_id {
        Some(window_id) => Action::TargetedWindow {
            window_id,
            operation,
        },
        None => Action::Window(operation),
    })
}

fn parse_operation(tokens: &[&str]) -> Result<Operation> {
    Ok(match tokens {
        ["move", direction] => Operation::Move(Direction::parse(direction)?),
        ["center"] => Operation::Center,
        [direction @ ("grow" | "shrink"), axis] => Operation::Resize {
            axis: ResizeAxis::parse(axis)?,
            direction: ResizeDirection::parse(direction)?,
        },
        ["maximize"] => Operation::Maximize,
        ["snap"] => Operation::Snap,
        ["toggle", "floating"] => Operation::ToggleFloating,
        ["toggle", "stack"] => Operation::ToggleStack,
        _ => return Err(ParseError::invalid(tokens)),
    })
}

fn parse_layout_action(tokens: &[&str]) -> Result<Action> {
    let (args, opts) = options(tokens, &["--space", "--column", "--reference-column"], &[])?;
    let space_id = opts
        .get("--space")
        .map(|v| positive(v, "Space ID"))
        .transpose()?;
    let operation = match args.as_slice() {
        ["width", value] if !opts.contains_key("--reference-column") => {
            let column = opts
                .get("--column")
                .ok_or_else(|| ParseError::new("width requires --column"))
                .and_then(|value| positive(value, "column ordinal"))?;
            let width = if *value == "inherit" {
                ColumnWidth::Inherit
            } else {
                let (value, ratio) = value
                    .strip_suffix('%')
                    .map_or((*value, false), |v| (v, true));
                let number = value
                    .parse::<f64>()
                    .map_err(|_| ParseError::new("invalid width"))?;
                if !number.is_finite() || number <= 0.0 {
                    return Err(ParseError::new("width must be positive and finite"));
                }
                if ratio {
                    ColumnWidth::Ratio(number / 100.0)
                } else {
                    ColumnWidth::Points(number)
                }
            };
            SpaceLayoutOperation::SetWidth { column, width }
        }
        ["equalize"] if !opts.contains_key("--reference-column") => {
            SpaceLayoutOperation::Equalize {
                column: opts
                    .get("--column")
                    .map(|v| positive(v, "column ordinal"))
                    .transpose()?,
            }
        }
        ["balance"] if !opts.contains_key("--column") => SpaceLayoutOperation::Balance {
            reference_column: opts
                .get("--reference-column")
                .map(|v| positive(v, "column ordinal"))
                .transpose()?,
        },
        ["toggle", "tiled-visibility"]
            if !opts.contains_key("--column") && !opts.contains_key("--reference-column") =>
        {
            SpaceLayoutOperation::ToggleTiledVisibility
        }
        _ => return Err(ParseError::invalid(tokens)),
    };
    if opts.is_empty() {
        return Ok(Action::Window(match operation {
            SpaceLayoutOperation::SetWidth { .. } => {
                unreachable!("width requires an explicit column")
            }
            SpaceLayoutOperation::Equalize { .. } => Operation::Equalize,
            SpaceLayoutOperation::Balance { .. } => Operation::Balance,
            SpaceLayoutOperation::ToggleTiledVisibility => Operation::ToggleTiledVisibility,
        }));
    }
    Ok(Action::SpaceLayout {
        space_id,
        operation,
    })
}

impl Action {
    /// The resource tokens encoding shared by CLI controls and Lua's `spool.run`.
    /// Internal-only actions have no public encoding.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive command/source branches share one admission or capture boundary"
    )]
    pub fn to_argv(&self) -> Option<Vec<String>> {
        let owned = |tokens: &[&str]| tokens.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let follow = |value: &MoveFocus| match value {
            MoveFocus::Follow => "--follow",
            MoveFocus::Stay => "--stay",
        };
        Some(match self {
            Self::Window(Operation::SetWidth(_))
            | Self::Lua(_)
            | Self::Layout(_)
            | Self::RestoreIntents(_)
            | Self::ReorderColumn { .. }
            | Self::MoveColumnToSpace { .. } => return None,

            Self::Window(Operation::Equalize) => owned(&["space", "layout", "equalize"]),
            Self::Window(Operation::Balance) => owned(&["space", "layout", "balance"]),
            Self::Window(Operation::ToggleTiledVisibility) => {
                owned(&["space", "layout", "toggle", "tiled-visibility"])
            }
            Self::Window(operation) => {
                let mut tokens = owned(&["window"]);
                tokens.extend(operation.to_argv());
                tokens
            }
            Self::TargetedWindow {
                window_id,
                operation,
            } => {
                if !matches!(
                    operation,
                    Operation::Move(_)
                        | Operation::Center
                        | Operation::Resize { .. }
                        | Operation::Maximize
                        | Operation::Snap
                        | Operation::ToggleFloating
                        | Operation::ToggleStack
                        | Operation::ToNextDisplay(_)
                ) {
                    return None;
                }
                let mut tokens = Self::Window(operation.clone()).to_argv()?;
                tokens.extend(["--window".into(), window_id.to_string()]);
                tokens
            }
            Self::SpaceLayout {
                space_id,
                operation,
            } => {
                let mut tokens = owned(&["space", "layout"]);
                match operation {
                    SpaceLayoutOperation::SetWidth { column, width } => {
                        let value = match width {
                            ColumnWidth::Inherit => "inherit".into(),
                            ColumnWidth::Points(value) => value.to_string(),
                            ColumnWidth::Ratio(value) => format!("{}%", value * 100.0),
                        };
                        tokens.extend([
                            "width".into(),
                            value,
                            "--column".into(),
                            column.to_string(),
                        ]);
                    }
                    SpaceLayoutOperation::Equalize { column } => {
                        tokens.push("equalize".into());
                        if let Some(n) = column {
                            tokens.extend(["--column".into(), n.to_string()]);
                        }
                    }
                    SpaceLayoutOperation::Balance { reference_column } => {
                        tokens.push("balance".into());
                        if let Some(n) = reference_column {
                            tokens.extend(["--reference-column".into(), n.to_string()]);
                        }
                    }
                    SpaceLayoutOperation::ToggleTiledVisibility => {
                        tokens.extend(owned(&["toggle", "tiled-visibility"]));
                    }
                }
                if let Some(id) = space_id {
                    tokens.extend(["--space".into(), id.to_string()]);
                }
                tokens
            }
            Self::Mouse(MouseMove::ToNextDisplay) => owned(&["mouse", "next-display"]),
            Self::FocusWindow { window_id } => {
                vec!["window".into(), "focus".into(), window_id.to_string()]
            }
            Self::FocusWindowInSpace {
                window_id,
                space_id,
            } => vec![
                "window".into(),
                "focus".into(),
                window_id.to_string(),
                "--space".into(),
                space_id.to_string(),
            ],
            Self::SetSpaceFocusPreference {
                space_id,
                window_id,
            } => vec![
                "space".into(),
                "prefer-focus".into(),
                space_id.to_string(),
                "--window".into(),
                window_id.to_string(),
            ],
            Self::FocusSpace { space_id } => {
                vec!["space".into(), "focus".into(), space_id.to_string()]
            }
            Self::MoveWindowToSpace {
                window_id,
                space_id,
                move_focus,
            } => vec![
                "window".into(),
                "move-to-space".into(),
                space_id.to_string(),
                "--window".into(),
                window_id.to_string(),
                follow(move_focus).into(),
            ],
            Self::MoveFocusedWindowToSpace {
                space_id,
                move_focus,
            } => vec![
                "window".into(),
                "move-to-space".into(),
                space_id.to_string(),
                follow(move_focus).into(),
            ],
            Self::CreateSpace { display_id } => vec![
                "space".into(),
                "create".into(),
                "--display".into(),
                display_id.to_string(),
            ],
            Self::DeleteSpace { space_id } => {
                vec!["space".into(), "delete".into(), space_id.to_string()]
            }
            Self::Quit => owned(&["service", "quit"]),
            Self::Restart => owned(&["service", "restart"]),
            Self::PrintState => owned(&["service", "dump-state"]),
            Self::ReconcileWindows => owned(&["window", "reconcile"]),
            Self::MissionControl => owned(&["session", "mission-control"]),
            Self::ShowDesktop => owned(&["session", "show-desktop"]),
            Self::ToggleBarCollapse => owned(&["bar", "toggle-collapse"]),
        })
    }
}

impl Operation {
    /// The tokens tail following `window`, e.g. `["focus", "east"]`.
    fn to_argv(&self) -> Vec<String> {
        let owned = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect();
        match self {
            Operation::Focus(Direction::Nth(index)) => {
                vec!["focus".into(), "--nth".into(), (index + 1).to_string()]
            }
            Operation::Focus(direction) => vec!["focus".to_string(), direction.token()],
            Operation::FocusStep(step) => owned(&["focus", step.token()]),
            Operation::FocusOtherLayer => owned(&["focus", "other-layer"]),
            Operation::Move(direction) => vec!["move".to_string(), direction.token()],
            Operation::Center => owned(&["center"]),
            Operation::Resize { axis, direction } => owned(&[direction.token(), axis.token()]),
            Operation::SetWidth(_) => Vec::new(),
            Operation::Maximize => owned(&["maximize"]),
            Operation::ToNextDisplay(MoveFocus::Follow) => {
                owned(&["move-to-display", "next", "--follow"])
            }
            Operation::ToNextDisplay(MoveFocus::Stay) => {
                owned(&["move-to-display", "next", "--stay"])
            }
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

    #[test]
    fn resource_focus_distinguishes_native_id_from_layout_ordinal() {
        assert_eq!(
            parse_action(&["window", "focus", "42"]).unwrap(),
            Action::FocusWindow { window_id: 42 }
        );
        assert_eq!(
            parse_action(&["window", "focus", "--nth", "2"]).unwrap(),
            Action::Window(Operation::Focus(Direction::Nth(1)))
        );
        assert!(parse_action(&["window", "focus", "42", "--nth", "2"]).is_err());
        assert!(parse_action(&["window", "focusid", "42"]).is_err());
    }

    #[test]
    fn resource_actions_enforce_scope_and_remove_legacy_paths() {
        assert_eq!(parse_action(&["service", "quit"]).unwrap(), Action::Quit);
        assert_eq!(
            parse_action(&["space", "layout", "balance"]).unwrap(),
            Action::Window(Operation::Balance)
        );
        assert!(parse_action(&["window", "center", "--window", "42"]).is_ok());
        assert!(
            parse_action(&[
                "space", "layout", "equalize", "--space", "9", "--column", "2"
            ])
            .is_ok()
        );
        for args in [
            vec!["quit"],
            vec!["window", "balance"],
            vec!["window", "center", "extra"],
            vec!["window", "move-to-display", "next"],
            vec!["space", "layout", "equalize", "--column", "0"],
            vec!["space", "layout", "balance", "--column", "2"],
        ] {
            assert!(parse_action(&args).is_err(), "accepted {args:?}");
        }
        assert!(Action::Window(Operation::SetWidth(0.5)).to_argv().is_none());
    }

    #[test]
    fn column_width_tokens_preserve_original_units() {
        for (value, width) in [
            ("800", ColumnWidth::Points(800.0)),
            ("75%", ColumnWidth::Ratio(0.75)),
            ("inherit", ColumnWidth::Inherit),
        ] {
            let action = parse_action(&[
                "space", "layout", "width", value, "--column", "2", "--space", "77",
            ])
            .unwrap();
            assert_eq!(
                action,
                Action::SpaceLayout {
                    space_id: Some(77),
                    operation: SpaceLayoutOperation::SetWidth { column: 2, width }
                }
            );
            assert_eq!(round_trip(&action), action);
        }
        for value in ["0", "-1", "NaN", "inf", "0%"] {
            assert!(parse_action(&["space", "layout", "width", value, "--column", "1"]).is_err());
        }
        assert!(parse_action(&["space", "layout", "width", "800"]).is_err());
    }

    fn round_trip(action: &Action) -> Action {
        let tokens = action.to_argv().expect("action should encode to tokens");
        let borrowed: Vec<&str> = tokens.iter().map(String::as_str).collect();
        parse_action(&borrowed).unwrap_or_else(|err| panic!("re-parsing {tokens:?}: {err}"))
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
                "tokens round-trip changed {operation:?}"
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
            Action::FocusWindowInSpace {
                window_id: 42,
                space_id: 99,
            },
            Action::FocusSpace { space_id: 99 },
            Action::SetSpaceFocusPreference {
                space_id: 99,
                window_id: 42,
            },
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
            let action = parse_action(&["session", name]).expect("system overview action");
            assert_eq!(action.to_argv().unwrap(), vec!["session", name]);
            assert!(parse_action(&["session", name, "unexpected"]).is_err());
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

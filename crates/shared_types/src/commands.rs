//! The Spool action vocabulary.
//!
//! Every way of telling the window manager to do something funnels through
//! [`Action`]: the built-in Bar, the `spool action` interface, an
//! embedded Lua `init.lua`, and the loadable Lua client module. This crate owns
//! the types and their argv encoding ([`parse_action`] / [`Action::to_argv`]).

use serde::{Deserialize, Serialize};

pub use crate::argv::{ParseError, parse_action};

/// Represents a cardinal or directional choice for window manipulation.
///
/// Deserializes from either a direction name (`"east"`) or a 1-based position
/// (`3`, meaning the third column), which is what both the argv encoding and the
/// Lua API accept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    North,
    South,
    West,
    East,
    First,
    Last,
    Nth(usize),
}

impl Direction {
    #[must_use]
    pub fn reverse(&self) -> Self {
        match self {
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
            Direction::First => Direction::Last,
            Direction::Last => Direction::First,
            Direction::Nth(index) => Direction::Nth(*index),
        }
    }

    /// Parses a direction name. Positions are not accepted; see
    /// [`Direction::parse_positional`].
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if `name` is not a recognized direction name.
    pub fn parse(name: &str) -> Result<Self, ParseError> {
        Ok(match name {
            "north" => Direction::North,
            "south" => Direction::South,
            "west" => Direction::West,
            "east" => Direction::East,
            "first" => Direction::First,
            "last" => Direction::Last,
            other => return Err(ParseError::new(format!("unhandled direction '{other}'"))),
        })
    }

    /// Parses a direction name or a 1-based position (`"3"` → `Nth(2)`).
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if `input` is neither a recognized direction name
    /// nor a positive integer.
    pub fn parse_positional(input: &str) -> Result<Self, ParseError> {
        match input.parse::<usize>() {
            Ok(0) => Err(ParseError::new("window numbers start at 1")),
            Ok(number) => Ok(Direction::Nth(number - 1)),
            Err(_) => Direction::parse(input),
        }
    }

    /// The argv token this direction encodes to.
    #[must_use]
    pub fn token(&self) -> String {
        match self {
            Direction::North => "north".into(),
            Direction::South => "south".into(),
            Direction::West => "west".into(),
            Direction::East => "east".into(),
            Direction::First => "first".into(),
            Direction::Last => "last".into(),
            Direction::Nth(index) => (index + 1).to_string(),
        }
    }
}

/// The plain, externally tagged spelling of [`Direction`], used on the wire.
///
/// A binary format cannot decode the flexible `"east"`-or-`3` form below —
/// `untagged` works by asking the format what the next value *is*, which only a
/// self-describing one can answer. This mirror carries the same variants with a
/// derived impl, so the wire gets a discriminant and a payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DirectionRepr {
    North,
    South,
    West,
    East,
    First,
    Last,
    Nth(usize),
}

impl From<DirectionRepr> for Direction {
    fn from(repr: DirectionRepr) -> Self {
        match repr {
            DirectionRepr::North => Self::North,
            DirectionRepr::South => Self::South,
            DirectionRepr::West => Self::West,
            DirectionRepr::East => Self::East,
            DirectionRepr::First => Self::First,
            DirectionRepr::Last => Self::Last,
            DirectionRepr::Nth(index) => Self::Nth(index),
        }
    }
}

impl<'de> Deserialize<'de> for Direction {
    /// From a human-readable format, accepts `"east"` or `3`, so a Lua caller
    /// can write either `{ direction = "east" }` or `{ number = 3 }` and get the
    /// same enum. From a binary one — the wire — reads the derived spelling,
    /// because the flexible form is undecodable there and unnecessary: nothing
    /// hand-writes a request.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The two spellings a human-readable format accepts.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Name(String),
            Position(i64),
        }

        if !deserializer.is_human_readable() {
            return DirectionRepr::deserialize(deserializer).map(Direction::from);
        }

        match Repr::deserialize(deserializer)? {
            Repr::Name(name) => Direction::parse(&name).map_err(serde::de::Error::custom),
            Repr::Position(number) => usize::try_from(number)
                .ok()
                .filter(|number| *number > 0)
                .map(|number| Direction::Nth(number - 1))
                .ok_or_else(|| serde::de::Error::custom("window numbers start at 1")),
        }
    }
}

/// Axis affected by a window resize action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeAxis {
    Width,
    Height,
}

impl ResizeAxis {
    /// # Errors
    ///
    /// Returns [`ParseError`] if `input` is not a recognized resize axis.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        Ok(match input {
            "width" => Self::Width,
            "height" => Self::Height,
            other => return Err(ParseError::new(format!("unhandled resize axis '{other}'"))),
        })
    }

    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}

/// Direction used when growing or shrinking a window dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeDirection {
    Grow,
    Shrink,
}

/// Direction used when focusing the next window in a stable tier order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusStep {
    Next,
    Previous,
}

impl FocusStep {
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Previous => "previous",
        }
    }
}

impl ResizeDirection {
    /// # Errors
    ///
    /// Returns [`ParseError`] if `input` is not a recognized resize direction.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        Ok(match input {
            "grow" => ResizeDirection::Grow,
            "shrink" => ResizeDirection::Shrink,
            other => {
                return Err(ParseError::new(format!(
                    "unhandled resize direction '{other}'"
                )));
            }
        })
    }

    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            ResizeDirection::Grow => "grow",
            ResizeDirection::Shrink => "shrink",
        }
    }
}

/// Controls whether focus follows the window after a move operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveFocus {
    Follow,
    Stay,
}

/// Which side of an existing layout column receives a dragged column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Before,
    After,
}

impl MoveFocus {
    /// `follow = true` is the default everywhere a caller can choose.
    #[must_use]
    pub fn follows(follow: bool) -> Self {
        if follow { Self::Follow } else { Self::Stay }
    }
}

/// Defines the various operations that can be performed on windows.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Focuses on a window in the specified `Direction`.
    Focus(Direction),
    /// Moves the current window in the specified `Direction`. Tiled windows
    /// are reordered in the layout; floating windows move geometrically.
    Move(Direction),
    /// Centers the currently focused window on the display.
    Center,
    /// Grows or shrinks one dimension of the focused window.
    Resize {
        axis: ResizeAxis,
        direction: ResizeDirection,
    },
    /// Resizes the focused window to an exact display-width ratio.
    SetWidth(f64),
    /// Maximizes the focused window, or restores its previous size.
    Maximize,
    /// Moves the focused window to the next available display.
    ToNextDisplay(MoveFocus),
    /// Distributes heights equally among windows in the focused stack.
    Equalize,
    /// Makes all columns in the active strip the same width as the focused window.
    Balance,
    /// Toggles the focused window between tiled and floating layout modes.
    ToggleFloating,
    /// Toggles the focused tiled item between an independent column and a
    /// stack on its left.
    ToggleStack,
    /// Resizes and repositions the focused window to fit within the visible viewport
    /// (including edge padding).
    Snap,
    /// Focuses the Space's last-focused floating window.
    FocusFloating,
    /// Focuses the Space's last-focused tiled window.
    FocusTiled,
    /// Focuses the other tiled/floating tier, raising it as part of focus.
    FocusOtherLayer,
    /// Focuses the next or previous window in the current tiled/floating tier.
    /// Append new operations below this variant to preserve postcard discriminants.
    FocusStep(FocusStep),
    /// Parks/restores the current Space's tiled windows at the screen edge.
    ToggleTiledVisibility,
}

/// Defines operations that can be performed on the mouse.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMove {
    /// Moves the mouse pointer to the next available display.
    ToNextDisplay,
}

/// A serializable request for the window manager to perform a state transition
/// or runtime effect. Dispatching an action may legitimately be a no-op when
/// its preconditions are not met.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// An action targeting a window with a specific `Operation`.
    Window(Operation),
    /// An action targeting the mouse with a specific `MouseMove`.
    Mouse(MouseMove),
    /// Focuses the exact Spool-known window. If it belongs to a parked virtual
    /// workspace, that row is selected first.
    FocusWindow {
        window_id: i32,
    },
    /// Requests that macOS focus an existing native Space.
    FocusSpace {
        space_id: u64,
    },
    /// Requests that macOS move a window to an existing native Space.
    MoveWindowToSpace {
        window_id: i32,
        space_id: u64,
        move_focus: MoveFocus,
    },
    /// Requests creation of a native Space on a display.
    CreateSpace {
        display_id: u32,
    },
    /// Requests deletion of an existing native Space.
    DeleteSpace {
        space_id: u64,
    },
    /// Collapses or expands the Bar on the active display.
    ///
    /// Runtime-only presentation state: it is never persisted and every Bar
    /// starts expanded.
    ToggleBarCollapse,
    /// Quits the window manager application.
    Quit,
    /// Restarts the window manager service.
    Restart,
    PrintState,
    /// Reconciles Spool's tracked windows with the current macOS inventory.
    ReconcileWindows,
    /// Invokes a Lua keybind handler by its registry id (see the daemon's
    /// `crate::lua`). Never produced by parsing; the runtime issues it directly.
    Lua(u32),
    /// A snapshot-bound plan a Lua handler produced by transforming a `WindowSet`.
    /// Window-addressed, unlike every other action here, and applied
    /// best-effort: see `ecs::layout_ops`. Never produced by parsing.
    Layout(crate::windowset::LayoutPlan),
    /// Reorders the complete tiled column containing `window_id` relative to
    /// the complete tiled column containing `anchor_window_id`.
    ReorderColumn {
        window_id: i32,
        anchor_window_id: i32,
        placement: Placement,
    },
    /// Moves the complete tiled column containing `window_id` to another
    /// native Space while preserving stack/tab structure and member order.
    MoveColumnToSpace {
        window_id: i32,
        space_id: u64,
        move_focus: MoveFocus,
    },
    // IPC encodes actions and their nested operations by serde name.
    // Renaming a variant or field changes that wire contract.
    /// Requests the system Mission Control overview.
    MissionControl,
    /// Requests the system Show Desktop overview.
    ShowDesktop,
    /// Focuses the exact window, switching to `space_id` first when that is not
    /// the Space macOS is showing, and focusing it once it has arrived.
    ///
    /// The two halves cannot be split across a plain `FocusWindow` and a
    /// `FocusSpace`: `FocusWindow` refuses a window it cannot confirm on the
    /// visible Space, because focusing a window in a Space that is not up yet is
    /// unreliable, and a switch alone loses which window was meant.
    FocusWindowInSpace {
        window_id: i32,
        space_id: u64,
    },
}

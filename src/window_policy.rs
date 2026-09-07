//! Window identity admission and layout capability, independent of application purpose.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admission {
    Track,
    Ignore,
    Defer,
}

/// A missing AX attribute is not the same thing as an explicit `AXUnknown` value.
pub(crate) fn admission(
    role: Option<&str>,
    subrole: Option<&str>,
    parent_role: Option<&str>,
    track: Option<bool>,
) -> Admission {
    if track == Some(false) {
        return Admission::Ignore;
    }
    let Some(role) = role else {
        return Admission::Defer;
    };
    if role != "AXWindow" || matches!(parent_role, Some("AXWindow" | "AXSheet" | "AXDrawer")) {
        return Admission::Ignore;
    }
    if track == Some(true) {
        return Admission::Track;
    }
    match subrole {
        Some("AXStandardWindow" | "AXFloatingWindow" | "AXDialog" | "AXSystemDialog") => {
            Admission::Track
        }
        Some(_) => Admission::Ignore,
        None => Admission::Defer,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FloatReason {
    Rule,
    NotMovable,
    NotResizable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LayoutDecision {
    Tile,
    Float(FloatReason),
    Defer,
}

/// The strip currently needs both arbitrary movement and two-axis resizing.
/// A user can override a preference, but cannot manufacture an OS capability.
pub(crate) fn layout(
    movable: Option<bool>,
    resizable: Option<bool>,
    floating: bool,
) -> LayoutDecision {
    if floating {
        LayoutDecision::Float(FloatReason::Rule)
    } else if movable == Some(false) {
        LayoutDecision::Float(FloatReason::NotMovable)
    } else if resizable == Some(false) {
        LayoutDecision::Float(FloatReason::NotResizable)
    } else if movable == Some(true) && resizable == Some(true) {
        LayoutDecision::Tile
    } else {
        LayoutDecision::Defer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_dialogs_are_tracked_without_implying_float() {
        for subrole in [
            "AXStandardWindow",
            "AXFloatingWindow",
            "AXDialog",
            "AXSystemDialog",
        ] {
            assert_eq!(
                admission(Some("AXWindow"), Some(subrole), Some("AXApplication"), None),
                Admission::Track
            );
        }
        assert_eq!(layout(Some(true), Some(true), false), LayoutDecision::Tile);
    }

    #[test]
    fn force_track_cannot_promote_controls_or_attached_windows() {
        for role in ["AXMenu", "AXSheet", "AXButton"] {
            assert_eq!(
                admission(Some(role), Some("AXStandardWindow"), None, Some(true)),
                Admission::Ignore
            );
        }
        assert_eq!(
            admission(
                Some("AXWindow"),
                Some("AXStandardWindow"),
                Some("AXWindow"),
                Some(true)
            ),
            Admission::Ignore
        );
        assert_eq!(
            admission(Some("AXWindow"), Some("AXUnknown"), None, Some(true)),
            Admission::Track
        );
    }

    #[test]
    fn exclusion_and_unknown_metadata_are_distinct() {
        assert_eq!(admission(None, None, None, None), Admission::Defer);
        assert_eq!(
            admission(Some("AXWindow"), None, None, None),
            Admission::Defer
        );
        assert_eq!(
            admission(Some("AXWindow"), Some("AXUnknown"), None, None),
            Admission::Ignore
        );
        assert_eq!(admission(None, None, None, Some(false)), Admission::Ignore);
    }

    #[test]
    fn capability_matrix_never_tiles_an_unknown_or_unsupported_operation() {
        for movable in [None, Some(false), Some(true)] {
            for resizable in [None, Some(false), Some(true)] {
                assert_eq!(
                    layout(movable, resizable, false) == LayoutDecision::Tile,
                    movable == Some(true) && resizable == Some(true)
                );
                assert_eq!(
                    layout(movable, resizable, true),
                    LayoutDecision::Float(FloatReason::Rule)
                );
            }
        }
        assert_eq!(layout(None, Some(true), false), LayoutDecision::Defer);
        assert_eq!(
            layout(Some(true), Some(false), false),
            LayoutDecision::Float(FloatReason::NotResizable)
        );
    }
}

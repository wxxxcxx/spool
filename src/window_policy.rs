//! Window identity admission and layout capability, independent of application purpose.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admission {
    Track,
    TrackFloating,
    Ignore,
    Defer,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FallbackEvidence {
    pub geometry: Option<bool>,
    pub surface: Option<bool>,
    pub close_button: Option<bool>,
    pub minimize_button: Option<bool>,
    /// Whether the application can move this window through AX. A borderless
    /// window may carry no chrome at all and still be a window the user works in.
    pub movable: Option<bool>,
    pub resizable: Option<bool>,
}

pub(crate) fn admission_with_fallback(
    role: Option<&str>,
    subrole: Option<&str>,
    parent_role: Option<&str>,
    track: Option<bool>,
    evidence: FallbackEvidence,
) -> Admission {
    let base = admission(role, subrole, parent_role, track);
    // A rule that names this window is the user's own decision, in both
    // directions; anything that is not an `AXWindow` never gets here.
    if track.is_some() || role != Some("AXWindow") {
        return base;
    }
    if base == Admission::Track {
        // A subrole the policy knows is not enough on its own. macOS answers
        // `AXFloatingWindow` and `AXDialog` for its own panels too — Control
        // Center's panel is one — and those carry no window chrome and cannot be
        // moved or resized. Without that evidence they are surfaces, not windows
        // (see `docs/WINDOW_POLICY.md`).
        return match chrome_evidence(evidence) {
            Some(true) => Admission::Track,
            Some(false) => Admission::Ignore,
            None => Admission::Defer,
        };
    }
    if base != Admission::Ignore {
        return base;
    }
    match parent_role {
        Some("AXApplication") => {}
        None => return Admission::Defer,
        Some(_) => return Admission::Ignore,
    }
    let required = [
        evidence.geometry,
        evidence.surface,
        chrome_evidence(evidence),
    ];
    if required.contains(&Some(false)) {
        Admission::Ignore
    } else if required.contains(&None) {
        Admission::Defer
    } else {
        Admission::TrackFloating
    }
}

/// Whether the window carries evidence of being a window: window chrome, or a
/// frame the application can move and resize.
///
/// `None` means unknown, which defers rather than admits: an unreadable
/// `AXCloseButton` is not proof that a window has none.
fn chrome_evidence(evidence: FallbackEvidence) -> Option<bool> {
    if matches!(
        (evidence.movable, evidence.resizable),
        (Some(true), Some(true))
    ) {
        return Some(true);
    }
    match (evidence.close_button, evidence.minimize_button) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
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

    fn interactive_evidence() -> FallbackEvidence {
        FallbackEvidence {
            geometry: Some(true),
            surface: Some(true),
            close_button: Some(true),
            minimize_button: Some(false),
            movable: Some(true),
            resizable: Some(true),
        }
    }

    #[test]
    fn quick_look_and_other_independent_nonstandard_windows_use_floating_fallback() {
        for subrole in ["Quick Look", "AXUnknown", "CustomToolWindow"] {
            assert_eq!(
                admission_with_fallback(
                    Some("AXWindow"),
                    Some(subrole),
                    Some("AXApplication"),
                    None,
                    interactive_evidence()
                ),
                Admission::TrackFloating
            );
        }
    }

    #[test]
    fn fallback_does_not_admit_controls_attached_windows_or_explicit_exclusions() {
        for (role, parent, track) in [
            ("AXMenu", "AXApplication", None),
            ("AXSheet", "AXApplication", Some(true)),
            ("AXWindow", "AXWindow", None),
            ("AXWindow", "AXSheet", None),
            ("AXWindow", "AXDrawer", Some(true)),
            ("AXWindow", "AXApplication", Some(false)),
        ] {
            assert_eq!(
                admission_with_fallback(
                    Some(role),
                    Some("Quick Look"),
                    Some(parent),
                    track,
                    interactive_evidence()
                ),
                Admission::Ignore
            );
        }
    }

    /// The case that started this: macOS answers `AXFloatingWindow` (and
    /// `AXDialog`) for its own panels — Control Center's panel is one — and those
    /// carry no window chrome and cannot be moved or resized. The subrole alone
    /// must not admit them.
    #[test]
    fn a_known_subrole_without_window_evidence_is_not_a_window() {
        let chrome_less = FallbackEvidence {
            close_button: Some(false),
            minimize_button: Some(false),
            movable: Some(false),
            resizable: Some(false),
            ..FallbackEvidence::default()
        };
        for subrole in [
            "AXStandardWindow",
            "AXFloatingWindow",
            "AXDialog",
            "AXSystemDialog",
        ] {
            assert_eq!(
                admission_with_fallback(
                    Some("AXWindow"),
                    Some(subrole),
                    Some("AXApplication"),
                    None,
                    chrome_less
                ),
                Admission::Ignore,
                "{subrole} without chrome or a movable, resizable frame is a surface"
            );
        }
        // A real window of the same subroles still passes, and a borderless window
        // the application can move and resize is window enough on its own.
        let with_button = FallbackEvidence {
            close_button: Some(true),
            minimize_button: Some(false),
            movable: Some(false),
            resizable: Some(false),
            ..FallbackEvidence::default()
        };
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("AXStandardWindow"),
                Some("AXApplication"),
                None,
                with_button
            ),
            Admission::Track
        );
        let borderless = FallbackEvidence {
            close_button: Some(false),
            minimize_button: Some(false),
            movable: Some(true),
            resizable: Some(true),
            ..FallbackEvidence::default()
        };
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("AXFloatingWindow"),
                Some("AXApplication"),
                None,
                borderless
            ),
            Admission::Track
        );
        // Unknown evidence defers rather than admitting: a failed read is not
        // proof that a window has no chrome.
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("AXStandardWindow"),
                Some("AXApplication"),
                None,
                FallbackEvidence::default()
            ),
            Admission::Defer
        );
        // A rule still decides alone, in both directions.
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("AXFloatingWindow"),
                Some("AXApplication"),
                Some(true),
                chrome_less
            ),
            Admission::Track
        );
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("AXStandardWindow"),
                Some("AXApplication"),
                Some(false),
                with_button
            ),
            Admission::Ignore
        );
    }

    #[test]
    fn fallback_requires_positive_geometry_surface_and_button_evidence() {
        for evidence in [
            FallbackEvidence {
                geometry: Some(false),
                ..interactive_evidence()
            },
            FallbackEvidence {
                surface: Some(false),
                ..interactive_evidence()
            },
            FallbackEvidence {
                close_button: Some(false),
                minimize_button: Some(false),
                movable: Some(false),
                resizable: Some(false),
                ..interactive_evidence()
            },
        ] {
            assert_eq!(
                admission_with_fallback(
                    Some("AXWindow"),
                    Some("Quick Look"),
                    Some("AXApplication"),
                    None,
                    evidence
                ),
                Admission::Ignore
            );
        }
        for evidence in [
            FallbackEvidence {
                geometry: None,
                ..interactive_evidence()
            },
            FallbackEvidence {
                surface: None,
                ..interactive_evidence()
            },
            // Unknown chrome means both senses of chrome: the buttons and the
            // ability to move and resize the frame.
            FallbackEvidence {
                close_button: None,
                minimize_button: None,
                movable: None,
                resizable: None,
                ..interactive_evidence()
            },
        ] {
            assert_eq!(
                admission_with_fallback(
                    Some("AXWindow"),
                    Some("Quick Look"),
                    Some("AXApplication"),
                    None,
                    evidence
                ),
                Admission::Defer
            );
        }
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("Quick Look"),
                None,
                None,
                interactive_evidence()
            ),
            Admission::Defer
        );
        assert_eq!(
            admission_with_fallback(
                Some("AXWindow"),
                Some("Quick Look"),
                Some("AXApplication"),
                None,
                FallbackEvidence {
                    close_button: None,
                    minimize_button: Some(true),
                    ..interactive_evidence()
                }
            ),
            Admission::TrackFloating
        );
    }

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

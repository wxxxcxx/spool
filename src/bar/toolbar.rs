use spool_shared_types::commands::Action;

use super::layout::Rect;

pub const TOOLBAR_WIDTH: f64 = 66.0;

pub fn width(mission_control: bool, show_desktop: bool) -> f64 {
    let count = u8::from(mission_control) + u8::from(show_desktop);
    if count == 0 {
        0.0
    } else {
        10.0 + f64::from(count) * 28.0
    }
}

pub fn configured_buttons(
    height: f64,
    mission_control: bool,
    show_desktop: bool,
) -> Vec<ToolbarButton> {
    let mut buttons = buttons(height)
        .into_iter()
        .filter(|button| match button.action {
            Action::MissionControl => mission_control,
            Action::ShowDesktop => show_desktop,
            _ => false,
        })
        .collect::<Vec<_>>();
    for (index, button) in buttons.iter_mut().enumerate() {
        button.rect.x = 5.0
            + (24.0 - button.rect.width) / 2.0
            + f64::from(u32::try_from(index).unwrap_or(0)) * 28.0;
    }
    buttons
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolbarButton {
    pub action: Action,
    pub symbol: &'static str,
    pub tooltip: &'static str,
    pub rect: Rect,
}

pub fn buttons(height: f64) -> [ToolbarButton; 2] {
    let size = (height - 4.0).clamp(14.0, 24.0).min(height);
    let rect = Rect {
        x: 5.0 + (24.0 - size) / 2.0,
        y: (height - size) / 2.0,
        width: size,
        height: size,
    };
    [
        ToolbarButton {
            action: Action::MissionControl,
            symbol: "rectangle.3.group",
            tooltip: "Mission Control",
            rect,
        },
        ToolbarButton {
            action: Action::ShowDesktop,
            symbol: "menubar.dock.rectangle",
            tooltip: "Show Desktop",
            rect: Rect {
                x: rect.x + 28.0,
                ..rect
            },
        },
    ]
}

pub fn separator(height: f64) -> Rect {
    Rect {
        x: TOOLBAR_WIDTH - 3.0,
        y: (height - 14.0) / 2.0,
        width: 1.0,
        height: 14.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_reclaims_width_and_moves_remaining_button_into_first_slot() {
        for mission_control in [false, true] {
            for show_desktop in [false, true] {
                for height in [18.0, 22.0, 24.0, 37.0, 64.0] {
                    let buttons = configured_buttons(height, mission_control, show_desktop);
                    let expected = usize::from(mission_control) + usize::from(show_desktop);
                    assert_eq!(buttons.len(), expected);
                    let width = width(mission_control, show_desktop);
                    assert_eq!(width > 0.0, expected > 0);
                    for button in &buttons {
                        assert!(button.rect.y >= 0.0);
                        assert!(button.rect.y + button.rect.height <= height);
                        assert!(button.rect.x + button.rect.width < width - 3.0);
                    }
                    if expected == 1 {
                        assert!(buttons[0].rect.x < 12.0);
                    }
                }
            }
        }
    }

    #[test]
    fn buttons_are_distinct_labeled_and_bounded_at_every_supported_height() {
        for height in [22.0, 34.0, 64.0] {
            let buttons = buttons(height);
            assert_eq!(buttons[0].action, Action::MissionControl);
            assert_eq!(buttons[1].action, Action::ShowDesktop);
            assert_ne!(buttons[0].symbol, buttons[1].symbol);
            assert!(buttons[0].rect.x + buttons[0].rect.width < buttons[1].rect.x);
            for button in buttons {
                assert!(!button.tooltip.is_empty());
                assert!(button.rect.y >= 0.0);
                assert!(button.rect.y + button.rect.height <= height);
                assert!(button.rect.x + button.rect.width < separator(height).x);
            }
        }
    }
}

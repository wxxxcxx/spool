use std::process::Command as ProcessCommand;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSFont, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSString};
use tracing::warn;

use crate::accessibility_prompt::{AccessibilitySetupAction, show_accessibility_setup};
use crate::commands::Action;
use crate::events::EventSender;
use crate::manager::request_ax_privilege;

#[derive(Debug, Clone)]
struct MenuActionTargetIvars {
    events: EventSender,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpoolMenuActionTarget"]
    #[ivars = MenuActionTargetIvars]
    #[derive(Debug)]
    struct MenuActionTarget;

    impl MenuActionTarget {
        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _: &NSMenuItem) {
            if let Err(error) = ProcessCommand::new("/usr/bin/open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                .spawn()
            {
                warn!(%error, "unable to open Accessibility settings");
            }
        }

        #[unsafe(method(showAccessibilityInstructions:))]
        fn show_accessibility_instructions(&self, _: &NSMenuItem) {
            let Some(main_thread_marker) = MainThreadMarker::new() else {
                warn!("unable to show Accessibility instructions outside the main thread");
                return;
            };
            if show_accessibility_setup(main_thread_marker) == AccessibilitySetupAction::Continue {
                request_ax_privilege();
            }
        }

        #[unsafe(method(quitSpool:))]
        fn quit_spool(&self, _: &NSMenuItem) {
            if let Err(error) = self.ivars().events.dispatch(Action::Quit) {
                warn!(%error, "unable to dispatch menu bar action");
            }
        }
    }
);

impl MenuActionTarget {
    fn new(mtm: MainThreadMarker, events: EventSender) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuActionTargetIvars { events });
        unsafe { msg_send![super(this), init] }
    }
}

/// Minimal status item shown only while Spool waits for Accessibility access.
pub struct MenuBarManager {
    mtm: MainThreadMarker,
    status_bar: Retained<NSStatusBar>,
    status_item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    action_target: Retained<MenuActionTarget>,
}

impl MenuBarManager {
    pub fn new_accessibility_required(mtm: MainThreadMarker, events: EventSender) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);
        let action_target = MenuActionTarget::new(mtm, events);
        menu.setAutoenablesItems(false);
        status_item.setMenu(Some(&menu));
        status_item.setVisible(true);

        let manager = Self {
            mtm,
            status_bar,
            status_item,
            menu,
            action_target,
        };
        manager.rebuild_accessibility_menu();
        manager.show_label();
        manager
    }

    fn rebuild_accessibility_menu(&self) {
        let status = self.add_item("Spool - Accessibility Required", None);
        status.setEnabled(false);
        let hint = self.add_item("Grant access; Spool will start automatically", None);
        hint.setEnabled(false);
        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item(
            "Show Setup Instructions...",
            Some(sel!(showAccessibilityInstructions:)),
        );
        self.add_item(
            "Open Accessibility Settings...",
            Some(sel!(openAccessibilitySettings:)),
        );
        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item("Quit Spool", Some(sel!(quitSpool:)));
    }

    fn add_item(&self, title: &str, action: Option<objc2::runtime::Sel>) -> Retained<NSMenuItem> {
        let item = unsafe {
            self.menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                action,
                &NSString::from_str(""),
            )
        };
        if action.is_some() {
            unsafe { item.setTarget(Some(&self.action_target)) };
        }
        item
    }

    fn show_label(&self) {
        let Some(button) = self.status_item.button(self.mtm) else {
            return;
        };
        let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("exclamationmark.triangle.fill"),
            None,
        );
        if let Some(icon) = &icon {
            icon.setTemplate(true);
        }
        button.setImage(icon.as_deref());
        button.setFont(Some(&NSFont::menuBarFontOfSize(8.0)));
        button.setImageHugsTitle(true);
        button.setTitle(&NSString::from_str("!"));
        button.setToolTip(Some(&NSString::from_str(
            "Spool requires Accessibility access",
        )));
    }
}

impl Drop for MenuBarManager {
    fn drop(&mut self) {
        self.status_bar.removeStatusItem(&self.status_item);
    }
}

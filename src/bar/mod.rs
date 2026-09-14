mod appkit;
mod drag;
mod layout;
mod model;
mod motion;
mod placement;
mod preferences;
mod runtime;
mod state;
mod toolbar;

use bevy::ecs::system::{Local, NonSendMut, Res, ResMut};

pub use appkit::BarManager;
use model::BarSnapshot;
pub(crate) use preferences::BarPreferences;

use state::BarStateParams;

type BarWindowIdentities = Vec<(u32, u64, i32)>;

pub(crate) fn update_bar(
    bar: Option<NonSendMut<BarManager>>,
    state: BarStateParams,
    config: Res<crate::config::Config>,
    mut diagnostic_windows: Local<Option<BarWindowIdentities>>,
) {
    let Some(mut bar) = bar else {
        return;
    };
    let Ok(snapshot) = state.extract() else {
        return;
    };
    if tracing::enabled!(target: "spool::bar", tracing::Level::DEBUG) {
        let windows = snapshot
            .displays
            .iter()
            .flat_map(|display| {
                display.spaces.iter().flat_map(move |space| {
                    space
                        .windows()
                        .map(|window| window.id)
                        .map(move |id| (display.id, space.id, id))
                })
            })
            .collect::<BarWindowIdentities>();
        if diagnostic_windows.as_ref() != Some(&windows) {
            tracing::debug!(target: "spool::bar", ?windows, "bar_window_identities");
            *diagnostic_windows = Some(windows);
        }
    }
    bar.update(snapshot, config.bar_preferences());
}

/// A Bar-side request raised by an action and applied on the next frame.
///
/// The action bus cannot touch the Bar directly: `BarManager` is a non-send
/// `AppKit` resource and Bar updates run in their own chain. A one-shot flag
/// keeps the action path free of platform types and still lands within a frame.
#[derive(bevy::ecs::resource::Resource, Default)]
pub struct BarRequests {
    toggle_collapse: bool,
}

impl BarRequests {
    pub fn request_toggle_collapse(&mut self) {
        self.toggle_collapse = true;
    }
}

/// Applies queued Bar requests on the frame they arrive, so a keybinding does
/// not wait for the Bar's own refresh cadence.
pub(crate) fn apply_bar_requests(
    bar: Option<NonSendMut<BarManager>>,
    mut requests: ResMut<BarRequests>,
) {
    if !std::mem::take(&mut requests.toggle_collapse) {
        return;
    }
    if let Some(mut bar) = bar {
        bar.toggle_collapse();
    }
}

pub(crate) fn animate_bar(bar: Option<NonSendMut<BarManager>>) {
    if let Some(mut bar) = bar {
        bar.animate();
    }
}

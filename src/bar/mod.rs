mod appkit;
mod drag;
mod layout;
mod model;
mod motion;
mod placement;
mod preferences;
mod state;
mod toolbar;

use bevy::ecs::system::{Local, NonSendMut, Res};

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
                        .chain(space.unresolved.iter().map(|surface| surface.id))
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

pub(crate) fn animate_bar(bar: Option<NonSendMut<BarManager>>) {
    if let Some(mut bar) = bar {
        bar.animate();
    }
}

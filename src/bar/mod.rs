mod appkit;
mod drag;
mod geometry;
mod gesture;
mod layout;
mod model;
mod motion;
mod placement;
mod preferences;
mod state;
mod toolbar;

use bevy::ecs::system::{Local, NonSendMut, Res, ResMut};

pub use appkit::BarManager;
use model::BarSnapshot;
pub(crate) use preferences::BarPreferences;

pub use geometry::BarGeometryStore;
/// Only the config test needs the value; the Bar uses it through `toolbar`.
#[cfg(test)]
pub(crate) use layout::GRIP_WIDTH;

use state::BarStateParams;

type BarWindowIdentities = Vec<(u32, u64, i32)>;

pub(crate) fn update_bar(
    bar: Option<NonSendMut<BarManager>>,
    state: BarStateParams,
    config: Res<crate::config::Config>,
    geometry: Res<BarGeometryStore>,
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
    bar.update(snapshot, config.bar_preferences(), &geometry);
}

/// Records what the user placed by hand, so the next frame already lays out
/// against it. Persisting is a separate, periodic step.
pub(crate) fn capture_bar_edits(
    bar: Option<NonSendMut<BarManager>>,
    mut geometry: ResMut<BarGeometryStore>,
) {
    let Some(mut bar) = bar else {
        return;
    };
    for (display_id, panel) in bar.take_panel_edits() {
        geometry.set(display_id, panel);
    }
}

/// Saves the Bar geometry on the same timer as the other state files, and costs
/// nothing on a run where the user never moved the Bar.
pub(crate) fn periodic_bar_geometry_save(store: Option<ResMut<BarGeometryStore>>) {
    if let Some(mut store) = store {
        store.save_if_dirty();
    }
}

/// Saves on the way out, so the last gesture of a session is not the one lost.
pub(crate) fn bar_geometry_cleanup_on_exit(
    mut exit_events: bevy::ecs::message::MessageReader<bevy::app::AppExit>,
    store: Option<ResMut<BarGeometryStore>>,
) {
    if exit_events.read().next().is_some()
        && let Some(mut store) = store
    {
        store.save_if_dirty();
    }
}

pub(crate) fn animate_bar(bar: Option<NonSendMut<BarManager>>) {
    if let Some(mut bar) = bar {
        bar.animate();
    }
}

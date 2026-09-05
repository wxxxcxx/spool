mod appkit;
mod drag;
mod layout;
mod model;
mod motion;
mod placement;
mod preferences;
mod state;
mod toolbar;

use bevy::ecs::system::{NonSendMut, Res};

pub use appkit::BarManager;
use model::BarSnapshot;
pub(crate) use preferences::BarPreferences;

use state::BarStateParams;

pub(crate) fn update_bar(
    bar: Option<NonSendMut<BarManager>>,
    state: BarStateParams,
    config: Res<crate::config::Config>,
) {
    let Some(mut bar) = bar else {
        return;
    };
    let Ok(snapshot) = state.extract() else {
        return;
    };
    bar.update(snapshot, config.bar_preferences());
}

pub(crate) fn animate_bar(bar: Option<NonSendMut<BarManager>>) {
    if let Some(mut bar) = bar {
        bar.animate();
    }
}

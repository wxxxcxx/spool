//! Restores the geometry of windows that existed before this Spool process.
//!
//! This session-local launch snapshot is deliberately separate from
//! [`super::state::SpoolState`], which persists Spool's layout for a later
//! launch. Native Space membership, visibility, focus and z-order remain owned
//! by macOS and are never changed during exit restoration.

use std::cmp::Reverse;

use bevy::app::AppExit;
use bevy::ecs::component::Component;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Without;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, SystemParam};
use bevy::math::IRect;
use objc2_core_graphics::CGDirectDisplayID;
use tracing::{debug, info, warn};

use super::DockPosition;
use super::layout::clamp_origin_to_viewport;
use super::native_space::{NativeSpace, SpaceKind};
use crate::config::Config;
use crate::ecs::reconcile::WindowUnavailable;
use crate::manager::{Application, Display, Window, WindowManager};
use crate::platform::{Pid, ProcessSerialNumber, WinID, WindowIncarnation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FullscreenState {
    Windowed,
    Fullscreen,
    Unknown,
}

/// Present from the moment a graceful exit is observed until the app stops.
/// Geometry-producing systems use it to yield ownership to exit restoration.
#[derive(Resource)]
pub(crate) struct ExitInProgress;

pub(super) fn begin_exit(
    mut exit_events: MessageReader<AppExit>,
    exiting: Option<Res<ExitInProgress>>,
    mut commands: Commands,
) {
    if exiting.is_none() && exit_events.read().next().is_some() {
        commands.insert_resource(ExitInProgress);
    }
}

/// The exact pre-Spool frame and identity of one confirmed windowed startup
/// window. Absence of this component means exit restoration must leave the
/// window alone.
#[derive(Clone, Component, Copy, Debug)]
pub(crate) struct LaunchWindowSnapshot {
    window_id: WinID,
    incarnation: WindowIncarnation,
    pid: Pid,
    psn: ProcessSerialNumber,
    frame: IRect,
    display_id: CGDirectDisplayID,
    display_bounds: IRect,
}

fn fullscreen_state(
    window: &Window,
    spaces: &Query<&NativeSpace>,
    window_manager: &WindowManager,
) -> FullscreenState {
    match window.try_is_full_screen() {
        Ok(true) => return FullscreenState::Fullscreen,
        Ok(false) => {}
        Err(error) => {
            debug!(window_id = window.id(), %error, "unable to confirm fullscreen state");
            return FullscreenState::Unknown;
        }
    }

    for space in spaces
        .iter()
        .filter(|space| space.kind == SpaceKind::Fullscreen)
    {
        match window_manager.windows_in_workspace(space.id) {
            Ok(window_ids) if window_ids.contains(&window.id()) => {
                return FullscreenState::Fullscreen;
            }
            Ok(_) => {}
            Err(error) => {
                debug!(
                    window_id = window.id(),
                    space_id = space.id,
                    %error,
                    "unable to confirm fullscreen Space membership"
                );
                return FullscreenState::Unknown;
            }
        }
    }

    FullscreenState::Windowed
}

/// The capture side of the exit-restore module. Startup discovery passes each
/// window through this interface immediately after the authoritative AX frame
/// read and before the entity becomes visible to layout systems.
#[derive(SystemParam)]
pub(crate) struct LaunchCapture<'w, 's> {
    displays: Query<'w, 's, &'static Display>,
    spaces: Query<'w, 's, &'static NativeSpace>,
    window_manager: Res<'w, WindowManager>,
}

impl LaunchCapture<'_, '_> {
    pub(crate) fn capture(
        &self,
        window: &Window,
        application: &Application,
        frame: IRect,
    ) -> Option<LaunchWindowSnapshot> {
        if fullscreen_state(window, &self.spaces, &self.window_manager) != FullscreenState::Windowed
        {
            return None;
        }
        let Ok(pid) = window.pid() else {
            return None;
        };
        if pid != application.pid() {
            return None;
        }

        let Some((display_id, display_bounds)) = self
            .displays
            .iter()
            .map(|display| {
                let overlap = frame.intersect(display.bounds());
                let width = overlap.width().max(0);
                let height = overlap.height().max(0);
                let area = i64::from(width) * i64::from(height);
                (display.id(), display.bounds(), area)
            })
            .filter(|(_, _, area)| *area > 0)
            .max_by_key(|(display_id, _, area)| (*area, Reverse(*display_id)))
            .map(|(display_id, display_bounds, _)| (display_id, display_bounds))
        else {
            debug!(
                window_id = window.id(),
                ?frame,
                "startup window has no present display"
            );
            return None;
        };

        debug!(
            window_id = window.id(),
            ?frame,
            display_id,
            "captured launch window frame"
        );
        Some(LaunchWindowSnapshot {
            window_id: window.id(),
            incarnation: window.incarnation(),
            pid,
            psn: application.psn(),
            frame,
            display_id,
            display_bounds,
        })
    }
}

/// Applies the launch snapshot after every ordinary layout/animation commit in
/// the final schedule, making this the last geometry writer of a graceful exit.
pub(super) fn restore_launch_windows(
    mut exit_events: MessageReader<AppExit>,
    mut windows: Query<(&mut Window, &ChildOf, &LaunchWindowSnapshot), Without<WindowUnavailable>>,
    applications: Query<&Application>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    spaces: Query<&NativeSpace>,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
) {
    if exit_events.read().next().is_none() {
        return;
    }

    let mut restored = 0;
    for (mut window, child, snapshot) in &mut windows {
        let Ok(application) = applications.get(child.parent()) else {
            continue;
        };
        let identity_matches = window.id() == snapshot.window_id
            && window.incarnation() == snapshot.incarnation
            && window.pid().is_ok_and(|pid| pid == snapshot.pid)
            && application.pid() == snapshot.pid
            && application.psn() == snapshot.psn
            && application.is_running().is_ok_and(|running| running)
            && application.owns_window(&window).is_ok_and(|owned| owned);
        if !identity_matches {
            debug!(
                window_id = window.id(),
                "launch snapshot identity is no longer live"
            );
            continue;
        }
        if fullscreen_state(&window, &spaces, &window_manager) != FullscreenState::Windowed {
            continue;
        }

        let Some((display, dock)) = displays
            .iter()
            .find(|(display, _)| display.id() == snapshot.display_id)
        else {
            debug!(
                window_id = window.id(),
                display_id = snapshot.display_id,
                "launch display is no longer present"
            );
            continue;
        };

        let target = if display.bounds() == snapshot.display_bounds {
            snapshot.frame
        } else {
            let size = snapshot.frame.size();
            let viewport = display.actual_display_bounds(dock, &config);
            let origin = clamp_origin_to_viewport(snapshot.frame.min, size, viewport);
            IRect::from_corners(origin, origin + size)
        };

        match window.set_frame(target) {
            Ok(observed) => {
                restored += 1;
                if observed != target {
                    warn!(
                        window_id = window.id(),
                        ?target,
                        ?observed,
                        "application constrained launch-frame restoration"
                    );
                }
            }
            Err(error) => {
                warn!(window_id = window.id(), %error, "unable to restore launch window frame");
            }
        }
    }

    info!(restored, "exit cleanup restored launch window frames");
}

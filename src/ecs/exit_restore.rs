//! Restores launch geometry and brings tracked windows inside present displays.
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
use bevy::ecs::system::{Commands, Populated, Query, Res, SystemParam};
use bevy::math::{IRect, IVec2};
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
/// window. Without a snapshot, exit only constrains the current geometry.
#[derive(Clone, Component, Copy, Debug)]
pub(crate) struct LaunchWindowSnapshot {
    window_id: WinID,
    incarnation: WindowIncarnation,
    pid: Pid,
    psn: ProcessSerialNumber,
    frame: IRect,
    display_id: CGDirectDisplayID,
}

fn nearest_viewport(
    frame: IRect,
    viewports: &[(CGDirectDisplayID, IRect)],
) -> Option<(CGDirectDisplayID, IRect)> {
    viewports.iter().copied().min_by_key(|(id, viewport)| {
        let overlap = frame.intersect(*viewport);
        let area = i64::from(overlap.width().max(0)) * i64::from(overlap.height().max(0));
        let dx = (i64::from(viewport.min.x) - i64::from(frame.max.x))
            .max(i64::from(frame.min.x) - i64::from(viewport.max.x))
            .max(0);
        let dy = (i64::from(viewport.min.y) - i64::from(frame.max.y))
            .max(i64::from(frame.min.y) - i64::from(viewport.max.y))
            .max(0);
        (
            Reverse(area),
            i128::from(dx).pow(2) + i128::from(dy).pow(2),
            *id,
        )
    })
}

fn fit_frame(frame: IRect, viewport: IRect) -> IRect {
    let size = frame.size().clamp(IVec2::ONE, viewport.size());
    let origin = clamp_origin_to_viewport(frame.min, size, viewport);
    IRect::from_corners(origin, origin + size)
}

fn restore_frame(window: &mut Window, target: IRect, viewport: IRect) -> bool {
    let mut observed = match window.set_frame(target) {
        Ok(frame) => frame,
        Err(error) => {
            warn!(window_id = window.id(), %error, "unable to restore exit window frame");
            return false;
        }
    };
    if observed.intersect(viewport) != observed {
        // A minimum size may reject shrinking. One bounded retry corrects the
        // origin using the accepted size and keeps an oversized title bar reachable.
        let size = observed.size();
        let mut origin = clamp_origin_to_viewport(observed.min, size, viewport);
        if size.x > viewport.width() {
            origin.x = viewport.min.x;
        }
        if size.y > viewport.height() {
            origin.y = viewport.min.y;
        }
        let corrected = IRect::from_corners(origin, origin + size);
        if corrected != observed {
            match window.set_frame(corrected) {
                Ok(frame) => observed = frame,
                Err(error) => {
                    warn!(window_id = window.id(), %error, "unable to correct exit window position");
                }
            }
        }
    }
    if observed != target {
        warn!(
            window_id = window.id(),
            ?target,
            ?observed,
            "application constrained exit-frame restoration"
        );
    }
    let inside =
        observed.width() > 0 && observed.height() > 0 && observed.intersect(viewport) == observed;
    if !inside {
        warn!(
            window_id = window.id(),
            ?observed,
            ?viewport,
            "application could not fit its window inside the exit viewport"
        );
    }
    inside
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
    // Discovery retries early spawns once displays exist. Do not admit a
    // startup window before its pre-layout launch frame can be captured.
    displays: Populated<'w, 's, &'static Display>,
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

        let viewports = self
            .displays
            .iter()
            .map(|display| (display.id(), display.bounds()))
            .filter(|(_, bounds)| bounds.width() > 0 && bounds.height() > 0)
            .collect::<Vec<_>>();
        let Some((display_id, _)) = nearest_viewport(frame, &viewports) else {
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
        })
    }
}

/// Applies the launch snapshot after every ordinary layout/animation commit in
/// the final schedule, making this the last geometry writer of a graceful exit.
pub(super) fn restore_launch_windows(
    mut exit_events: MessageReader<AppExit>,
    mut windows: Query<
        (&mut Window, &ChildOf, Option<&LaunchWindowSnapshot>),
        Without<WindowUnavailable>,
    >,
    applications: Query<&Application>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    spaces: Query<&NativeSpace>,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
) {
    if exit_events.read().next().is_none() {
        return;
    }

    let viewports = displays
        .iter()
        .filter_map(|(display, dock)| {
            display
                .checked_actual_display_bounds(dock, &config)
                .map(|viewport| (display.id(), viewport))
        })
        .collect::<Vec<_>>();
    let mut restored = 0;
    for (mut window, child, snapshot) in &mut windows {
        let Ok(application) = applications.get(child.parent()) else {
            continue;
        };
        let identity_matches = snapshot.is_none_or(|snapshot| {
            window.id() == snapshot.window_id
                && window.incarnation() == snapshot.incarnation
                && application.pid() == snapshot.pid
                && application.psn() == snapshot.psn
        }) && window.pid().is_ok_and(|pid| pid == application.pid())
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

        let current = match window.update_frame() {
            Ok(frame) => frame,
            Err(error) => {
                warn!(window_id = window.id(), %error, "unable to read exit window frame");
                continue;
            }
        };
        // If the original display disappeared, preserve the current placement
        // when possible instead of moving the window to an obsolete coordinate.
        let launch_target = snapshot.and_then(|snapshot| {
            viewports
                .iter()
                .find(|(id, _)| *id == snapshot.display_id)
                .map(|(_, viewport)| (snapshot.frame, *viewport))
        });
        let Some((frame, viewport)) = launch_target.or_else(|| {
            nearest_viewport(current, &viewports).map(|(_, viewport)| (current, viewport))
        }) else {
            warn!(
                window_id = window.id(),
                "no usable display for exit restoration"
            );
            continue;
        };
        let target = fit_frame(frame, viewport);
        if target == current {
            continue;
        }

        if restore_frame(&mut window, target, viewport) {
            restored += 1;
        }
    }

    info!(
        restored,
        "exit cleanup restored window frames inside present displays"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_frame_fits_negative_coordinate_and_dock_limited_viewports() {
        for viewport in [IRect::new(-1600, -880, 0, 0), IRect::new(60, 30, 1000, 700)] {
            for x in [-3000, -800, 0, 1400] {
                for y in [-1800, 0, 600, 1200] {
                    for size in [IVec2::new(400, 300), IVec2::new(3000, 2000)] {
                        let frame = IRect::from_corners(IVec2::new(x, y), IVec2::new(x, y) + size);
                        let fitted = fit_frame(frame, viewport);
                        assert_eq!(fitted.intersect(viewport), fitted);
                        assert_eq!(fit_frame(fitted, viewport), fitted);
                    }
                }
            }
        }
    }

    #[test]
    fn exit_display_selection_prefers_overlap_then_distance_and_stable_id() {
        let left = (2, IRect::new(-1600, -880, 0, 0));
        let right = (1, IRect::new(0, 20, 1024, 768));
        assert_eq!(
            nearest_viewport(IRect::new(-800, -800, -400, -400), &[right, left]),
            Some(left)
        );
        assert_eq!(
            nearest_viewport(IRect::new(-2400, -700, -2000, -300), &[right, left]),
            Some(left)
        );
        assert_eq!(
            nearest_viewport(IRect::new(1600, 100, 2000, 500), &[left, right]),
            Some(right)
        );
        assert_eq!(nearest_viewport(IRect::new(0, 0, 100, 100), &[]), None);
    }
}

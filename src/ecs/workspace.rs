use bevy::app::{App, Last, Plugin, PostUpdate, PreUpdate, Update};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::Add;
use bevy::ecs::message::MessageReader;
use bevy::ecs::observer::On;
use bevy::ecs::query::{Added, Has, With};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::schedule::common_conditions::{not, resource_exists};
use bevy::ecs::system::{Commands, Local, Populated, Query, Res, ResMut, Single};
use bevy::time::common_conditions::on_timer;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{Level, debug, error, instrument, warn};

use super::{ActiveDisplayMarker, SpawnWindowTrigger};
use crate::config::Config;
use crate::ecs::focus::FocusHistory;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::native_space;
use crate::ecs::params::{WindowCtx, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, DockPosition, Floating, Initializing, NativeFullscreenMarker,
    Position, RefreshWindowSizes, SpawnCommandsExt, Timeout,
};
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Application, Display, Size, Window, WindowManager};
use crate::platform::{WinID, WorkspaceId};

pub struct WorkspaceEventsPlugin;

impl Plugin for WorkspaceEventsPlugin {
    fn build(&self, app: &mut App) {
        const REFRESH_WINDOW_CHECK_FREQ_MS: u64 = 1000;
        const DISPLAY_CHANGE_CHECK_FREQ_MS: u64 = 1000;

        app.init_resource::<native_space::NativeSpaceTransactions>();
        app.add_systems(
            PreUpdate,
            (
                native_space::handle_focus_window_commands,
                native_space::handle_native_space_commands,
            ),
        );
        app.add_systems(
            Update,
            (
                workspace_change_handler,
                workspace_created_handler,
                detect_moved_windows.run_if(not(resource_exists::<Initializing>)),
                refresh_workspace_window_sizes.run_if(on_timer(Duration::from_millis(
                    REFRESH_WINDOW_CHECK_FREQ_MS,
                ))),
                find_orphaned_workspaces
                    .after(crate::ecs::display::reconcile_displays)
                    .run_if(on_timer(Duration::from_millis(
                        DISPLAY_CHANGE_CHECK_FREQ_MS,
                    ))),
            ),
        );
        app.add_systems(PostUpdate, workspace_destroyed_handler);
        app.add_systems(
            Last,
            (
                native_space::reconcile_native_spaces,
                native_space::reconcile_native_space_transactions,
            )
                .chain(),
        );
        app.add_observer(cleanup_active_workspace_marker);
    }
}

fn fullscreen_window_in_strip(
    workspace_id: WorkspaceId,
    strip: &LayoutStrip,
    windows: &Windows,
    window_manager: &WindowManager,
) -> Option<Entity> {
    window_manager
        .windows_in_workspace(workspace_id)
        .ok()
        .and_then(|window_ids| {
            window_ids.into_iter().find_map(|window_id| {
                windows
                    .find_tiled(window_id)
                    .map(|(_, entity)| entity)
                    .filter(|entity| strip.contains(*entity))
            })
        })
        .or_else(|| {
            windows.tiled_iter().find_map(|(window, entity, _)| {
                (strip.contains(entity) && window.is_full_screen()).then_some(entity)
            })
        })
        .or_else(|| {
            windows
                .focused()
                .map(|(_, entity)| entity)
                .filter(|entity| strip.contains(*entity))
        })
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn workspace_change_handler(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<ActiveWorkspaceMarker>)>,
    active_display: Single<(&Display, Entity), With<ActiveDisplayMarker>>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !messages
        .read()
        .any(|event| matches!(event, Event::SpaceChanged))
    {
        return;
    }
    let (active_display, display_entity) = *active_display;

    let Ok(workspace_id) = window_manager.active_display_space(active_display.id()) else {
        error!("Unable to get active workspace id!");
        return;
    };

    let mut remove_from = None;
    let mut insert_into = None;
    for (strip, entity, active) in &workspaces {
        if active && strip.id() == workspace_id {
            debug!("Workspace id {} already active", strip.id());
            return;
        }
        if active && strip.id() != workspace_id {
            debug!("Workspace id {} no longer active", strip.id());
            remove_from = Some(entity);
        }
        if !active && strip.id() == workspace_id {
            debug!("Workspace id {} is active", strip.id());
            insert_into = Some(entity);
        }
    }

    if insert_into.is_none() {
        // The Space may have been observed before its display association was
        // updated; find its sole strip by stable ID.
        insert_into = workspaces
            .iter()
            .find(|(strip, _, _)| strip.id() == workspace_id)
            .map(|(_, entity, _)| entity);
    }

    if insert_into.is_none()
        && let Some(old_space) = remove_from
        && window_manager.is_fullscreen_space(active_display.id())
        && let Ok((mut old_strip, old_strip_entity, _)) = workspaces.get_mut(old_space)
        && let Some(fullscreen_window) =
            fullscreen_window_in_strip(workspace_id, &old_strip, &windows, &window_manager)
        && let Ok(original_index) = old_strip.index_of(fullscreen_window)
    {
        debug!("workspace_change: space={workspace_id} fullscreen");

        let fullscreen_marker = NativeFullscreenMarker {
            layout_strip: old_strip_entity,
            workspace_id: old_strip.id(),
            index: original_index,
        };
        old_strip.remove(fullscreen_window);

        let fullscreen_strip = LayoutStrip::fullscreen(workspace_id, fullscreen_window);
        let entity = commands
            .spawn((
                Position(active_display.bounds().min),
                fullscreen_marker,
                fullscreen_strip,
                ChildOf(display_entity),
            ))
            .id();
        insert_into = Some(entity);
    }

    if let Some(into) = insert_into
        && let Ok(mut entity_commands) = commands.get_entity(into)
    {
        entity_commands.try_insert(ActiveWorkspaceMarker);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn detect_moved_windows(
    activated_workspace: Single<Entity, Added<ActiveWorkspaceMarker>>,
    mut workspaces: Query<(&mut LayoutStrip, Entity, Has<NativeFullscreenMarker>)>,
    apps: Query<&mut Application>,
    window_manager: Res<WindowManager>,
    mut ignored_windows: Local<HashSet<WinID>>,
    mut ctx: WindowCtx,
) {
    let Ok(workspace_id) = workspaces
        .get(*activated_workspace)
        .map(|strip| strip.0.id())
    else {
        return;
    };
    debug!("workspace {workspace_id}");

    let strips = workspaces
        .iter()
        .filter_map(|strip| (strip.0.id() == workspace_id).then_some(strip.0))
        .collect::<Vec<_>>();
    let find_window = |window_id| ctx.windows.find_tiled(window_id).map(|(_, entity)| entity);
    let Ok((moved_windows, mut unresolved)) =
        windows_not_in_strips(workspace_id, find_window, &strips, &window_manager).inspect_err(
            |err| {
                warn!("unable to get windows in the current workspace: {err}");
            },
        )
    else {
        return;
    };
    // Skip windows that Spool deliberately does not track.
    unresolved.retain(|window_id| {
        !ignored_windows.contains(window_id) && ctx.windows.find(*window_id).is_none()
    });

    if !unresolved.is_empty() {
        let unresolved_ids = unresolved.iter().copied().collect::<HashSet<_>>();
        // Retry unresolved window IDs: during startup bruteforce, windows on
        // inactive workspaces may have stale AX attributes (e.g. AXGroup instead
        // of AXWindow).  Now that this workspace is active, re-query each app's
        // window list — the AX data should be correct.
        let retry_windows = apps
            .into_iter()
            .flat_map(|app| {
                app.window_list(&ctx.config)
                    .into_iter()
                    .filter(|window| unresolved_ids.contains(&window.id()))
            })
            .collect::<Vec<_>>();
        if retry_windows.is_empty() {
            for id in unresolved_ids {
                ignored_windows.insert(id);
            }
        } else {
            debug!(
                "retrying unresolved windows: {}",
                retry_windows
                    .iter()
                    .map(|window| format!("{}", window.id()))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            ctx.commands.trigger(SpawnWindowTrigger(retry_windows));
        }
    }

    for entity in moved_windows {
        if workspaces
            .iter()
            .any(|(strip, _, fullscreen)| fullscreen && strip.contains(entity))
        {
            // Do not relocate fullscreen windows, this will happen
            // during the destructino of their workspace.
            continue;
        }

        debug!("Window {entity} moved to workspace {workspace_id}.");
        let moving_entities = workspaces
            .iter()
            .find_map(|(strip, _, _)| strip.tab_group(entity))
            .unwrap_or_else(|| vec![entity]);
        for (mut strip, strip_entity, _) in &mut workspaces {
            if strip_entity == *activated_workspace {
                strip.append_tab_group(&moving_entities);
            } else {
                for moving_entity in &moving_entities {
                    strip.remove(*moving_entity);
                }
            }
        }
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn workspace_destroyed_handler(
    mut messages: MessageReader<Event>,
    mut workspaces: Populated<(&mut LayoutStrip, Entity, Option<&NativeFullscreenMarker>)>,
    mut focus_history: ResMut<FocusHistory>,
    mut commands: Commands,
) {
    for event in messages.read() {
        let Event::SpaceDestroyed { space_id } = event else {
            continue;
        };
        focus_history.forget_workspace(*space_id);

        let Some((entity, fullscreen)) =
            workspaces.iter().find_map(|(strip, entity, fullscreen)| {
                let window = strip.first().ok().and_then(|col| col.top());
                (strip.id() == *space_id).then_some((entity, window.zip(fullscreen.cloned())))
            })
        else {
            continue;
        };

        if let Some((
            window,
            NativeFullscreenMarker {
                layout_strip,
                workspace_id,
                index,
            },
        )) = fullscreen
        {
            let mut strip = workspaces
                .iter_mut()
                .find_map(|(strip, entity, _)| (entity == layout_strip).then_some(strip));
            if strip.is_none() {
                strip = workspaces
                    .iter_mut()
                    .find_map(|(strip, _, _)| (strip.id() == workspace_id).then_some(strip));
            }

            debug!(
                "previously fullscreened window {entity} inserted at {}",
                index
            );
            if let Some(mut strip) = strip {
                strip.insert_at(index, window);
                commands.reshuffle_around(window);
            }
        }

        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            debug!("Workspace destroyed {space_id} {entity}");
            entity_commands.try_despawn();
        }
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn workspace_created_handler(
    mut messages: MessageReader<Event>,
    active_display: Single<(&Display, Entity), With<ActiveDisplayMarker>>,
    workspaces: Query<&LayoutStrip>,
    mut commands: Commands,
) {
    for event in messages.read() {
        let Event::SpaceCreated { space_id } = event else {
            continue;
        };

        if workspaces.into_iter().any(|strip| strip.id() == *space_id) {
            warn!("Workspace {space_id} already exists!");
            continue;
        }
        debug!("Workspace create {space_id}");
        let (active_display, display_entity) = *active_display;
        let strip = LayoutStrip::new(*space_id);
        let origin = active_display.bounds().min;
        commands.spawn_layout_strip(strip, origin, display_entity, false);
    }
}

fn windows_not_in_strips<F: Fn(WinID) -> Option<Entity>>(
    workspace_id: WorkspaceId,
    find_window: F,
    strips: &[&LayoutStrip],
    window_manager: &WindowManager,
) -> Result<(Vec<Entity>, Vec<WinID>)> {
    window_manager
        .windows_in_workspace(workspace_id)
        .map(|ids| {
            let mut moved = Vec::new();
            let mut unresolved = Vec::new();
            for id in ids {
                if let Some(entity) = find_window(id) {
                    // If window exists in any of the active workspace rows.
                    if strips.iter().any(|strip| strip.contains(entity)) {
                        continue;
                    }
                    moved.push(entity);
                } else {
                    unresolved.push(id);
                }
            }
            (moved, unresolved)
        })
}

#[instrument(level = Level::DEBUG, skip_all)]
fn find_orphaned_workspaces(
    orphans: Populated<(&LayoutStrip, Entity, &Timeout, Option<&ChildOf>), With<Timeout>>,
    displays: Populated<(&Display, Entity)>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    let present = window_manager.present_displays();

    for (orphan, orphan_entity, timeout, child) in orphans {
        if orphan.len() == 0 {
            if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
                cmd.try_despawn();
            }
            debug!("despawning empty orphan workspace {}", orphan.id());
            continue;
        }
        if child.is_some() {
            // Was reparented, remove timer.
            if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
                cmd.try_remove::<Timeout>();
                cmd.insert(RefreshWindowSizes::default());
            }
            debug!(
                "layout strip {} was re-parented, removing timeout.",
                orphan.id()
            );
            continue;
        }

        if timeout.timer.is_finished() {
            // Rescue windows from orphaned strips before despawning by floating them.
            debug!("Rescue windows from timed out orphan {}.", orphan.id());
            for lost_window in orphan.all_windows() {
                if let Ok(mut cmd) = commands.get_entity(lost_window) {
                    cmd.try_insert(Floating);
                }
            }
            continue;
        }

        // Find which display now owns this space ID.
        let target = present.iter().find_map(|(present_display, spaces)| {
            if spaces.iter().any(|&id| id == orphan.id()) {
                displays
                    .iter()
                    .find(|(d, _)| d.id() == present_display.id())
            } else {
                None
            }
        });
        let Some((target_display, target_entity)) = target else {
            continue; // No display owns this space yet; wait for next tick.
        };

        debug!(
            "Re-parenting orphaned strip {} to display {}",
            orphan.id(),
            target_display.id(),
        );

        if let Ok(mut cmd) = commands.get_entity(orphan_entity) {
            cmd.try_remove::<Timeout>()
                .insert(ChildOf(target_entity))
                .insert(RefreshWindowSizes::default());
        }
    }
}

fn refresh_workspace_window_sizes(
    layout_strip: Populated<(&RefreshWindowSizes, &LayoutStrip, Entity, &ChildOf)>,
    mut windows: Query<(Entity, &mut Window, &mut Bounds, Has<Floating>)>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    mut commands: Commands,
) {
    for (_, strip, strip_entity, child) in
        layout_strip.into_iter().filter(|marker| marker.0.ready())
    {
        debug!("refreshing workspace {} sizes", strip.id());
        let Ok((display, dock)) = displays.get(child.parent()) else {
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);

        let mut in_workspace = window_manager
            .windows_in_workspace(strip.id())
            .inspect_err(|err| {
                warn!("getting windows in workspace: {err}");
            })
            .unwrap_or_default();

        // Resize windows for the new display dimensions.
        for entity in strip.all_windows() {
            let Ok((_, ref mut window, ref mut bounds, _)) = windows.get_mut(entity) else {
                continue;
            };
            if bounds.x > viewport.width() || bounds.y > viewport.height() {
                let clamped_size = Size::new(
                    bounds.x.clamp(0, viewport.width()),
                    bounds.y.clamp(0, viewport.height()),
                );
                debug!("refreshing window {} size to {clamped_size}", window.id());
                commands.resize_entity(entity, clamped_size);
            }

            in_workspace.retain(|window_id| *window_id != window.id());
        }

        // Find remaining windows which are outside of the strip.                                                  ...
        let floating = in_workspace.into_iter().filter_map(|window_id| {
            windows.iter().find_map(|(entity, window, _, floating)| {
                (window_id == window.id() && floating).then_some(entity)
            })
        });
        for window_entity in floating {
            debug!("repositioning floating window {window_entity}");
            commands.reposition_entity(window_entity, viewport.min);
        }

        if let Ok(mut cmds) = commands.get_entity(strip_entity) {
            cmds.try_remove::<RefreshWindowSizes>();
        }
    }
}

/// Removes previuos `ActiveWorkspaceMarker`'s when a new one is inserted.
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn cleanup_active_workspace_marker(
    trigger: On<Add, ActiveWorkspaceMarker>,
    workspaces: Query<(Entity, Has<ActiveWorkspaceMarker>), With<LayoutStrip>>,
    mut commands: Commands,
) {
    workspaces.iter().for_each(|(entity, marker)| {
        if entity != trigger.entity
            && marker
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            // Remove the active marker from any other workspace.
            entity_commands.try_remove::<ActiveWorkspaceMarker>();
        }
    });
}

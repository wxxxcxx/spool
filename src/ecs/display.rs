use bevy::app::{App, Plugin, PreUpdate, Update};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::Add;
use bevy::ecs::message::MessageReader;
use bevy::ecs::observer::On;
use bevy::ecs::query::{Has, With};
use bevy::ecs::system::{Commands, Local, NonSend, Query, Res, SystemParam};
use bevy::platform::collections::HashSet;
use bevy::time::Time;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::CGDirectDisplayID;
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;
use tracing::{Level, debug, error, instrument, warn};

use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, DockPosition, ReadDisplayProperties,
    RefreshWindowSizes, SendMessageTrigger,
};
use crate::events::Event;
use crate::manager::{Display, DisplayObservation, WindowManager, irect_from};
use crate::platform::PlatformCallbacks;
use crate::util::{read_screen_property, round_px};

pub struct DisplayEventsPlugin;

impl Plugin for DisplayEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, display_change_handler);
        app.add_systems(
            Update,
            (reconcile_displays, refresh_display_properties_for_dock),
        )
        .add_observer(read_display_properties_trigger)
        .add_observer(cleanup_active_display_marker);
    }
}

/// Treat Dock notifications as invalidation hints and periodically reconcile
/// `NSScreen.visibleFrame` so a dropped or early notification cannot leave the
/// usable viewport stale. Repeated reads are cheap and only publish ECS changes
/// when the resulting display or Dock geometry actually differs.
fn refresh_display_properties_for_dock(
    mut messages: MessageReader<Event>,
    displays: Query<Entity, With<Display>>,
    time: Res<Time>,
    mut since_refresh: Local<Duration>,
    mut commands: Commands,
) {
    const DOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

    let invalidated = messages.read().any(|event| {
        matches!(
            event,
            Event::DockDidChangePref { .. } | Event::DockDidRestart { .. }
        )
    });
    *since_refresh = since_refresh.saturating_add(time.delta());
    if !invalidated && *since_refresh < DOCK_REFRESH_INTERVAL {
        return;
    }
    *since_refresh = Duration::ZERO;

    for entity in displays {
        commands.trigger(ReadDisplayProperties(entity));
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn cleanup_active_display_marker(
    trigger: On<Add, ActiveDisplayMarker>,
    displays: Query<(Entity, Has<ActiveDisplayMarker>), With<Display>>,
    mut commands: Commands,
) {
    for (entity, active) in displays {
        if active
            && entity != trigger.entity
            && let Ok(mut cmd) = commands.get_entity(entity)
        {
            debug!("Display id {entity} lost active marker.");
            cmd.try_remove::<ActiveDisplayMarker>();
        }
    }
}

/// Handles display change events.
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn display_change_handler(
    mut messages: MessageReader<Event>,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !messages
        .read()
        .any(|event| matches!(event, Event::DisplayChanged))
    {
        return;
    }

    let Ok(active_id) = window_manager.active_display_id() else {
        error!("Unable to get active display id!");
        return;
    };

    for (display, entity, focused) in displays {
        let display_id = display.id();
        if !focused
            && display_id == active_id
            && let Ok(mut cmd) = commands.get_entity(entity)
        {
            debug!("Display id {display_id} is active");
            cmd.try_insert(ActiveDisplayMarker);
        }
    }
    commands.trigger(SendMessageTrigger(Event::SpaceChanged));
}

#[derive(SystemParam)]
pub(crate) struct DisplayProjection<'w, 's> {
    workspaces: Query<'w, 's, (&'static LayoutStrip, Entity, Option<&'static ChildOf>)>,
    displays: Query<'w, 's, (&'static mut Display, Entity)>,
    active_strips: Query<'w, 's, Entity, (With<LayoutStrip>, With<ActiveWorkspaceMarker>)>,
}

/// Reconciles physical display inventory on invalidation and heartbeat. Space
/// lookup failures retain their display and its previous workspace projection.
pub(crate) fn reconcile_displays(
    mut messages: MessageReader<Event>,
    projection: DisplayProjection,
    topology: Res<super::topology::NativeTopology>,
    mut generation: Local<u64>,
    mut commands: Commands,
) {
    let DisplayProjection {
        workspaces,
        mut displays,
        active_strips,
    } = projection;

    let mut needs_reconcile = false;
    for event in messages.read() {
        needs_reconcile |= matches!(
            event,
            Event::SystemWoke { .. }
                | Event::DisplayAdded { .. }
                | Event::DisplayRemoved { .. }
                | Event::DisplayMoved { .. }
                | Event::DisplayResized { .. }
                | Event::DisplayConfigured { .. }
        );
    }
    if *generation == topology.generation() {
        return;
    }
    *generation = topology.generation();

    debug!("Reconciling displays against OS after wake / resize / configure");

    let Some(observed) = topology.displays() else {
        return;
    };
    let mut present_displays: HashMap<CGDirectDisplayID, _> = observed
        .iter()
        .map(|entry| (entry.display.id(), entry.clone()))
        .collect();

    let existing_displays: HashMap<CGDirectDisplayID, _> = displays
        .iter()
        .map(|(display, workspaces)| (display.id(), (display, workspaces)))
        .collect();

    let present_ids = present_displays.keys().copied().collect::<HashSet<_>>();
    let existing_ids = existing_displays.keys().copied().collect::<HashSet<_>>();
    let mut changed = present_ids != existing_ids;

    // Displays that vanished while we were away (e.g. unplugged during sleep).
    for display_id in existing_ids.difference(&present_ids) {
        let Some((display, _)) = existing_displays.get(display_id) else {
            error!("Unable to find removed display: {display_id}");
            continue;
        };
        remove_display(display, &workspaces, &displays, &mut commands);
    }

    // Displays that appeared while we were away.
    for display_id in present_ids.difference(&existing_ids) {
        let Some(observed) = present_displays.remove(display_id) else {
            error!("Unable to find added display: {display_id}");
            continue;
        };
        add_display(observed.display, &mut commands);
    }

    // Displays that are still present: refresh their bounds (resolution or
    // menubar may have changed). Native Space projection owns reparenting.
    for display_id in present_ids.intersection(&existing_ids) {
        let Some(observed) = present_displays.remove(display_id) else {
            continue;
        };
        changed |= move_display(observed, &mut displays, &mut commands);
    }

    // Re-tile the active workspace even when the topology is unchanged — the OS
    // shuffles window frames across a sleep/wake cycle.
    for entity in active_strips.iter().filter(|_| needs_reconcile) {
        if let Ok(mut cmd) = commands.get_entity(entity) {
            cmd.insert(RefreshWindowSizes::default());
        }
    }

    if changed || needs_reconcile {
        commands.trigger(SendMessageTrigger(Event::DisplayChanged));
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(display_id))]
fn add_display(display: Display, commands: &mut Commands) {
    let display_id = display.id();
    debug!("Display Added: {display_id}");

    let display_entity = commands.spawn(display).id();
    commands.trigger(ReadDisplayProperties(display_entity));
}

#[instrument(level = Level::DEBUG, skip_all, fields(display_id))]
fn remove_display(
    display: &Display,
    workspaces: &Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    displays: &Query<(&mut Display, Entity)>,
    commands: &mut Commands,
) {
    let display_id = display.id();
    debug!("Display Removed: {display_id:?}");
    let Some((display, display_entity)) = displays
        .into_iter()
        .find(|(display, _)| display.id() == display_id)
    else {
        error!("Unable to find removed display!");
        return;
    };

    for (strip, entity, _) in workspaces
        .into_iter()
        .filter(|(_, _, child)| child.is_some_and(|child| child.parent() == display_entity))
    {
        let display_id = display.id();
        debug!(
            "orphaning strip {} after removal of display {display_id}.",
            strip.id(),
        );
        if let Ok(mut commands) = commands.get_entity(entity) {
            commands.try_insert(super::native_space::DetachedSpace {
                source_display_id: display_id,
            });
        }
        if let Ok(mut commands) = commands.get_entity(display_entity) {
            commands.detach_child(entity);
        }
    }

    if let Ok(mut commands) = commands.get_entity(display_entity) {
        commands.try_despawn();
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
fn move_display(
    observed: DisplayObservation,
    displays: &mut Query<(&mut Display, Entity)>,
    commands: &mut Commands,
) -> bool {
    let display_id = observed.display.id();
    debug!("Display Moved: {display_id:?}");
    let Some((mut display, display_entity)) = displays
        .iter_mut()
        .find(|(display, _)| display.id() == display_id)
    else {
        error!("Unable to find moved display!");
        return false;
    };
    let changed = display
        .bypass_change_detection()
        .update_geometry(&observed.display);
    if changed {
        display.set_changed();
        commands.trigger(ReadDisplayProperties(display_entity));
    }

    changed
}

/// The last selected floating tier, owned by the canonical Space entity.
#[derive(Clone, Component, Copy, Default)]
pub struct FloatingLayer {
    pub front: bool,
}

fn read_display_properties_trigger(
    trigger: On<ReadDisplayProperties>,
    mut displays: Query<(&mut Display, Entity, Option<&DockPosition>)>,
    platform: Option<NonSend<Pin<Box<PlatformCallbacks>>>>,
    config: Option<Res<Config>>,
    mut commands: Commands,
) {
    let Ok((mut display, entity, current_dock)) = displays.get_mut(trigger.event().0) else {
        return;
    };
    let display_id = display.id();

    // NSScreen::screen needs to run in the main thread, thus we run it in a NonSend trigger.
    let Some(screens) = platform.map(|platform| NSScreen::screens(platform.main_thread_marker))
    else {
        return;
    };

    let notch = read_screen_property(&screens, display_id, |screen| {
        let insets = screen.safeAreaInsets();
        debug!("notch on display {display_id}: {insets:?}");
        round_px(insets.top)
    });

    let menubar_height = config.as_deref().and_then(Config::menubar_height);
    let previous_bounds = display.bounds();
    {
        let display = display.bypass_change_detection();
        if let Some(height) = notch {
            display.set_notch_height(height);
        }
        if config.is_some() {
            display.set_menubar_height_override(menubar_height);
        }
    }
    if display.bounds() != previous_bounds {
        display.set_changed();
    }

    let dock = read_screen_property(&screens, display_id, |screen| {
        let visible_frame = irect_from(screen.visibleFrame());
        display.locate_dock(&visible_frame)
    });
    if let Some(dock) = dock
        && current_dock.copied() != Some(dock)
    {
        debug!("dock on display {display_id}: {:?}", dock);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert(dock);
        }
    }
}

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use bevy::app::App as BevyApp;
use bevy::app::{First, Last, PostStartup, PostUpdate, PreStartup, PreUpdate, Startup};
use bevy::ecs::change_detection::DetectChanges as _;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::query::{Added, Changed, Or, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::common_conditions::{not, resource_exists};
use bevy::ecs::schedule::{ScheduleLabel as _, SingleThreadedExecutor, SystemCondition as _};
use bevy::ecs::system::{Commands, EntityCommands, Query, Res, SystemId};
use bevy::prelude::Event as BevyEvent;
use bevy::time::Timer;
use bevy::time::common_conditions::on_timer;
use bevy::time::{Time, Virtual};
use bevy::{
    app::Update,
    ecs::{component::Component, entity::Entity, schedule::IntoScheduleConfigs},
};
use derive_more::{Deref, DerefMut};
#[cfg(feature = "lua")]
use tracing::error;
use tracing::{Level, instrument, warn};

/// Whether a process is this daemon itself.
///
/// Spool must never track its own windows: they are its Bar and overlay
/// panels, not user windows. Auditing them costs a full accessibility
/// inventory of this process on every lifecycle heartbeat, and nothing about
/// them is actionable.
pub(crate) fn is_own_process(pid: Pid) -> bool {
    u32::try_from(pid).is_ok_and(|pid| pid == std::process::id())
}

type ChangedNativeSpaces<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        Added<native_space::NativeSpace>,
        Changed<native_space::NativeSpace>,
    )>,
>;

use crate::bar::BarManager;
use crate::commands::register_commands;
use crate::config::{Config, WindowParams};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::state::{SpoolState, StateFilePath};
use crate::errors::Result;
use crate::events::{Event, EventSender, FocusObservation, InputEvent};
#[cfg(feature = "lua")]
use crate::lua;
use crate::manager::discovery::WindowDiscovery;
use crate::manager::{
    Application, Display, Origin, ProcessApi, Size, Window, WindowManager, WindowManagerApi,
    WindowManagerOS,
};
use crate::overlay::{FlashMessageManager, OverlayManager};
use crate::platform::{Modifiers, Pid, PlatformCallbacks, WinID, WorkspaceId};

pub(crate) mod defaults;
pub mod display;
pub(crate) mod exit_restore;
pub mod focus;
pub mod layout;
#[cfg(feature = "lua")]
pub mod layout_ops;
pub(crate) mod layout_snapshot;
pub mod mouse;
pub mod native_space;
pub mod params;
pub(crate) mod reconcile;
pub(crate) mod restore;
pub mod script_state;
pub mod scroll;
pub mod state;
pub(crate) mod systems;
pub(crate) mod tiled_visibility;
pub(crate) mod topology;
mod triggers;
pub mod window_frame;
pub(crate) mod window_geometry;
pub mod workspace;

pub use window_frame::{
    DesiredWindowFrame, PresentedWindowFrame, WindowFrameCommitSuspended, WindowFrameMotion,
};

// Shared by the Lua reload system so a `spool.setup{...}` reload applies the
// menubar/passthrough side effects after a successful Lua reload.
#[cfg(feature = "lua")]
pub(crate) use triggers::apply_config_side_effects;

#[allow(clippy::too_many_arguments)]
fn overlay_dirty(
    strip_changed: Query<(), (With<ActiveWorkspaceMarker>, Changed<LayoutStrip>)>,
    focus_gained: Query<(), Added<FocusedMarker>>,
    workspace_changed: Query<(), Added<ActiveWorkspaceMarker>>,
    native_space_changed: ChangedNativeSpaces,
    focused_moved: Query<(), (With<FocusedMarker>, Changed<Position>)>,
    focused_resized: Query<(), (With<FocusedMarker>, Changed<Bounds>)>,
    // Overlay targets also include requested/navigation windows and hidden
    // Spaces. Their readbacks remain relevant while FocusedMarker is absent.
    observed_moved: Query<(), Changed<ObservedWindowFrame>>,
    config: Option<Res<Config>>,
    focus: Option<Res<focus::FocusCoordinator>>,
    mission_control: Option<Res<MissionControlActive>>,
    mut focus_lost: RemovedComponents<FocusedMarker>,
    mut workspace_lost: RemovedComponents<ActiveWorkspaceMarker>,
    mut observed_lost: RemovedComponents<ObservedWindowFrame>,
    mut native_space_removed: RemovedComponents<native_space::NativeSpace>,
    mut window_removed: RemovedComponents<Window>,
) -> bool {
    !strip_changed.is_empty()
        || !focus_gained.is_empty()
        || !workspace_changed.is_empty()
        || !native_space_changed.is_empty()
        || !focused_moved.is_empty()
        || !focused_resized.is_empty()
        || !observed_moved.is_empty()
        || config.is_some_and(|config| config.is_changed())
        || focus.is_some_and(|focus| focus.is_changed())
        || mission_control.is_some_and(|state| state.is_changed())
        || focus_lost.read().next().is_some()
        || workspace_lost.read().next().is_some()
        || observed_lost.read().next().is_some()
        || native_space_removed.read().next().is_some()
        || window_removed.read().next().is_some()
}

type BarGained = Or<(
    Added<native_space::VisibleNativeSpaceMarker>,
    Added<FocusedMarker>,
    Added<Floating>,
)>;
type BarAvailabilityChanged = Or<(
    Changed<WindowVisibility>,
    Added<reconcile::WindowUnavailable>,
)>;

/// Whether anything the Bar draws has changed since this last ran.
///
/// Deliberately not `Changed<Window>`: systems that tick a window's cached
/// frame or a lifecycle timer re-mark that component without changing anything
/// the Bar renders, and that alone made the Bar re-extract the whole native
/// Space projection at the frame rate.
///
/// The terms are the Bar's own projection: its strips and Spaces, the focused,
/// floating, hidden and unavailable states of the windows it lists, the app
/// identity behind an icon, the display frames it is placed in, and the config
/// it is drawn from. A window's cached frame and title are carried in the
/// snapshot but never drawn, so they deliberately do not appear.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy injects independent change readers as system parameters"
)]
pub(crate) fn bar_projection_dirty(
    layout_changed: Query<(), Changed<LayoutStrip>>,
    native_space_changed: ChangedNativeSpaces,
    gained: Query<(), BarGained>,
    hidden_or_unavailable: Query<(), BarAvailabilityChanged>,
    app_changed: Query<(), Changed<Application>>,
    display_changed: Query<(), Changed<Display>>,
    focus: Option<Res<focus::FocusCoordinator>>,
    config: Option<Res<Config>>,
    mut visible_space_lost: RemovedComponents<native_space::VisibleNativeSpaceMarker>,
    mut focus_lost: RemovedComponents<FocusedMarker>,
    mut floating_lost: RemovedComponents<Floating>,
    mut unavailable_lost: RemovedComponents<reconcile::WindowUnavailable>,
    mut visibility_lost: RemovedComponents<WindowVisibility>,
    mut native_space_removed: RemovedComponents<native_space::NativeSpace>,
    mut window_removed: RemovedComponents<Window>,
) -> bool {
    !layout_changed.is_empty()
        || !native_space_changed.is_empty()
        || !gained.is_empty()
        || !hidden_or_unavailable.is_empty()
        || !app_changed.is_empty()
        || !display_changed.is_empty()
        // A request the Bar draws before confirmation moves the indicator; it
        // is not a component change, so it has to be watched explicitly.
        || focus.is_some_and(|focus| focus.is_changed())
        || config.is_some_and(|config| config.is_changed())
        || visible_space_lost.read().next().is_some()
        || focus_lost.read().next().is_some()
        || floating_lost.read().next().is_some()
        || unavailable_lost.read().next().is_some()
        || visibility_lost.read().next().is_some()
        || native_space_removed.read().next().is_some()
        || window_removed.read().next().is_some()
}

/// Registers the Bevy systems for the `WindowManager`.
/// This function adds various systems to the `Update` schedule, including event dispatchers,
/// process/application/window lifecycle management, animation, and periodic watchers.
///
/// # Arguments
///
/// * `app` - The Bevy application to register the systems with.
#[allow(clippy::too_many_lines)]
pub fn register_systems(app: &mut bevy::app::App) {
    const LOW_POWER_MODE_CHECK_SEC: u64 = 60;

    app.init_resource::<reconcile::WindowStateSync>();
    app.init_resource::<window_geometry::WindowGeometrySettling>();
    app.init_resource::<topology::NativeTopology>();
    app.init_resource::<layout_snapshot::LayoutSession>();
    app.init_resource::<defaults::DefaultRetries>();
    app.init_resource::<state::StatePersistence>();

    let not_swiping = |scrolling: Query<&Scrolling, With<ActiveWorkspaceMarker>>| {
        scrolling
            .iter()
            .next()
            .is_none_or(|marker| !marker.is_user_swiping)
    };
    let bar_dirty = bar_projection_dirty.or_eager(on_timer(Duration::from_secs(1)));

    app.add_systems(
        Startup,
        (
            topology::gather_initial_topology,
            systems::gather_displays,
            native_space::reconcile_native_spaces,
            systems::gather_initial_processes,
        )
            .chain(),
    );
    // Registered with `add_message`, not `init_resource`, so the buffer is
    // double-buffered and dropped after a frame like any other message stream.
    app.add_message::<InputEvent>();
    app.add_systems(
        PreUpdate,
        (
            systems::window_creation_event,
            systems::pump_events,
            systems::demux_input_events.after(systems::pump_events),
            exit_restore::begin_exit.after(systems::pump_events),
        ),
    );
    app.add_systems(
        Update,
        (
            (
                defaults::refresh_default_retries.after(native_space::reconcile_native_spaces),
                triggers::apply_window_defaults,
                systems::commit_default_window_frames,
                triggers::apply_window_positions,
                triggers::retry_pending_retiles,
            )
                .chain()
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            (
                systems::add_existing_process,
                systems::add_existing_application,
                systems::advance_window_discovery,
                systems::finish_setup.run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            )
                .chain()
                .run_if(resource_exists::<Initializing>),
            systems::add_launched_process,
            systems::add_launched_application,
            systems::fresh_marker_cleanup,
            systems::timeout_ticker,
            systems::retry_front_switch.after(triggers::front_switched_trigger),
            systems::update_low_power_state
                .run_if(resource_exists::<LowPowerMode>)
                .run_if(on_timer(Duration::from_secs(LOW_POWER_MODE_CHECK_SEC))),
            (
                window_geometry::observe_external_window_geometry,
                window_geometry::settle_external_window_geometry.run_if(not_swiping),
            )
                .chain(),
            systems::cleanup_on_exit,
            reconcile::reconcile_windows
                .after(window_geometry::settle_external_window_geometry)
                // Audit the confirmed focus, not an older application between
                // direct focus resolution and its resulting observation.
                .after(triggers::window_focused_trigger)
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            reconcile::confirm_unavailable_windows,
            systems::refresh_window_notifications,
            state::periodic_state_save.run_if(on_timer(Duration::from_mins(5))),
            state::cleanup_on_exit,
            script_state::periodic_script_state_save.run_if(on_timer(Duration::from_mins(5))),
            script_state::script_state_cleanup_on_exit,
        ),
    );
    app.add_systems(
        PostUpdate,
        (
            systems::animate_entities.run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            systems::animate_resize_entities
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            window_frame::resume_suspended_window_frame_commits
                .before(window_frame::animate_presented_window_frames),
            window_frame::animate_presented_window_frames
                .after(systems::animate_entities)
                .after(systems::animate_resize_entities)
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            systems::commit_window_frame
                .after(window_frame::animate_presented_window_frames)
                .run_if(not(resource_exists::<Initializing>))
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            systems::verify_window_position
                .after(window_frame::animate_presented_window_frames)
                .before(systems::commit_window_frame)
                .run_if(not(resource_exists::<Initializing>))
                .run_if(not(resource_exists::<exit_restore::ExitInProgress>)),
            (
                systems::update_overlays
                    .after(systems::commit_window_frame)
                    .run_if(overlay_dirty.or_eager(on_timer(Duration::from_secs(1)))),
                systems::animate_decoration_overlay,
                systems::update_flash_messages,
            )
                .chain(),
            (
                crate::bar::update_bar.run_if(bar_dirty),
                crate::bar::apply_bar_requests,
                crate::bar::animate_bar,
            )
                .chain(),
        ),
    );
    app.add_systems(PostUpdate, state::capture_state_changes);
    app.add_systems(Last, exit_restore::restore_launch_windows);
}

/// Registers all the event triggers for the window manager.
pub fn register_triggers(app: &mut bevy::app::App) {
    app.add_observer(tiled_visibility::added)
        .add_observer(tiled_visibility::removed);
    app.add_systems(
        Update,
        (
            triggers::front_switched_trigger,
            triggers::window_focused_trigger.after(triggers::front_switched_trigger),
            triggers::mission_control_trigger,
            triggers::application_event_trigger,
            triggers::dispatch_application_messages,
            triggers::window_destroyed_trigger,
            triggers::invalidate_window_title,
            triggers::theme_change_trigger,
            triggers::window_resize_verifier,
        ),
    );
    app.add_observer(triggers::window_floating_trigger)
        .add_observer(triggers::window_floating_removed_trigger)
        .add_observer(triggers::window_visibility_trigger)
        .add_observer(triggers::window_visibility_removed_trigger)
        .add_observer(triggers::retile_window_trigger)
        .add_observer(triggers::spawn_window_trigger)
        .add_observer(triggers::send_message_trigger)
        .add_observer(triggers::window_removal_trigger)
        .add_observer(triggers::cleanup_timeout_trigger);
}

/// Projection of the tracked window most recently confirmed focused by macOS.
/// Only the focus coordinator's projection system may add or remove it.
#[derive(Component)]
pub struct FocusedMarker;

#[derive(Component)]
pub struct ActiveWorkspaceMarker;

#[derive(Component)]
pub struct FlashMessage(pub String);

/// Marker component for the currently active display.
#[derive(Component)]
pub struct ActiveDisplayMarker;

/// Marker component signifying a freshly created process, application, or window.
#[derive(Component)]
pub struct FreshMarker;

/// A concrete AX window moved to a different Application entity. Systems that
/// derive rules and layout from the owning bundle treat this like a fresh
/// window without changing its stable ECS identity.
#[derive(Component)]
pub(crate) struct WindowOwnershipChanged;

/// Keeps default window rules pending until their AX geometry readback and the
/// corresponding layout insertion both succeed. Transient AX failures leave
/// this marker in place so the next update retries the complete operation.
#[derive(Component)]
pub(crate) struct WindowDefaultsPending;

/// The window-rule/default phase completed successfully and the window may
/// now be inserted into its layout strip. Kept separate from
/// [`WindowDefaultsPending`] so a later successful probe cannot bypass a
/// transient AX failure from the defaults phase in the same update.
#[derive(Component)]
pub(crate) struct WindowDefaultsApplied;

/// A startup window is still owned by a native fullscreen Space, so its
/// physical fullscreen frame must not be captured as tiled layout state.
#[derive(Component)]
pub(crate) struct FullscreenDefaultsDeferred;

/// Marker component used to gather existing processes and windows during initialization.
#[derive(Component)]
pub struct ExistingMarker;

/// Marks a window discovered during startup so deferred default application
/// cannot mistake it for a newly opened window after initialization ends.
#[derive(Component)]
pub struct InitialWindowMarker;

/// Component representing a request to reposition a window.
#[derive(Component, Debug, Deref, DerefMut)]
pub struct RepositionMarker(pub Origin);

/// Component representing a request to resize a window.
#[derive(Component, Debug, Deref, DerefMut)]
pub struct ResizeMarker(pub Size);

/// Marker component indicating that windows around the marked entity need to be reshuffled.
#[derive(Component)]
pub struct ReshuffleAroundMarker;

/// Marker component requesting that the strip scroll *minimally* to keep the
/// entity's NEW layout position inside the viewport. Unlike
/// [`ReshuffleAroundMarker`], this does not anchor the entity to its old visual
/// position — if the new layout slot is already on-screen, the strip is left
/// alone and the entity is free to slide there. Only when the new slot would
/// fall off the edge does the strip scroll just enough to expose it.
#[derive(Component)]
pub struct EnsureVisibleMarker;

#[derive(Component, Debug)]
pub struct Scrolling {
    pub velocity: f64,
    pub position: f64,
    /// When true, the user's fingers are on the trackpad.
    pub is_user_swiping: bool,
    /// Last time a physical swipe event was received.
    pub last_event: Duration,
}

#[derive(Component, Clone, Debug, Default, Deref, DerefMut)]
pub struct LayoutPosition(pub Origin);

/// Compatibility origin used by the current `LayoutStrip` math.
/// [`DesiredWindowFrame`] is the canonical rendered target.
#[derive(Component, Clone, Debug, Deref, DerefMut)]
pub struct Position(pub Origin);

/// Logical size input used by the current `LayoutStrip` math.
/// It is neither animation progress nor confirmed macOS geometry.
#[derive(Component, Clone, Debug, Deref, DerefMut)]
pub struct Bounds(pub Size);

/// The most recent window frame successfully read back from macOS.
///
/// During animation or when an application constrains a write this may differ
/// from both the desired and presented projections. Consumers that must follow
/// the real surface, such as the focus border, use this projection instead of
/// optimistic layout state or `WindowOS`'s write-through cache.
#[derive(Component, Clone, Copy, Debug, Deref, DerefMut, PartialEq, Eq)]
pub struct ObservedWindowFrame(pub bevy::math::IRect);

/// Marks a window entity that is currently on a native macOS fullscreen space.
/// The window has been removed from its tiled position in the strip.
/// `order` gives the sequence in which windows went fullscreen (0, 1, 2, …)
/// so they can be navigated left-to-right in that order after the tiled strip.
#[derive(Clone, Component, Debug)]
pub struct NativeFullscreenMarker {
    pub layout_strip: Entity,
    pub workspace_id: WorkspaceId,
    pub index: usize,
}

#[derive(Component)]
pub struct FullWidthMarker {
    pub width_ratio: f64,
    /// Original floating frame restored by a second maximize action. Tiled
    /// windows keep using `width_ratio` and leave this empty.
    pub floating_frame: Option<bevy::math::IRect>,
}

/// Marks a tracked window that does not participate in the tiling layout.
#[derive(Component, Debug)]
pub struct Floating;

/// Visibility state for a tracked window that is not currently visible.
#[derive(Component, Debug)]
pub enum WindowVisibility {
    /// The window is minimized.
    Minimized,
    /// The window is hidden.
    Hidden,
}

#[derive(BevyEvent)]
pub struct RetileWindow(pub Entity);

/// A tile request waiting for trustworthy capability or native state reads.
#[derive(Component)]
pub(crate) struct RetilePending(pub Duration);

#[derive(Clone, Component, Copy, Debug)]
pub struct PreviousTiledStrip {
    pub workspace_id: WorkspaceId,
    pub index: usize,
}

/// Wrapper component for a `ProcessApi` trait object, enabling dynamic dispatch for process-related operations within Bevy.
#[derive(Component, Deref, DerefMut)]
pub struct BProcess(pub Box<dyn ProcessApi>);

/// Component to manage a timeout, often used for delaying actions or retries.
#[derive(Component)]
pub struct Timeout {
    /// The Bevy timer instance.
    pub timer: Timer,
    /// An optional system to execute on timeout.
    pub system_id: Option<SystemId>,
}

impl Timeout {
    /// Creates a new `Timeout` with a specified duration and an optional message.
    /// The timer is set to run once.
    ///
    /// # Arguments
    ///
    /// * `duration` - The `Duration` for the timeout.
    /// * `message` - An `Option<String>` containing a message to associate with the timeout.
    ///
    /// # Returns
    ///
    /// A new `Timeout` instance.
    pub fn new(duration: Duration, message: Option<String>, commands: &mut Commands) -> Self {
        let timer = Timer::new(duration, bevy::time::TimerMode::Once);
        if let Some(message) = message {
            let callback = move || {
                tracing::debug!("{message}");
            };
            let system_id = Some(commands.register_system(callback));

            Self { timer, system_id }
        } else {
            Self {
                timer,
                system_id: None,
            }
        }
    }
}

/// Component used as a retry mechanism for stray focus events that arrive before the target window is fully created.
#[derive(Component)]
pub struct StrayFocusEvent(pub FocusObservation);

/// Component used as a retry mechanism when `focused_window_id()` fails during
/// an `ApplicationFrontSwitched` event (e.g. transient `kAXErrorCannotComplete`).
#[derive(Component)]
pub struct RetryFrontSwitch {
    pub app_entity: Entity,
    pub generation: u64,
    pub timer: Timer,
    pub probe: Timer,
}

impl RetryFrontSwitch {
    pub fn new(app_entity: Entity, generation: u64, duration: Duration) -> Self {
        Self {
            app_entity,
            generation,
            timer: Timer::from_seconds(duration.as_secs_f32(), bevy::time::TimerMode::Once),
            probe: Timer::new(Duration::from_millis(100), bevy::time::TimerMode::Repeating),
        }
    }
}

#[derive(Clone, Component, Copy, Debug, Eq, PartialEq)]
pub enum DockPosition {
    Bottom(i32),
    Left(i32),
    Right(i32),
    Hidden,
}

#[derive(Component)]
pub struct RefreshWindowSizes(pub Instant);

impl Default for RefreshWindowSizes {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl RefreshWindowSizes {
    pub fn ready(&self) -> bool {
        const REFRESH_WINDOW_SIZE_DELAY_SEC: u64 = 5;
        self.0.elapsed() > Duration::from_secs(REFRESH_WINDOW_SIZE_DELAY_SEC)
    }
}

#[derive(Component)]
pub struct VerifyWindowPosition {
    remaining: u8,
}

impl Default for VerifyWindowPosition {
    fn default() -> Self {
        Self { remaining: 3 }
    }
}

impl VerifyWindowPosition {
    pub fn tick(&mut self) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.remaining == 0
    }
}

#[derive(Deref, DerefMut, Resource)]
pub struct LowPowerMode(pub bool);

#[derive(Resource)]
pub struct SystemTheme {
    pub is_dark: bool,
}

/// Resource to control whether window reshuffling should be skipped.
#[derive(Resource)]
pub struct SkipReshuffle(pub bool);

/// Component marking a deferred reshuffle while the mouse button is held down.
/// Spawned with a `Timeout` so it auto-despawns if the mouse-up event is lost.
#[derive(Component)]
pub struct MouseHeldMarker(pub Entity);

/// Resource indicating whether Mission Control is currently active.
#[derive(Resource)]
pub struct MissionControlActive(pub bool);

/// Resource holding the `WinID` of a window that should gain focus when focus-follows-mouse is enabled.
#[derive(Resource)]
pub struct FocusFollowsMouse(pub Option<WinID>);

#[derive(Resource)]
pub struct Initializing;

/// Bevy event trigger for spawning new windows.
#[derive(BevyEvent)]
pub struct SpawnWindowTrigger {
    windows: Vec<Window>,
    application: Option<Entity>,
}

impl SpawnWindowTrigger {
    pub fn new(windows: Vec<Window>) -> Self {
        Self {
            windows,
            application: None,
        }
    }

    pub(crate) fn for_application(application: Entity, windows: Vec<Window>) -> Self {
        Self {
            windows,
            application: Some(application),
        }
    }
}

#[derive(BevyEvent)]
pub struct ReadDisplayProperties(pub Entity);

#[derive(BevyEvent)]
pub struct SendMessageTrigger(pub Event);

#[derive(BevyEvent)]
pub struct RaiseWindow {
    pub entity: Entity,
    pub with_strip: bool,
}

pub trait SpawnCommandsExt {
    fn reposition_entity(&mut self, entity: Entity, origin: Origin);

    fn resize_entity(&mut self, entity: Entity, size: Size);

    fn reshuffle_around(&mut self, entity: Entity);

    fn ensure_visible(&mut self, entity: Entity);

    fn focus_entity(&mut self, entity: Entity, raise: bool);

    /// Restores focus without superseding a pending native follow.
    fn restore_focus_entity(&mut self, entity: Entity, raise: bool);

    #[cfg(feature = "lua")]
    fn flash_message(&mut self, message: String, duration: Duration);

    // Spawns a layout strip in a single place, to properly insert all components.
    fn spawn_layout_strip(
        &mut self,
        layout_strip: LayoutStrip,
        origin: Origin,
        display_entity: Entity,
        active: bool,
    ) -> EntityCommands<'_>;
}

impl SpawnCommandsExt for Commands<'_, '_> {
    #[instrument(level = Level::TRACE, skip(self))]
    fn reposition_entity(&mut self, entity: Entity, origin: Origin) {
        if let Ok(mut entity_commands) = self.get_entity(entity) {
            entity_commands.try_insert(RepositionMarker(origin));
        }
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn resize_entity(&mut self, entity: Entity, size: Size) {
        if size.x <= 0 || size.y <= 0 {
            return;
        }
        if let Ok(mut entity_commands) = self.get_entity(entity) {
            entity_commands.try_insert(ResizeMarker(size));
        }
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn reshuffle_around(&mut self, entity: Entity) {
        if let Ok(mut entity_commands) = self.get_entity(entity) {
            entity_commands.try_insert(ReshuffleAroundMarker);
        }
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn ensure_visible(&mut self, entity: Entity) {
        if let Ok(mut entity_commands) = self.get_entity(entity) {
            entity_commands.try_insert(EnsureVisibleMarker);
        }
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn focus_entity(&mut self, entity: Entity, raise: bool) {
        if self.get_entity(entity).is_ok() {
            self.trigger(focus::FocusWindow {
                entity,
                raise,
                kind: focus::FocusRequestKind::Explicit,
            });
        }
    }

    fn restore_focus_entity(&mut self, entity: Entity, raise: bool) {
        if self.get_entity(entity).is_ok() {
            self.trigger(focus::FocusWindow {
                entity,
                raise,
                kind: focus::FocusRequestKind::Automatic,
            });
        }
    }

    #[cfg(feature = "lua")]
    #[instrument(level = Level::TRACE, skip(self))]
    fn flash_message(&mut self, message: String, duration: Duration) {
        let timeout = Timeout::new(duration, None, self);
        self.spawn((timeout, FlashMessage(message)));
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn spawn_layout_strip(
        &mut self,
        layout_strip: LayoutStrip,
        origin: Origin,
        display_entity: Entity,
        active: bool,
    ) -> EntityCommands<'_> {
        let mut spawned = self.spawn((layout_strip, Position(origin), ChildOf(display_entity)));
        if active {
            spawned.insert(ActiveWorkspaceMarker);
        }
        spawned
    }
}

/// Rebuilds the Lua watcher after an atomic save or symlink replacement.
#[cfg(feature = "lua")]
pub(crate) fn rewatch_configs(
    window_manager: &WindowManager,
    path: &std::path::Path,
) -> Option<Box<dyn notify::Watcher>> {
    window_manager
        .setup_config_watcher(path)
        .inspect_err(|err| error!("watching the config '{}': {err}", path.display()))
        .ok()
}

pub fn setup_bevy_app(sender: EventSender, receiver: Receiver<Event>) -> Result<BevyApp> {
    let window_manager: Box<dyn WindowManagerApi> = Box::new(WindowManagerOS::new(sender.clone())?);

    #[cfg(feature = "lua")]
    let lua_path = crate::config::ensure_lua_file()?;
    #[cfg(feature = "lua")]
    let watcher = window_manager.setup_config_watcher(&lua_path)?;

    let mut app = BevyApp::new();

    app.add_plugins(MinimalPlugins)
        // `add_message`, not `init_resource`: the latter never registers the
        // buffer with bevy's `MessageRegistry`, so it's never double-buffered
        // and grows unbounded instead — every event lived for the process's
        // lifetime. Messages now live two frames, which every reader here
        // tolerates: readers gated on `not_swiping` or IPC subscribers would
        // rather drop a missed frame than act on a backlog.
        .add_message::<Event>()
        .insert_resource(Time::<Virtual>::from_max_delta(Duration::from_secs(10)))
        .insert_resource(WindowManager(window_manager))
        .insert_resource(SkipReshuffle(false))
        .insert_resource(SystemTheme {
            is_dark: crate::util::is_dark_mode(),
        })
        .insert_resource(MissionControlActive(false))
        .insert_resource(FocusFollowsMouse(None))
        .insert_resource(Initializing)
        .add_plugins(mouse::MouseEventsPlugin)
        .add_plugins(scroll::ScrollEventsPlugin)
        .add_plugins(workspace::WorkspaceEventsPlugin)
        .add_plugins(layout::LayoutEventsPlugin)
        .add_plugins(focus::FocusEventsPlugin)
        .add_plugins(display::DisplayEventsPlugin)
        .add_plugins((register_triggers, register_systems, register_commands));

    #[cfg(feature = "lua")]
    app.insert_non_send(watcher);

    // Run every schedule inline rather than fanning systems out across the task
    // pool: the task-pool handoff measured ~45% of main-thread time against
    // ~16% actually spent on accessibility calls, dropping to ~10% once
    // inlined. The expensive systems here all take `&mut Window` and are
    // already mutually exclusive, so the fan-out bought little. Startup must
    // also remain inline: inventory touches AppKit, CoreGraphics, and AX.
    configure_main_thread_schedules(&mut app);

    let bar_events = sender.clone();
    let mut platform_callbacks = PlatformCallbacks::new(sender);
    platform_callbacks.setup_handlers()?;
    let mtm = platform_callbacks.main_thread_marker;
    let overlay_manager = OverlayManager::new(mtm);
    let flash_message_manager = FlashMessageManager::new(mtm);
    let bar_manager = BarManager::new(mtm, bar_events);
    app.insert_non_send(platform_callbacks)
        .insert_non_send(WindowDiscovery::new(mtm))
        .insert_non_send(overlay_manager)
        .insert_non_send(flash_message_manager)
        .insert_non_send(bar_manager)
        .insert_non_send(receiver);

    let state_file_path = StateFilePath::default();
    if let Some(previous_state) = SpoolState::load_from_file(state_file_path.as_path()) {
        app.insert_resource(state::StatePersistence::starting_after(
            previous_state.revision,
        ));
        app.insert_resource(restore::RestoreCandidates::from(previous_state));
    }
    app.insert_resource(state_file_path);

    // Overwrites the empty store `register_commands` put there, which is what
    // the mock harness keeps: only the real app reads the user's file.
    app.insert_resource(script_state::ScriptStateStore::load());

    // Do not insert this in mocks.
    app.insert_resource(LowPowerMode(false));

    // Start the Lua worker and install its hot-reload plugin (kept out of the
    // mock harness). A missing/broken script falls back to an empty runtime so
    // the watcher can still pick up a later fix. `spawn` blocks until the
    // script finishes loading, so its keybinds are published before the event
    // tap can see a keypress.
    #[cfg(feature = "lua")]
    {
        let path = lua_path;
        // `spool.bind` resolves chords on the worker, and the layout-aware
        // keymap behind that goes through Carbon/TIS — must capture it here,
        // on the main thread, before the worker can ask for it.
        crate::config::prime_virtual_keymap();
        // The worker caches the script state store and watches this stamp to
        // know when its copy is stale — including when the writer was a client
        // rather than the script itself.
        let revision = app
            .world()
            .resource::<script_state::ScriptStateStore>()
            .revision_handle();
        let worker = lua::LuaWorker::spawn(lua::LuaSource::Path(path.clone()), revision);
        // Publish Lua settings before Startup; the input tap retains the same
        // shared config handle in gather_initial_processes.
        if let Some(config) = worker.built_config() {
            app.insert_resource(config);
        }
        app.insert_resource(worker);
        app.insert_resource(lua::LuaScriptPath(path));
        app.add_plugins(lua::LuaPlugin {});
    }

    Ok(app)
}

fn configure_main_thread_schedules(app: &mut BevyApp) {
    for label in [
        PreStartup.intern(),
        Startup.intern(),
        PostStartup.intern(),
        First.intern(),
        PreUpdate.intern(),
        Update.intern(),
        PostUpdate.intern(),
        Last.intern(),
    ] {
        app.edit_schedule(label, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
    }
}

struct WindowProperties {
    params: Vec<WindowParams>,
    pending: bool,
    default_floating: bool,
}

impl WindowProperties {
    pub fn new(app: &Application, window: &Window, config: &Config) -> Self {
        let bundle_id = app.bundle_id().unwrap_or_default();
        let title = window.title().ok();
        let role = window.role().ok();
        let subrole = window.subrole().ok();
        let matched = config.match_window_rules(
            title.as_deref(),
            Some(&bundle_id),
            role.as_deref(),
            subrole.as_deref(),
        );
        Self {
            params: matched.params,
            pending: matched.pending,
            default_floating: window.default_floating(),
        }
    }

    pub fn layout_decision(&self, window: &Window) -> crate::window_policy::LayoutDecision {
        if self.pending {
            crate::window_policy::LayoutDecision::Defer
        } else {
            window.layout_decision(self.floating())
        }
    }

    pub fn floating(&self) -> bool {
        self.params
            .iter()
            .find_map(|props| props.floating)
            .unwrap_or(self.default_floating)
    }

    pub fn insertion(&self) -> Option<usize> {
        self.params.iter().find_map(|props| props.index)
    }

    pub fn dont_focus(&self) -> bool {
        self.params
            .iter()
            .find_map(|props| props.dont_focus)
            .unwrap_or(false)
    }

    pub fn border_radius(&self) -> Option<f64> {
        self.params.iter().find_map(|p| p.border_radius)
    }

    pub fn grid_ratios(&self) -> Option<(f64, f64, f64, f64)> {
        self.params.iter().find_map(WindowParams::grid_ratios)
    }

    pub fn passthrough_keys(&self) -> Vec<(u8, Modifiers)> {
        self.params
            .iter()
            .flat_map(|p| p.passthrough_keys().to_vec())
            .collect::<Vec<_>>()
    }

    pub fn width_ratio(&self) -> Option<f64> {
        self.params.iter().find_map(|props| props.width)
    }

    pub fn vertical_padding(&self) -> i32 {
        self.params
            .iter()
            .find_map(|props| props.vertical_padding)
            .unwrap_or(0)
    }

    pub fn horizontal_padding(&self) -> i32 {
        self.params
            .iter()
            .find_map(|props| props.horizontal_padding)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod main_thread_tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn overlay_refreshes_moving_navigation_target_while_focus_is_unresolved() {
        use bevy::ecs::change_detection::DetectChangesMut as _;
        use bevy::ecs::system::{Local, ResMut};
        use bevy::math::IRect;

        #[derive(Resource, Default)]
        struct ProjectedFrame {
            frame: Option<IRect>,
            updates: usize,
        }

        let mut app = BevyApp::new();
        app.init_resource::<ProjectedFrame>();
        // Finder's observed readbacks from the VS Code -> Finder trace.
        let frames = [1070, 1034, 1020].map(|x| IRect::new(x, 34, x + 896, 990));
        let window = app
            .world_mut()
            .spawn((ObservedWindowFrame(frames[0]), FocusedMarker))
            .id();
        app.add_systems(
            PostUpdate,
            (
                move |mut windows: Query<&mut ObservedWindowFrame>, mut tick: Local<usize>| {
                    windows
                        .get_mut(window)
                        .unwrap()
                        .set_if_neq(ObservedWindowFrame(frames[(*tick).min(2)]));
                    *tick += 1;
                },
                (move |windows: Query<&ObservedWindowFrame>,
                       mut projected: ResMut<ProjectedFrame>| {
                    projected.frame = Some(windows.get(window).unwrap().0);
                    projected.updates += 1;
                })
                .run_if(overlay_dirty),
            )
                .chain(),
        );
        app.update();
        app.world_mut().entity_mut(window).remove::<FocusedMarker>();
        // Drain the removal invalidation. Subsequent readbacks must still
        // reach the overlay without a new focus event or the one-second audit.
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<ProjectedFrame>().frame,
            Some(frames[2]),
            "overlay kept an intermediate Finder frame after the final readback"
        );
        let updates = app.world().resource::<ProjectedFrame>().updates;
        app.update();
        assert_eq!(app.world().resource::<ProjectedFrame>().updates, updates);
    }

    #[test]
    fn timeouts_preserve_duration_boundaries_without_float_round_trips() {
        let mut world = bevy::ecs::world::World::new();
        for duration in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
            for message in [None, Some("timeout".to_owned())] {
                let has_callback = message.is_some();
                let timeout = Timeout::new(duration, message, &mut world.commands());
                assert_eq!(timeout.timer.duration(), duration);
                assert_eq!(timeout.system_id.is_some(), has_callback);
                world.flush();
            }
        }
    }

    #[test]
    fn startup_schedules_execute_on_the_calling_thread() {
        let caller = std::thread::current().id();
        let mut app = BevyApp::new();
        app.add_plugins(MinimalPlugins);
        configure_main_thread_schedules(&mut app);

        for label in [PreStartup.intern(), Startup.intern(), PostStartup.intern()] {
            let threads = Arc::new(Mutex::new(Vec::new()));
            for _ in 0..32 {
                let threads = threads.clone();
                app.add_systems(label, move || {
                    threads.lock().unwrap().push(std::thread::current().id());
                    std::thread::sleep(Duration::from_millis(1));
                });
            }
            app.world_mut().run_schedule(label);
            let threads = threads.lock().unwrap();
            assert_eq!(threads.len(), 32);
            assert!(
                threads.iter().all(|thread| *thread == caller),
                "{label:?} dispatched platform-capable startup work off its caller: {threads:?}"
            );
        }
    }
}

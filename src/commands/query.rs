use bevy::app::{App, PostUpdate, PreUpdate};
use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::Added;
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Query, Res, ResMut};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::warn;

use super::{Command, Operation};

use crate::ecs::state::{
    QueryStateParams, SpoolActiveState, SpoolQueryState, SpoolSpaceState, SpoolWindowState,
    StateEvent,
};
use crate::ecs::{ActiveWorkspaceMarker, FocusedMarker};
use crate::events::Event;
use crate::platform::WinID;
use spool_shared_types::wire::Response;

/// One connected `spool subscribe` client.
///
/// The channel is only ever touched from a task on the IO pool, since writing
/// to a peer that may not be reading can block. `alive` lets the main thread
/// learn a subscriber is gone via a plain atomic flag instead of a lock shared
/// with that task.
struct Subscriber {
    channel: Arc<spool_mach_ipc::Subscriber>,
    alive: Arc<AtomicBool>,
}

#[derive(Default, Resource)]
struct StateSubscribers {
    streams: Vec<Subscriber>,
}

#[derive(Default, Resource)]
struct StateBroadcastCache {
    workspace: Option<WorkspaceBroadcastSnapshot>,
    focus: Option<FocusBroadcastSnapshot>,
    spaces: Option<Vec<SpoolSpaceState>>,
    on_screen: Option<Vec<SpoolWindowState>>,
    titles: BTreeMap<WinID, String>,
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspaceBroadcastSnapshot {
    display_id: Option<u32>,
    space_id: Option<u64>,
}

impl From<&SpoolActiveState> for WorkspaceBroadcastSnapshot {
    fn from(active: &SpoolActiveState) -> Self {
        Self {
            display_id: active.display_id,
            space_id: active.space_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct FocusBroadcastSnapshot {
    window_id: Option<WinID>,
    bundle_id: Option<String>,
    title: Option<String>,
    space_id: Option<u64>,
}

impl From<&SpoolActiveState> for FocusBroadcastSnapshot {
    fn from(active: &SpoolActiveState) -> Self {
        Self {
            window_id: active.focused_window_id,
            bundle_id: active.focused_bundle_id.clone(),
            title: active.focused_window_title.clone(),
            space_id: active.space_id,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct StateBroadcastSignals {
    space_changed: bool,
    windows_changed: bool,
    window_focused: bool,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq)]
struct StateBroadcastIntent {
    space_changed: bool,
    windows_changed: bool,
    window_focused: bool,
    on_screen_changed: bool,
    title_changes: BTreeSet<WinID>,
    display_changes: Vec<Option<u32>>,
    active_display_changed: bool,
}

impl StateBroadcastIntent {
    fn from_events<'a>(
        events: impl IntoIterator<Item = &'a Event>,
        signals: StateBroadcastSignals,
    ) -> Self {
        let mut intent = Self {
            space_changed: signals.space_changed,
            windows_changed: signals.windows_changed,
            window_focused: signals.window_focused,
            ..Self::default()
        };

        for event in events {
            match event {
                Event::SpaceChanged => intent.space_changed = true,
                Event::WindowCreated { .. }
                | Event::WindowSpawned { .. }
                | Event::WindowDestroyed { .. }
                | Event::WindowMinimized { .. }
                | Event::WindowDeminimized { .. }
                | Event::Command {
                    command: Command::Window(Operation::Swap(_)) | Command::MoveWindowToSpace { .. },
                } => intent.windows_changed = true,
                Event::WindowFocused(_) => intent.window_focused = true,
                // Geometry alone decides what's on screen, so plain moves and
                // resizes can change the visible set on their own.
                Event::WindowMoved { .. } | Event::WindowResized { .. } => {
                    intent.on_screen_changed = true;
                }
                Event::WindowTitleChanged { window_id } => {
                    intent.title_changes.insert(*window_id);
                }
                Event::DisplayAdded { display_id }
                | Event::DisplayRemoved { display_id }
                | Event::DisplayMoved { display_id }
                | Event::DisplayResized { display_id }
                | Event::DisplayConfigured { display_id } => {
                    let display_id = Some(*display_id);
                    if !intent.display_changes.contains(&display_id) {
                        intent.display_changes.push(display_id);
                    }
                }
                Event::DisplayChanged => {
                    intent.active_display_changed = true;
                }
                _ => {}
            }
        }

        // Anything that rearranges windows, rows or displays also rearranges
        // what is on screen; titles ride along in the on-screen payload.
        intent.on_screen_changed |= intent.windows_changed
            || intent.space_changed
            || intent.active_display_changed
            || !intent.title_changes.is_empty();

        intent
    }

    fn requires_state(&self) -> bool {
        self.space_changed
            || self.windows_changed
            || self.window_focused
            || self.on_screen_changed
            || self.active_display_changed
    }

    fn is_empty(&self) -> bool {
        !self.requires_state() && self.title_changes.is_empty() && self.display_changes.is_empty()
    }
}

pub(super) fn register_query_commands(app: &mut App) {
    let active_subscribers = |subscribers: Option<Res<StateSubscribers>>| {
        subscribers.is_some_and(|subscribers| !subscribers.streams.is_empty())
    };

    app.init_resource::<StateSubscribers>();
    app.init_resource::<StateBroadcastCache>();
    app.add_systems(PreUpdate, (state_subscribe_handler, state_query_handler));
    app.add_systems(
        PostUpdate,
        state_event_broadcast_handler.run_if(active_subscribers),
    );
}

/// Answers socket queries that read the world: state documents and the window
/// set. Both live in one system so only one system holds [`QueryStateParams`]'s
/// world access. The window set is a separate variant rather than folded into
/// [`StateQueryKind`] since it projects a different value (the layout tree).
fn state_query_handler(mut messages: MessageReader<Event>, state: QueryStateParams) {
    /// Sends an answer without ever waiting for it to be taken. The reply
    /// channel holds one message and exactly one is sent, so this cannot fill;
    /// a client that hung up in the meantime is simply gone.
    fn reply(respond_to: &crate::events::Reply, answer: Result<Response, String>) {
        _ = respond_to.try_send(answer.unwrap_or_else(Response::Error));
    }

    for event in messages.read() {
        match event {
            Event::StateQuery { kind, respond_to } => reply(
                respond_to,
                state
                    .extract()
                    .map_err(|err| err.to_string())
                    .map(|state| Response::Query(state.to_query_payload(*kind))),
            ),
            Event::WindowSetQuery { respond_to } => reply(
                respond_to,
                state
                    .extract_window_set()
                    .map_err(|err| err.to_string())
                    .map(|set| Response::WindowSet(Box::new(set))),
            ),
            _ => {}
        }
    }
}

fn state_subscribe_handler(
    mut messages: MessageReader<Event>,
    mut subscribers: ResMut<StateSubscribers>,
) {
    for event in messages.read() {
        let Event::StateSubscribe { subscriber } = event else {
            continue;
        };
        subscribers.streams.push(Subscriber {
            channel: subscriber.clone(),
            alive: Arc::new(AtomicBool::new(true)),
        });
    }
}

#[cfg(test)]
fn collect_state_broadcast_events<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    state: &SpoolQueryState,
    cache: &mut StateBroadcastCache,
    title_for_window: impl Fn(WinID) -> Option<String>,
    signals: StateBroadcastSignals,
) -> Vec<StateEvent> {
    let intent = StateBroadcastIntent::from_events(events, signals);
    collect_state_broadcast_events_for_intent(&intent, Some(state), cache, title_for_window)
}

fn collect_state_broadcast_events_for_intent(
    intent: &StateBroadcastIntent,
    state: Option<&SpoolQueryState>,
    cache: &mut StateBroadcastCache,
    title_for_window: impl Fn(WinID) -> Option<String>,
) -> Vec<StateEvent> {
    let mut display_changes = intent.display_changes.clone();
    if intent.active_display_changed
        && let Some(state) = state
        && !display_changes.contains(&state.active.display_id)
    {
        display_changes.push(state.active.display_id);
    }

    let mut title_changes = BTreeMap::new();
    for window_id in &intent.title_changes {
        title_changes.insert(*window_id, title_for_window(*window_id).unwrap_or_default());
    }

    let Some(state) = state else {
        let mut outgoing = Vec::new();
        for (window_id, title) in title_changes {
            if cache.titles.get(&window_id) == Some(&title) {
                continue;
            }
            outgoing.push(StateEvent::WindowTitleChanged {
                window_id,
                title: title.clone(),
            });
            cache.titles.insert(window_id, title);
        }
        for display_id in display_changes {
            outgoing.push(StateEvent::DisplayChanged { display_id });
        }
        return outgoing;
    };

    let mut outgoing = Vec::new();

    if intent.space_changed {
        let workspace = WorkspaceBroadcastSnapshot::from(&state.active);
        if cache.workspace.as_ref() != Some(&workspace) && workspace.space_id.is_some() {
            outgoing.push(StateEvent::SpaceChanged {
                active: state.active.clone(),
            });
            cache.workspace = Some(workspace);
        }
    }

    if intent.windows_changed && cache.spaces.as_ref() != Some(&state.spaces) {
        outgoing.push(StateEvent::WindowsChanged {
            space_id: state.active.space_id,
            active: state.active.clone(),
        });
        cache.spaces = Some(state.spaces.clone());
    }

    if intent.on_screen_changed {
        let on_screen = state.on_screen().into_iter().cloned().collect::<Vec<_>>();
        if cache.on_screen.as_ref() != Some(&on_screen) {
            outgoing.push(StateEvent::OnScreenChanged {
                windows: on_screen.clone(),
                active: state.active.clone(),
            });
            cache.on_screen = Some(on_screen);
        }
    }

    if intent.window_focused {
        let focus = FocusBroadcastSnapshot::from(&state.active);
        if focus.window_id.is_some() && cache.focus.as_ref() != Some(&focus) {
            outgoing.push(StateEvent::WindowFocused {
                window_id: focus.window_id,
                bundle_id: focus.bundle_id.clone(),
                title: focus.title.clone(),
                space_id: focus.space_id,
            });
            cache.focus = Some(focus);
        }
    }

    for (window_id, title) in title_changes {
        if cache.titles.get(&window_id) == Some(&title) {
            continue;
        }
        outgoing.push(StateEvent::WindowTitleChanged {
            window_id,
            title: title.clone(),
        });
        cache.titles.insert(window_id, title);
    }

    for display_id in display_changes {
        outgoing.push(StateEvent::DisplayChanged { display_id });
    }

    outgoing
}

fn state_event_broadcast_handler(
    mut messages: MessageReader<Event>,
    mut subscribers: ResMut<StateSubscribers>,
    mut cache: ResMut<StateBroadcastCache>,
    focused_changes: Query<Entity, Added<FocusedMarker>>,
    active_workspace_changes: Query<Entity, Added<ActiveWorkspaceMarker>>,
    state: QueryStateParams,
) {
    let events = messages.read().collect::<Vec<_>>();

    if subscribers.streams.is_empty() {
        return;
    }

    let signals = StateBroadcastSignals {
        space_changed: !active_workspace_changes.is_empty(),
        windows_changed: events.iter().any(|event| {
            let Event::WindowMoved { window_id } = event else {
                return false;
            };
            state
                .windows()
                .find(*window_id)
                .and_then(|(_, entity)| state.windows().get_tracked(entity))
                .is_some_and(|(_, _, window_state)| window_state.is_floating())
        }),
        window_focused: !focused_changes.is_empty(),
    };
    let intent = StateBroadcastIntent::from_events(events, signals);
    if intent.is_empty() {
        return;
    }

    let document = if intent.requires_state() {
        match state.extract() {
            Ok(document) => Some(document),
            Err(err) => {
                warn!("extracting query state for broadcast: {err}");
                return;
            }
        }
    } else {
        None
    };
    let outgoing = collect_state_broadcast_events_for_intent(
        &intent,
        document.as_ref(),
        &mut cache,
        |window_id| {
            state
                .windows()
                .find(window_id)
                .and_then(|(window, _)| window.title().ok())
        },
    );

    if outgoing.is_empty() {
        return;
    }

    // Each send is non-blocking and bounded, so a stalled reader just drops the
    // event instead of blocking the main thread; an exited subscriber is
    // marked below and reaped here on the next broadcast.
    subscribers
        .streams
        .retain(|subscriber| subscriber.alive.load(Ordering::Relaxed));

    let events = Arc::new(outgoing);
    for subscriber in &subscribers.streams {
        for event in events.iter() {
            match subscriber.channel.try_send(event) {
                Ok(()) => {}
                // The subscriber's process is gone; reaped on the next
                // broadcast. This is a real signal from the kernel rather than
                // a write error a merely slow reader would also produce.
                Err(spool_mach_ipc::Error::PeerGone) => {
                    subscriber.alive.store(false, Ordering::Relaxed);
                    break;
                }
                // Alive but not keeping up. The event is lost; the subscriber
                // is kept.
                Err(err) => warn!("pushing broadcast event: {err}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::state::{
        Frame, SpaceKind, SpoolDisplayState, SpoolSpaceState, SpoolWindowState,
    };
    use crate::events::Event as SpoolEvent;

    fn query_state_with_active_window(
        window_id: WinID,
        bundle_id: &str,
        title: &str,
        space_id: u32,
        window_ids: Vec<WinID>,
    ) -> SpoolQueryState {
        let active = SpoolActiveState {
            display_id: Some(1),
            space_id: Some(u64::from(space_id)),
            focused_window_id: Some(window_id),
            focused_bundle_id: Some(bundle_id.to_string()),
            focused_app_name: Some("Test App".to_string()),
            focused_window_title: Some(title.to_string()),
        };
        let windows = window_ids
            .into_iter()
            .map(|window_id| SpoolWindowState {
                window_id,
                bundle_id: bundle_id.to_string(),
                app_name: "Test App".to_string(),
                title: title.to_string(),
                focused: active.focused_window_id == Some(window_id),
                floating: false,
                display_id: Some(1),
                frame: Some(Frame {
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 600,
                }),
                visible: true,
            })
            .collect();

        SpoolQueryState {
            version: 3,
            timestamp: 123,
            active,
            capabilities: spool_shared_types::state::SpaceCapabilities::default(),
            displays: vec![SpoolDisplayState {
                display_id: 1,
                active: true,
                visible_space_id: Some(u64::from(space_id)),
            }],
            spaces: vec![SpoolSpaceState {
                space_id: u64::from(space_id),
                display_id: 1,
                ordinal: 0,
                kind: SpaceKind::User,
                visible: true,
                focused: true,
                windows,
            }],
        }
    }

    #[test]
    fn test_state_broadcast_coalesces_focus_events_to_current_state() {
        let state = query_state_with_active_window(
            26_261,
            "com.cmuxterm.app",
            "aicommit2 ~/P/nixos-config",
            2,
            vec![26_261],
        );
        let mut cache = StateBroadcastCache::default();
        let events = [
            SpoolEvent::window_focused(18_639),
            SpoolEvent::window_focused(26_261),
            SpoolEvent::window_focused(26_261),
        ];

        let outgoing = collect_state_broadcast_events(
            events.iter(),
            &state,
            &mut cache,
            |_| None,
            StateBroadcastSignals::default(),
        );

        assert_eq!(outgoing.len(), 1);
        assert_eq!(
            outgoing[0],
            StateEvent::WindowFocused {
                window_id: Some(26_261),
                bundle_id: Some("com.cmuxterm.app".to_string()),
                title: Some("aicommit2 ~/P/nixos-config".to_string()),
                space_id: Some(2),
            }
        );

        let duplicate = collect_state_broadcast_events(
            events.iter(),
            &state,
            &mut cache,
            |_| None,
            StateBroadcastSignals::default(),
        );

        assert!(duplicate.is_empty());
    }

    #[test]
    fn test_state_broadcasts_floating_window_moves_and_skips_unchanged_state() {
        let state =
            query_state_with_active_window(26_261, "com.cmuxterm.app", "term", 2, vec![26_261]);
        let mut cache = StateBroadcastCache::default();
        let events = [SpoolEvent::WindowMoved { window_id: 26_261 }];
        let signals = StateBroadcastSignals {
            windows_changed: true,
            ..StateBroadcastSignals::default()
        };

        let outgoing =
            collect_state_broadcast_events(events.iter(), &state, &mut cache, |_| None, signals);

        // A move republishes both the window list and the on-screen set.
        assert_eq!(outgoing.len(), 2);
        let StateEvent::WindowsChanged { space_id, active } = &outgoing[0] else {
            panic!("expected a windows_changed event, got {:?}", outgoing[0]);
        };
        assert_eq!(*space_id, Some(2));
        assert_eq!(active.focused_window_id, Some(26_261));
        let StateEvent::OnScreenChanged { windows, .. } = &outgoing[1] else {
            panic!("expected an on_screen_changed event, got {:?}", outgoing[1]);
        };
        assert_eq!(
            windows
                .iter()
                .map(|window| window.window_id)
                .collect::<Vec<_>>(),
            vec![26_261]
        );

        let duplicate =
            collect_state_broadcast_events(events.iter(), &state, &mut cache, |_| None, signals);

        assert!(duplicate.is_empty());

        let changed_state = query_state_with_active_window(
            26_261,
            "com.cmuxterm.app",
            "term",
            2,
            vec![26_261, 26_262],
        );
        let changed = collect_state_broadcast_events(
            events.iter(),
            &changed_state,
            &mut cache,
            |_| None,
            signals,
        );

        assert_eq!(changed.len(), 2);
        assert!(matches!(changed[0], StateEvent::WindowsChanged { .. }));
        let StateEvent::OnScreenChanged { windows, .. } = &changed[1] else {
            panic!("expected an on_screen_changed event, got {:?}", changed[1]);
        };
        assert_eq!(
            windows
                .iter()
                .map(|window| window.window_id)
                .collect::<Vec<_>>(),
            vec![26_261, 26_262]
        );
    }

    #[test]
    fn test_window_moves_track_the_on_screen_set() {
        // A bare move (no window-list or workspace change) still has to be
        // looked at.
        let intent = StateBroadcastIntent::from_events(
            [SpoolEvent::WindowMoved { window_id: 10 }].iter(),
            StateBroadcastSignals::default(),
        );
        assert!(intent.on_screen_changed);
        assert!(intent.requires_state());
        assert!(
            !intent.windows_changed,
            "a move is not a window-list change"
        );

        // The set itself is cached, so a move that changes nothing visible is
        // not broadcast twice.
        let state = query_state_with_active_window(1, "com.example.app", "term", 1, vec![1]);
        let mut cache = StateBroadcastCache::default();
        let first = collect_state_broadcast_events(
            [SpoolEvent::WindowMoved { window_id: 1 }].iter(),
            &state,
            &mut cache,
            |_| None,
            StateBroadcastSignals::default(),
        );
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0], StateEvent::OnScreenChanged { .. }));

        let repeat = collect_state_broadcast_events(
            [SpoolEvent::WindowMoved { window_id: 1 }].iter(),
            &state,
            &mut cache,
            |_| None,
            StateBroadcastSignals::default(),
        );
        assert!(repeat.is_empty());
    }

    #[test]
    fn test_state_broadcast_emits_focus_when_focused_marker_changes_without_event_message() {
        let state = query_state_with_active_window(
            26_262,
            "com.openai.codex",
            "Codex",
            2,
            vec![26_261, 26_262],
        );
        let mut cache = StateBroadcastCache::default();

        let outgoing = collect_state_broadcast_events(
            std::iter::empty(),
            &state,
            &mut cache,
            |_| None,
            StateBroadcastSignals {
                window_focused: true,
                ..StateBroadcastSignals::default()
            },
        );

        assert_eq!(outgoing.len(), 1);
        assert_eq!(
            outgoing[0],
            StateEvent::WindowFocused {
                window_id: Some(26_262),
                bundle_id: Some("com.openai.codex".to_string()),
                title: Some("Codex".to_string()),
                space_id: Some(2),
            }
        );
    }

    #[test]
    fn test_state_broadcast_intent_skips_state_for_empty_or_unrelated_events() {
        let empty =
            StateBroadcastIntent::from_events(std::iter::empty(), StateBroadcastSignals::default());
        assert!(empty.is_empty());
        assert!(!empty.requires_state());

        let unrelated = StateBroadcastIntent::from_events(
            [
                SpoolEvent::ThemeChanged,
                SpoolEvent::MouseUp {
                    point: objc2_core_foundation::CGPoint::default(),
                    modifiers: crate::platform::Modifiers::empty(),
                },
            ]
            .iter(),
            StateBroadcastSignals::default(),
        );
        assert!(unrelated.is_empty());
        assert!(!unrelated.requires_state());
    }

    #[test]
    fn test_state_broadcast_intent_classifies_relevant_events() {
        let intent = StateBroadcastIntent::from_events(
            [
                SpoolEvent::SpaceChanged,
                SpoolEvent::WindowMinimized { window_id: 10 },
                SpoolEvent::window_focused(11),
                SpoolEvent::WindowTitleChanged { window_id: 12 },
                SpoolEvent::DisplayResized { display_id: 2 },
            ]
            .iter(),
            StateBroadcastSignals::default(),
        );

        assert!(intent.space_changed);
        assert!(intent.windows_changed);
        assert!(intent.window_focused);
        assert_eq!(intent.title_changes, [12].into());
        assert_eq!(intent.display_changes, vec![Some(2)]);
        assert!(intent.requires_state());
        assert!(!intent.is_empty());
    }
}

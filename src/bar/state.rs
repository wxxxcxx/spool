use std::collections::{HashMap, HashSet};

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::Has;
use bevy::ecs::system::{Query, Res, SystemParam};
use spool_shared_types::windowset::ColumnKind;
use tracing::warn;

use super::BarSnapshot;
use super::model::{BarColumn, BarSpace, BarWindow};
use crate::ecs::focus::FocusCoordinator;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::reconcile::WindowUnavailable;
use crate::ecs::state::QueryStateParams;
use crate::ecs::{Floating, FocusedMarker, WindowVisibility};
use crate::manager::{Application, Window, WindowManager};

// Presentation must retain tracked identity even while AX operations are suspended.
// Do not widen the shared `Windows` query used by focus and layout commands.
type BarWindows<'w, 's> = Query<
    'w,
    's,
    (
        &'static Window,
        Entity,
        &'static ChildOf,
        Has<Floating>,
        Has<FocusedMarker>,
        Option<&'static WindowUnavailable>,
        Has<WindowVisibility>,
    ),
>;

#[derive(SystemParam)]
pub(crate) struct BarStateParams<'w, 's> {
    state: QueryStateParams<'w, 's>,
    strips: Query<'w, 's, &'static LayoutStrip>,
    windows: BarWindows<'w, 's>,
    apps: Query<'w, 's, (Entity, &'static Application)>,
    window_manager: Res<'w, WindowManager>,
    /// Per-Space selection is presentation state, independent of native focus.
    focus: Option<Res<'w, FocusCoordinator>>,
}

impl BarStateParams<'_, '_> {
    pub fn extract(&self) -> crate::errors::Result<BarSnapshot> {
        // One native membership scan serves the state document, the window set
        // and the Bar's own per-Space membership. A scan that fails publishes
        // nothing, so each Space falls back to being read on its own rather
        // than the whole Bar degrading to the retained layout.
        let memberships = self.state.memberships();
        let floating_by_window = self.state.floating_windows();
        let mut snapshot = BarSnapshot::from_state(
            &self
                .state
                .query_state(&floating_by_window, memberships.as_ref())?,
            &self
                .state
                .window_set(&floating_by_window, memberships.as_ref()),
        );
        let strips = self
            .strips
            .iter()
            .map(|strip| (strip.id(), strip))
            .collect::<HashMap<_, _>>();
        let floating = self
            .windows
            .iter()
            .filter_map(|(_, entity, _, floating, _, _, _)| floating.then_some(entity))
            .collect::<Vec<_>>();
        for display in &mut snapshot.displays {
            for space in &mut display.spaces {
                let membership = memberships.as_ref().map_or_else(
                    || {
                        self.window_manager
                            .windows_in_workspace(space.id)
                            .map(|ids| ids.into_iter().collect::<HashSet<_>>())
                            .inspect_err(|error| {
                                // SkyLight reports an empty Space as NotFound.
                                if !matches!(error, crate::errors::Error::NotFound(_)) {
                                    warn!(
                                        space_id = space.id,
                                        %error,
                                        "unable to observe Bar membership"
                                    );
                                }
                            })
                            .ok()
                    },
                    |memberships| Some(memberships.listed_in(space.id).collect()),
                );
                if let Some(strip) = strips.get(&space.id) {
                    self.project_windows(space, strip, &floating, membership.as_ref());
                }
            }
        }
        Ok(snapshot)
    }

    fn project_windows(
        &self,
        space: &mut BarSpace,
        strip: &LayoutStrip,
        floating: &[Entity],
        membership: Option<&HashSet<i32>>,
    ) {
        let known = space
            .windows()
            .map(|window| (window.id, window.clone()))
            .collect::<HashMap<_, _>>();
        // Whether this Space is where the Bar draws a window, by the same rule
        // the floating lane has always used.
        let in_space = |entity: Entity, window_id: i32| {
            membership.map_or_else(
                || strip.contains(entity) || known.contains_key(&window_id),
                |ids| ids.contains(&window_id),
            )
        };
        let selected = self
            .focus
            .as_ref()
            .and_then(|focus| focus.space_selection(space.id));
        space.columns = strip
            .columns()
            .filter_map(|column| {
                let windows = column
                    .window_iter()
                    .filter(|entity| {
                        self.windows
                            .get(*entity)
                            .is_ok_and(|(_, _, _, floating, _, _, _)| !floating)
                    })
                    .filter_map(|entity| {
                        self.window_record(entity, space.visible, &known, selected)
                    })
                    .collect::<Vec<_>>();
                if windows.is_empty() {
                    return None;
                }
                let selected = column
                    .top()
                    .and_then(|entity| self.windows.get(entity).ok())
                    .and_then(|(window, ..)| {
                        windows.iter().position(|record| record.id == window.id())
                    })
                    .unwrap_or(0);
                Some(BarColumn {
                    kind: match column {
                        Column::Single(_) => ColumnKind::Single,
                        Column::Stack(_) => ColumnKind::Stack,
                        Column::Tabs(_) => ColumnKind::Tabs,
                        Column::Fullscren(_) => ColumnKind::Fullscreen,
                    },
                    selected,
                    windows,
                })
            })
            .collect();

        space.floating.clear();
        if floating.is_empty() {
            return;
        }
        // Floating windows need native membership, including on inactive Spaces.
        // Filter server IDs through ECS identity so ignored/untracked surfaces stay out.
        space.floating = floating
            .iter()
            .copied()
            .filter(|entity| {
                self.windows
                    .get(*entity)
                    .is_ok_and(|(window, ..)| in_space(*entity, window.id()))
            })
            .filter_map(|entity| self.window_record(entity, space.visible, &known, selected))
            .collect();
    }

    fn window_record(
        &self,
        entity: Entity,
        space_visible: bool,
        known: &HashMap<i32, BarWindow>,
        selected: Option<Entity>,
    ) -> Option<BarWindow> {
        let (window, _, parent, _, _, unavailable, hidden) = self.windows.get(entity).ok()?;
        if unavailable.is_some()
            && self
                .windows
                .iter()
                .any(|(other, _, _, _, _, unavailable, _)| {
                    other.id() == window.id() && unavailable.is_none()
                })
        {
            return None;
        }
        // Lifecycle reconciliation owns whether a retained identity still has a
        // presentation slot. Bar and tiling must not classify absence separately.
        if !hidden && unavailable.is_some_and(WindowUnavailable::excludes_from_layout_projection) {
            return None;
        }
        let (_, app) = self.apps.get(parent.parent()).ok()?;
        Some(BarWindow {
            id: window.id(),
            bundle_id: app.bundle_id().unwrap_or_default(),
            app_name: app.name().to_owned(),
            // The available-window projection supplies titles; never ask AX for a
            // withdrawn window merely to draw its already-known application icon.
            title: known
                .get(&window.id())
                .map(|window| window.title.clone())
                .unwrap_or_default(),
            focused: selected == Some(entity),
            visible: space_visible && !hidden && unavailable.is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::layout::{BarLayout, ItemKind};
    use crate::ecs::ActiveWorkspaceMarker;
    use crate::ecs::layout::LayoutStrip;
    use crate::ecs::native_space::VisibleNativeSpaceMarker;
    use crate::ecs::reconcile::WindowUnavailable;
    use crate::events::Event;
    use crate::tests::{TEST_WORKSPACE_ID, TestHarness, find_window_entity};
    use bevy::ecs::system::RunSystemOnce;

    fn snapshot(state: BarStateParams) -> BarSnapshot {
        state.extract().unwrap()
    }

    fn original_space(state: &BarSnapshot) -> &BarSpace {
        state.displays[0]
            .spaces
            .iter()
            .find(|space| space.id == TEST_WORKSPACE_ID)
            .unwrap()
    }

    fn draws_focused(state: &BarSnapshot, id: i32) -> bool {
        state
            .displays
            .iter()
            .flat_map(|display| display.spaces.iter())
            .flat_map(BarSpace::windows)
            .any(|window| window.id == id && window.focused)
    }

    #[test]
    fn background_space_preference_is_visible_without_activation() {
        for floating in [false, true] {
            let mut harness = TestHarness::new()
                .with_display(
                    crate::tests::TEST_DISPLAY_ID,
                    bevy::math::IRect::new(0, 0, 1024, 768),
                    vec![TEST_WORKSPACE_ID, 77],
                )
                .with_windows(1)
                .with_workspace_window(1, 77, |window| window.default_floating = floating);
            harness.pump_frames(15);
            harness.mock_state.take_focus_requests();
            harness
                .world()
                .run_system_once_with(crate::ecs::focus::set_space_preference, (77, 1))
                .unwrap()
                .unwrap();
            let state = harness.world().run_system_once(snapshot).unwrap();
            assert!(draws_focused(&state, 0));
            assert!(draws_focused(&state, 1));
            assert!(harness.mock_state.take_focus_requests().is_empty());
            assert!(harness.mock_state.native_space_intents().is_empty());
            harness.mock_state.os_withdraw_window(1);
            harness.world().write_message(Event::SpaceChanged);
            harness.pump_frames(3);
            assert!(draws_focused(
                &harness.world().run_system_once(snapshot).unwrap(),
                1
            ));
        }
    }

    #[test]
    fn local_selection_falls_back_after_close_or_minimize_and_clears_without_candidates() {
        for close in [false, true] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(15);
            harness.world().write_message(Event::action_requested(
                crate::commands::Action::FocusWindow { window_id: 1 },
            ));
            harness.pump_frames(30);
            assert!(draws_focused(
                &harness.world().run_system_once(snapshot).unwrap(),
                1
            ));
            for (id, expected) in [(1, Some(0)), (0, None)] {
                if close {
                    harness.mock_state.os_close_window(id);
                } else {
                    harness.mock_state.os_minimize_window(id, true);
                }
                harness.pump_frames(3);
                let state = harness.world().run_system_once(snapshot).unwrap();
                let selected = original_space(&state)
                    .windows()
                    .filter(|window| window.focused)
                    .map(|window| window.id)
                    .collect::<Vec<_>>();
                assert_eq!(selected, expected.into_iter().collect::<Vec<_>>());
            }
        }
    }

    /// The whole point of drawing a request: the window the user clicked is the
    /// one the indicator moves to, without waiting for the accessibility
    /// observation that confirms it (measured at 13–72 ms, and then a frame or
    /// two of projection behind it).
    #[test]
    fn a_requested_focus_moves_the_indicator_before_accessibility_confirms() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        let before = harness.world().run_system_once(snapshot).unwrap();
        let target = [0, 1]
            .into_iter()
            .find(|id| !draws_focused(&before, *id))
            .expect("one of the two windows is not drawn as focused");

        harness.world().write_message(Event::action_requested(
            crate::commands::Action::FocusWindow { window_id: target },
        ));
        harness
            .world()
            .run_system_once(crate::commands::dispatch_actions)
            .unwrap();
        harness
            .world()
            .resource_mut::<bevy::ecs::message::Messages<Event>>()
            .clear();

        // No observation has been delivered: only the request exists.
        let after = harness.world().run_system_once(snapshot).unwrap();
        assert!(
            draws_focused(&after, target),
            "the Bar must draw the requested window as focused"
        );
        assert_eq!(
            after
                .displays
                .iter()
                .flat_map(|display| display.spaces.iter())
                .flat_map(BarSpace::windows)
                .filter(|window| window.focused)
                .count(),
            1,
            "exactly one window draws as focused"
        );
    }

    /// One extraction builds three read models — the state document, the
    /// window set and the Bar's own per-Space membership — and they must share
    /// one native membership scan: per-Space reads dominate the Bar's frame.
    #[test]
    fn one_bar_extraction_reads_native_membership_once_per_space() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        let spaces = harness
            .world()
            .resource::<crate::ecs::topology::NativeTopology>()
            .known_displays()
            .flat_map(|(_, spaces)| spaces.iter().copied())
            .collect::<HashSet<_>>()
            .len();
        assert!(spaces > 0, "the harness must have native Spaces to scan");
        let before = harness.mock_state.workspace_membership_query_count();
        harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(
            harness.mock_state.workspace_membership_query_count() - before,
            spaces,
            "one scan reads each Space once; the three read models share it"
        );
    }

    fn hide_space(harness: &mut TestHarness) {
        harness.mock_state.activate_workspace(
            crate::tests::TEST_DISPLAY_ID,
            TEST_WORKSPACE_ID + 1,
            false,
        );
        harness
            .world()
            .run_system_once(crate::ecs::topology::gather_initial_topology)
            .unwrap();
        let entity = harness
            .world()
            .query::<(bevy::prelude::Entity, &LayoutStrip)>()
            .iter(harness.world())
            .find(|(_, strip)| strip.id() == TEST_WORKSPACE_ID)
            .unwrap()
            .0;
        harness
            .world()
            .entity_mut(entity)
            .remove::<(ActiveWorkspaceMarker, VisibleNativeSpaceMarker)>();
    }

    #[test]
    fn collapsed_space_keeps_icons_when_accessibility_withdraws_its_windows() {
        let mut harness = TestHarness::new().with_windows(3);
        harness.pump_frames(15);
        for id in 0..3 {
            harness.mock_state.os_withdraw_window(id);
        }
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        for id in 0..3 {
            let entity = find_window_entity(id, harness.world());
            assert!(harness.world().get::<WindowUnavailable>(entity).is_some());
        }
        hide_space(&mut harness);
        let state = harness.world().run_system_once(snapshot).unwrap();
        let space = original_space(&state);
        assert!(!space.visible);
        assert_eq!(
            space.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let layout = crate::bar::layout::tests::collapsed_layout(&state.displays[0], 1200.0);
        assert_eq!(
            layout
                .items
                .iter()
                .filter(|item| matches!(
                    item.kind,
                    ItemKind::Window {
                        collapsed: true,
                        ..
                    }
                ))
                .count(),
            3
        );
        assert!(
            !layout
                .items
                .iter()
                .any(|item| matches!(item.kind, ItemKind::Placeholder { .. }))
        );
    }

    #[test]
    fn collapsed_space_includes_floating_windows_outside_its_strip() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        let floating = find_window_entity(1, harness.world());
        harness
            .world()
            .entity_mut(floating)
            .insert(crate::ecs::Floating);
        let mut strips = harness.world().query::<&mut LayoutStrip>();
        strips.single_mut(harness.world()).unwrap().remove(floating);
        harness.mock_state.update_window(1, |window| {
            window.minimized = true;
            window.ordered_out = true;
        });
        hide_space(&mut harness);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(
            original_space(&state)
                .floating
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn unavailable_icons_do_not_enable_actions_and_disappear_after_confirmed_close() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        harness.mock_state.os_withdraw_window(1);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        hide_space(&mut harness);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(original_space(&state).windows().count(), 2);
        assert!(
            original_space(&state)
                .windows()
                .all(|window| !window.visible)
        );
        assert!(
            harness
                .world()
                .run_system_once(|windows: crate::ecs::params::Windows| windows.find(1).is_none())
                .unwrap()
        );
        harness.mock_state.os_settle_withdrawn_surface(1);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        hide_space(&mut harness);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(
            original_space(&state)
                .windows()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn reopened_identity_replaces_unavailable_bar_projection() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        let old = find_window_entity(1, harness.world());
        let parent = harness.world().get::<ChildOf>(old).unwrap().parent();
        harness.mock_state.os_withdraw_window(1);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        assert!(harness.world().get::<WindowUnavailable>(old).is_some());
        let replacement = harness.mock_state.spawn_window(
            crate::tests::TEST_PROCESS_ID,
            TEST_WORKSPACE_ID,
            1,
            bevy::math::IRect::new(100, 100, 400, 300),
        );
        harness
            .world()
            .spawn((replacement, ChildOf(parent), Floating));
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(
            state.displays[0].spaces[0]
                .windows()
                .filter(|window| window.id == 1)
                .count(),
            1
        );
    }

    #[test]
    fn closed_retained_surface_has_no_bar_icon_and_can_return() {
        for collapsed in [false, true] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(15);
            let entity = find_window_entity(1, harness.world());
            // Ghostty and OPPO retain a normal-layer opaque surface after Cmd-W,
            // but remove the window from both AX and ordered Space membership.
            harness.mock_state.update_window(1, |window| {
                window.visible = false;
                window.ordered_out = true;
            });
            harness.mock_state.os_withdraw_window(1);
            harness.world().write_message(Event::SpaceChanged);
            harness.pump_frames(2);
            if collapsed {
                hide_space(&mut harness);
            }
            assert!(harness.world().get::<WindowUnavailable>(entity).is_some());
            let state = harness.world().run_system_once(snapshot).unwrap();
            assert_eq!(
                original_space(&state)
                    .windows()
                    .map(|window| window.id)
                    .collect::<Vec<_>>(),
                vec![0],
                "closed retained surface remained in the Bar (collapsed={collapsed})"
            );
            harness.mock_state.os_restore_withdrawn_window(1);
            harness.mock_state.update_window(1, |window| {
                window.visible = true;
                window.ordered_out = false;
            });
            harness.world().write_message(Event::SpaceChanged);
            harness.pump_frames(2);
            let state = harness.world().run_system_once(snapshot).unwrap();
            assert_eq!(original_space(&state).windows().count(), 2);
            assert_eq!(find_window_entity(1, harness.world()), entity);
        }
    }

    #[test]
    fn unavailable_minimized_and_hidden_windows_keep_bar_icons() {
        for visibility in [WindowVisibility::Minimized, WindowVisibility::Hidden] {
            let mut harness = TestHarness::new().with_windows(2);
            harness.pump_frames(15);
            let entity = find_window_entity(1, harness.world());
            harness.mock_state.update_window(1, |window| {
                window.visible = false;
                window.ordered_out = true;
            });
            harness.mock_state.os_withdraw_window(1);
            harness.world().write_message(Event::SpaceChanged);
            harness.pump_frames(2);
            harness.world().entity_mut(entity).insert(visibility);
            let state = harness.world().run_system_once(snapshot).unwrap();
            assert_eq!(state.displays[0].spaces[0].windows().count(), 2);
        }
    }

    #[test]
    fn failed_presentation_inventory_keeps_unavailable_bar_identity() {
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(15);
        let entity = find_window_entity(0, harness.world());
        harness.mock_state.update_window(0, |window| {
            window.visible = false;
            window.ordered_out = true;
        });
        harness
            .mock_state
            .set_presentation_inventory_available(false);
        harness.mock_state.os_withdraw_window(0);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(original_space(&state).windows().count(), 1);
        assert!(
            !harness
                .world()
                .get::<WindowUnavailable>(entity)
                .unwrap()
                .excludes_from_layout_projection()
        );
        harness
            .mock_state
            .set_presentation_inventory_available(true);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(original_space(&state).windows().count(), 0);
    }

    #[test]
    fn inactive_fullscreen_retains_application_identity_without_ax() {
        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(15);
        harness.mock_state.os_withdraw_window(0);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        hide_space(&mut harness);
        harness
            .world()
            .query::<&mut crate::ecs::native_space::NativeSpace>()
            .single_mut(harness.world())
            .unwrap()
            .kind = spool_shared_types::state::SpaceKind::Fullscreen;
        let state = harness.world().run_system_once(snapshot).unwrap();
        let space = &state.displays[0].spaces[0];
        assert_eq!(space.kind, spool_shared_types::state::SpaceKind::Fullscreen);
        assert!(!space.visible);
        let window = space.windows().next().expect("fullscreen application icon");
        assert_eq!(window.bundle_id, "test");
        assert_eq!(window.app_name, "TestApp");
        assert!(window.focused);
    }

    #[test]
    fn switching_spaces_retains_the_old_deck_beside_the_new_expanded_space() {
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(15);
        let other = TEST_WORKSPACE_ID + 1;
        harness
            .mock_state
            .activate_workspace(crate::tests::TEST_DISPLAY_ID, other, false);
        harness = harness.with_workspace_window(10, other, |_| {});
        for id in 0..2 {
            harness.mock_state.os_withdraw_window(id);
        }
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(15);
        let state = harness.world().run_system_once(snapshot).unwrap();
        let spaces = &state.displays[0].spaces;
        let old = spaces
            .iter()
            .find(|space| space.id == TEST_WORKSPACE_ID)
            .unwrap();
        let new = spaces.iter().find(|space| space.id == other).unwrap();
        assert!(!old.visible);
        assert_eq!(
            old.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(new.visible);
        assert_eq!(
            new.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[test]
    fn unresolved_surface_stays_hidden_until_ax_discovery() {
        use crate::tests::{TEST_DISPLAY_ID, TEST_PROCESS_ID};
        use bevy::math::IRect;

        let other = TEST_WORKSPACE_ID + 1;
        let mut harness = TestHarness::new().with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, other],
        );
        // The OS knows this surface before launch, but it has never entered ECS.
        let _window =
            harness
                .mock_state
                .spawn_window(TEST_PROCESS_ID, other, 10, IRect::new(0, 0, 400, 400));
        harness.mock_state.os_withdraw_window(10);
        harness.pump_frames(15);
        let manager = harness.world().resource::<WindowManager>();
        assert!(
            manager
                .window_owners_in_session()
                .unwrap()
                .contains_key(&10)
        );
        let before = harness.world().run_system_once(snapshot).unwrap();
        let space = before.displays[0]
            .spaces
            .iter()
            .find(|space| space.id == other)
            .unwrap();
        assert!(!space.visible);
        assert_eq!(space.windows().count(), 0);
        assert!(
            space.is_empty(),
            "a native surface without AX identity is not a Bar window"
        );
        assert!(space.columns.is_empty() && space.floating.is_empty());
        assert!(
            harness
                .world()
                .run_system_once(|windows: crate::ecs::params::Windows| windows.find(10).is_none())
                .unwrap()
        );
        let layout = BarLayout::resolve(&before.displays[0], 1200.0);
        assert_eq!(
            layout
                .items
                .iter()
                .filter(|item| matches!(item.kind, ItemKind::Window { window_id: 10, .. }))
                .count(),
            0
        );
        assert!(
            layout.items.iter().any(|item| matches!(
                item.kind,
                ItemKind::Placeholder { space_id } if space_id == other
            )),
            "an unresolved surface must not replace the empty Space placeholder"
        );

        // Reproduce the reported sequence without operating the user's desktop.
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, other, false);
        harness.mock_state.os_restore_withdrawn_window(10);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(15);
        let after = harness.world().run_system_once(snapshot).unwrap();
        let space = after.displays[0]
            .spaces
            .iter()
            .find(|space| space.id == other)
            .unwrap();
        assert!(space.visible);
        assert_eq!(
            space.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[test]
    fn floating_icon_and_focus_become_available_after_identity_discovery() {
        use crate::config::{Config, MainOptions, WindowParams};
        use crate::tests::TEST_PROCESS_ID;
        use bevy::math::IRect;
        use spool_shared_types::commands::{Action, Operation};

        for operation in [Operation::FocusFloating, Operation::FocusOtherLayer] {
            let mut floating = WindowParams::new("^Window 10$", None);
            floating.floating = Some(true);
            let config: Config = (MainOptions::default(), vec![floating]).into();
            let mut harness = TestHarness::new()
                .with_config(config)
                .with_windows(1)
                .with_focused_window(0);
            drop(harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                TEST_WORKSPACE_ID,
                10,
                IRect::new(100, 100, 500, 400),
            ));
            harness.mock_state.os_withdraw_window(10);
            // AlDente's AXWindows yields an application element, not a window ID.
            harness
                .mock_state
                .set_application_inventory_complete(TEST_PROCESS_ID, false);
            harness.pump_frames(15);
            harness.mock_state.take_focus_requests();
            harness.world().write_message(Event::ActionRequested {
                action: Action::Window(operation.clone()),
            });
            harness.pump_frames(5);
            assert!(harness.mock_state.take_focus_requests().is_empty());
            assert_eq!(bar_window_ids(&mut harness), vec![0]);
            assert!(
                harness
                    .world()
                    .run_system_once(|windows: crate::ecs::params::Windows| windows
                        .find(10)
                        .is_none())
                    .unwrap()
            );

            harness.mock_state.os_restore_withdrawn_window(10);
            harness
                .mock_state
                .set_application_inventory_complete(TEST_PROCESS_ID, true);
            harness.world().write_message(Event::SpaceChanged);
            harness.pump_frames(15);
            harness.mock_state.focus_window(0);
            harness.pump_frames(5);
            harness.mock_state.take_focus_requests();
            let state = harness.world().run_system_once(snapshot).unwrap();
            assert_eq!(
                original_space(&state)
                    .floating
                    .iter()
                    .map(|w| w.id)
                    .collect::<Vec<_>>(),
                vec![10]
            );
            harness.world().write_message(Event::ActionRequested {
                action: Action::Window(operation),
            });
            harness.pump_frames(5);
            assert!(harness.mock_state.take_focus_requests().contains(&10));
            let entity = find_window_entity(10, harness.world());
            assert!(harness.world().get::<FocusedMarker>(entity).is_some());
        }
    }

    fn unresolved_fullscreen_harness() -> TestHarness {
        use crate::tests::{TEST_DISPLAY_ID, TEST_PROCESS_ID};
        use bevy::math::IRect;

        let fullscreen = TEST_WORKSPACE_ID + 1;
        let mut harness = TestHarness::new().with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, fullscreen],
        );
        drop(harness.mock_state.spawn_window(
            TEST_PROCESS_ID,
            fullscreen,
            10,
            IRect::new(0, 0, 1200, 800),
        ));
        harness
            .mock_state
            .update_window(10, |window| window.is_full_screen = true);
        harness.mock_state.os_withdraw_window(10);
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, fullscreen, true);
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
        harness.pump_frames(15);
        assert!(bar_window_ids(&mut harness).is_empty());
        harness
    }

    fn assert_tracked_fullscreen_icon(harness: &mut TestHarness, visible: bool) {
        let fullscreen = TEST_WORKSPACE_ID + 1;
        let state = harness.world().run_system_once(snapshot).unwrap();
        let space = state.displays[0]
            .spaces
            .iter()
            .find(|space| space.id == fullscreen)
            .unwrap();
        assert_eq!(space.kind, spool_shared_types::state::SpaceKind::Fullscreen);
        assert_eq!(space.visible, visible);
        assert_eq!(space.windows().map(|w| w.id).collect::<Vec<_>>(), vec![10]);
        let layout = crate::bar::layout::tests::collapsed_layout(&state.displays[0], 1200.0);
        assert_eq!(
            layout
                .items
                .iter()
                .filter(|item| matches!(
                    item.kind,
                    ItemKind::Window { window_id: 10, space_id, fullscreen: true, collapsed, .. }
                        if space_id == fullscreen && collapsed != visible
                ))
                .count(),
            1
        );
    }

    #[test]
    fn startup_fullscreen_icon_survives_activation_and_ax_discovery() {
        use crate::ecs::{FullscreenDefaultsDeferred, WindowDefaultsPending};
        use crate::tests::TEST_DISPLAY_ID;

        let fullscreen = TEST_WORKSPACE_ID + 1;
        let mut harness = unresolved_fullscreen_harness();
        harness.mock_state.os_restore_withdrawn_window(10);
        let native_frame = harness.mock_state.actual_window_frame(10);
        for visible in [true, false, true, false, true] {
            if visible {
                harness.mock_state.os_restore_withdrawn_window(10);
            } else {
                harness.mock_state.os_withdraw_window(10);
            }
            harness.mock_state.activate_workspace(
                TEST_DISPLAY_ID,
                if visible {
                    fullscreen
                } else {
                    TEST_WORKSPACE_ID
                },
                visible,
            );
            harness.world().write_message(Event::SpaceChanged);
            for _ in 0..15 {
                harness.pump_frames(1);
                assert_tracked_fullscreen_icon(&mut harness, visible);
            }
        }
        assert_eq!(harness.mock_state.actual_window_frame(10), native_frame);
        assert_eq!(harness.mock_state.frame_write_attempts(10), 0);
        assert_eq!(harness.mock_state.resize_write_attempts(10), 0);
        let window = find_window_entity(10, harness.world());
        assert!(
            harness
                .world()
                .get::<FullscreenDefaultsDeferred>(window)
                .is_some()
        );

        harness.mock_state.update_window(10, |window| {
            window.workspace_id = TEST_WORKSPACE_ID;
            window.is_full_screen = false;
        });
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID, false);
        harness
            .mock_state
            .destroy_workspace(TEST_DISPLAY_ID, fullscreen);
        harness.world().write_message(Event::SpaceDestroyed {
            space_id: fullscreen,
        });
        harness.pump_frames(20);
        assert!(
            harness
                .world()
                .get::<FullscreenDefaultsDeferred>(window)
                .is_none()
        );
        assert!(
            harness
                .world()
                .get::<WindowDefaultsPending>(window)
                .is_none()
        );
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(state.displays[0].spaces.len(), 1);
        assert_eq!(state.displays[0].spaces[0].id, TEST_WORKSPACE_ID);
        assert_eq!(
            state.displays[0].spaces[0]
                .windows()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[test]
    fn startup_fullscreen_icon_appears_after_delayed_ax_discovery() {
        use crate::ecs::topology::NativeTopology;
        use crate::tests::{TEST_DISPLAY_ID, TEST_PROCESS_ID};

        let mut harness = unresolved_fullscreen_harness();
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, TEST_WORKSPACE_ID + 1, true);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(1);
        assert!(bar_window_ids(&mut harness).is_empty());
        let generation = harness.world().resource::<NativeTopology>().generation();

        harness.mock_state.os_restore_withdrawn_window(10);
        harness.world().write_message(Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::Application(TEST_PROCESS_ID),
        });
        harness.pump_frames(1);
        assert_eq!(
            harness.world().resource::<NativeTopology>().generation(),
            generation
        );
        assert_tracked_fullscreen_icon(&mut harness, true);
        for _ in 0..15 {
            harness.pump_frames(1);
            assert_tracked_fullscreen_icon(&mut harness, true);
        }
    }

    #[test]
    fn startup_fullscreen_discovery_retries_failed_membership() {
        use crate::tests::TEST_DISPLAY_ID;

        let fullscreen = TEST_WORKSPACE_ID + 1;
        let mut harness = unresolved_fullscreen_harness();
        harness
            .mock_state
            .activate_workspace(TEST_DISPLAY_ID, fullscreen, true);
        harness.mock_state.os_restore_withdrawn_window(10);
        harness
            .mock_state
            .script_workspace_membership_queries(fullscreen, std::iter::repeat_n(Err(()), 100));
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(1);
        let window = find_window_entity(10, harness.world());
        assert!(
            harness
                .world()
                .query::<&LayoutStrip>()
                .iter(harness.world())
                .all(|strip| !strip.contains(window))
        );
        harness
            .mock_state
            .script_workspace_membership_queries(fullscreen, []);
        harness.pump_frames(20);
        assert_tracked_fullscreen_icon(&mut harness, true);
    }

    fn unresolved_harness() -> TestHarness {
        use crate::tests::{TEST_DISPLAY_ID, TEST_PROCESS_ID};
        use bevy::math::IRect;
        let other = TEST_WORKSPACE_ID + 1;
        let mut harness = TestHarness::new().with_display(
            TEST_DISPLAY_ID,
            IRect::new(0, 0, 1200, 800),
            vec![TEST_WORKSPACE_ID, other],
        );
        for id in [12, 10, 11] {
            drop(harness.mock_state.spawn_window(
                TEST_PROCESS_ID,
                other,
                id,
                IRect::new(0, 0, 400, 400),
            ));
            harness.mock_state.os_withdraw_window(id);
        }
        harness.pump_frames(15);
        harness
    }

    fn bar_window_ids(harness: &mut TestHarness) -> Vec<i32> {
        harness
            .world()
            .run_system_once(snapshot)
            .unwrap()
            .displays
            .iter()
            .flat_map(|display| &display.spaces)
            .flat_map(BarSpace::windows)
            .map(|window| window.id)
            .collect()
    }

    #[test]
    fn unresolved_surfaces_stay_hidden_across_close_and_failed_inventory() {
        let mut harness = unresolved_harness();
        assert!(bar_window_ids(&mut harness).is_empty());
        assert!(bar_window_ids(&mut harness).is_empty());
        harness.mock_state.os_settle_withdrawn_surface(11);
        assert!(bar_window_ids(&mut harness).is_empty());
        harness
            .mock_state
            .set_window_server_inventory_available(false);
        assert!(bar_window_ids(&mut harness).is_empty());
        harness
            .mock_state
            .set_window_server_inventory_available(true);
        assert!(bar_window_ids(&mut harness).is_empty());
        harness
            .mock_state
            .script_workspace_membership_queries(TEST_WORKSPACE_ID + 1, [Err(())]);
        assert!(bar_window_ids(&mut harness).is_empty());
        assert!(bar_window_ids(&mut harness).is_empty());
    }

    #[test]
    fn ordered_out_calendar_surface_is_not_a_cold_start_window() {
        let mut harness = unresolved_harness();
        // Calendar's live trace: no AX windows; a full-size, layer-0, alpha-1
        // surface survives in the broad (0x7) list, but not the ordered (0x2) list.
        harness.mock_state.os_restore_withdrawn_window(10);
        harness.mock_state.update_window(10, |window| {
            window.ordered_out = true;
            window.visible = false;
        });
        harness.mock_state.os_withdraw_window(10);
        // Off-screen because its Space is inactive is different from ordered out.
        harness.mock_state.os_restore_withdrawn_window(11);
        harness
            .mock_state
            .update_window(11, |window| window.visible = false);
        harness.mock_state.os_withdraw_window(11);
        let manager = harness.world().resource::<WindowManager>();
        assert!(
            manager
                .window_owners_in_session()
                .unwrap()
                .contains_key(&10)
        );
        assert!(
            manager
                .windows_in_workspace(TEST_WORKSPACE_ID + 1)
                .unwrap()
                .contains(&10)
        );
        assert!(
            !manager
                .presentation_windows_in_workspace(TEST_WORKSPACE_ID + 1)
                .unwrap()
                .contains(&10)
        );
        assert!(bar_window_ids(&mut harness).is_empty());
    }

    #[test]
    fn ignored_ax_identity_never_returns_as_a_bar_window() {
        use crate::tests::TEST_PROCESS_ID;
        let mut harness = unresolved_harness();
        harness.mock_state.os_restore_withdrawn_window(10);
        harness
            .mock_state
            .update_window(10, |window| window.role = "AXUnknown".into());
        harness.world().write_message(Event::ReconcileWindows {
            scope: crate::events::ReconcileScope::Application(TEST_PROCESS_ID),
        });
        harness.pump_frames(2);
        assert!(bar_window_ids(&mut harness).is_empty());
        // Removing AX access again must not reclassify a rejected window as new.
        harness.mock_state.os_withdraw_window(10);
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        assert!(bar_window_ids(&mut harness).is_empty());
    }

    #[test]
    fn tracked_withdrawn_windows_keep_icons_without_exposing_unresolved_surfaces() {
        let mut harness = unresolved_harness();
        harness.mock_state.os_restore_withdrawn_window(10);
        harness.mock_state.activate_workspace(
            crate::tests::TEST_DISPLAY_ID,
            TEST_WORKSPACE_ID + 1,
            false,
        );
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(15);
        let _tracked = find_window_entity(10, harness.world());
        harness.mock_state.os_withdraw_window(10);
        harness.mock_state.activate_workspace(
            crate::tests::TEST_DISPLAY_ID,
            TEST_WORKSPACE_ID,
            false,
        );
        harness.world().write_message(Event::SpaceChanged);
        harness.pump_frames(2);
        assert_eq!(bar_window_ids(&mut harness), vec![10]);
        let state = harness.world().run_system_once(snapshot).unwrap();
        let space = state.displays[0]
            .spaces
            .iter()
            .find(|s| s.id == TEST_WORKSPACE_ID + 1)
            .unwrap();
        assert_eq!(space.windows().map(|w| w.id).collect::<Vec<_>>(), vec![10]);
    }

    #[test]
    fn application_metadata_does_not_promote_unresolved_surfaces() {
        let mut harness = unresolved_harness();
        harness
            .mock_state
            .update_app(crate::tests::TEST_PROCESS_ID, |app| app.bundle_id.clear());
        assert!(bar_window_ids(&mut harness).is_empty());
        harness
            .mock_state
            .update_app(crate::tests::TEST_PROCESS_ID, |app| {
                app.bundle_id = "test".into();
            });
        assert!(bar_window_ids(&mut harness).is_empty());
        let entities = harness
            .world()
            .query::<(Entity, &Application)>()
            .iter(harness.world())
            .map(|(entity, _)| entity)
            .collect::<Vec<_>>();
        for entity in entities {
            harness.world().entity_mut(entity).remove::<Application>();
        }
        assert!(bar_window_ids(&mut harness).is_empty());
    }
}

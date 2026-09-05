use std::collections::{HashMap, HashSet};

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::Has;
use bevy::ecs::system::{Query, Res, SystemParam};
use spool_shared_types::windowset::ColumnKind;
use tracing::warn;

use super::BarSnapshot;
use super::model::{BarColumn, BarSpace, BarWindow};
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
        Has<WindowUnavailable>,
        Has<WindowVisibility>,
    ),
>;

#[derive(SystemParam)]
pub(crate) struct BarStateParams<'w, 's> {
    state: QueryStateParams<'w, 's>,
    strips: Query<'w, 's, &'static LayoutStrip>,
    windows: BarWindows<'w, 's>,
    apps: Query<'w, 's, &'static Application>,
    window_manager: Res<'w, WindowManager>,
}

impl BarStateParams<'_, '_> {
    pub fn extract(&self) -> crate::errors::Result<BarSnapshot> {
        let mut snapshot =
            BarSnapshot::from_state(&self.state.extract()?, &self.state.extract_window_set());
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
                if let Some(strip) = strips.get(&space.id) {
                    self.project_windows(space, strip, &floating);
                }
            }
        }
        Ok(snapshot)
    }

    fn project_windows(&self, space: &mut BarSpace, strip: &LayoutStrip, floating: &[Entity]) {
        let known = space
            .windows()
            .map(|window| (window.id, window.clone()))
            .collect::<HashMap<_, _>>();
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
                    .filter_map(|entity| self.window_record(entity, space.visible, &known))
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
        let membership = self.window_manager.windows_in_workspace(space.id).map(|ids| ids.into_iter().collect::<HashSet<_>>()).inspect_err(|error| warn!(space_id = space.id, %error, "unable to observe floating Bar membership")).ok();
        space.floating = floating
            .iter()
            .copied()
            .filter(|entity| {
                self.windows.get(*entity).is_ok_and(|(window, ..)| {
                    membership.as_ref().map_or_else(
                        || strip.contains(*entity) || known.contains_key(&window.id()),
                        |ids| ids.contains(&window.id()),
                    )
                })
            })
            .filter_map(|entity| self.window_record(entity, space.visible, &known))
            .collect();
    }

    fn window_record(
        &self,
        entity: Entity,
        space_visible: bool,
        known: &HashMap<i32, BarWindow>,
    ) -> Option<BarWindow> {
        let (window, _, parent, _, focused, unavailable, hidden) = self.windows.get(entity).ok()?;
        let app = self.apps.get(parent.parent()).ok()?;
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
            focused: space_visible && focused && !unavailable,
            visible: space_visible && !hidden && !unavailable,
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

    fn hide_space(harness: &mut TestHarness) {
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
        let space = &state.displays[0].spaces[0];
        assert!(!space.visible);
        assert_eq!(
            space.windows().map(|window| window.id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let layout = BarLayout::resolve(&state.displays[0], 1200.0, 0.0);
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
        hide_space(&mut harness);
        let state = harness.world().run_system_once(snapshot).unwrap();
        assert_eq!(
            state.displays[0].spaces[0]
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
        assert_eq!(state.displays[0].spaces[0].windows().count(), 2);
        assert!(
            state.displays[0].spaces[0]
                .windows()
                .all(|window| !window.focused && !window.visible)
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
            state.displays[0].spaces[0]
                .windows()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![0]
        );
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
        assert!(!window.focused);
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
    fn unresolved_startup_surface_appears_after_activation_and_ax_discovery() {
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
}

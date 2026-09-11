use bevy::prelude::*;

use crate::ecs::layout::LayoutStrip;

use super::*;

#[test]
fn snapshot_preserves_legacy_tab_entries_inside_stacks() {
    use bevy::ecs::system::RunSystemOnce;
    use spool_shared_types::windowset::{ColumnKind, StackItemSet};

    let mut harness = TestHarness::new().with_windows(3);
    harness.pump_frames(5);
    let members = [1, 2].map(|id| find_window_entity(id, harness.world()));
    let independent = find_window_entity(0, harness.world());
    let world = harness.world();
    {
        let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
        strip.append_tab_group(&members);
        strip.stack(members[0]).unwrap();
        assert!(!strip.is_inactive_tab(members[0]));
        assert!(strip.is_inactive_tab(members[1]));
        assert!(!strip.is_inactive_tab(independent));
    }
    let snapshot = world
        .run_system_once(|state: crate::ecs::state::QueryStateParams| state.extract_window_set())
        .unwrap();
    let columns = &snapshot.workspace(TEST_WORKSPACE_ID).unwrap().columns;
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].kind, ColumnKind::Stack);
    assert_eq!(columns[0].items.len(), 2);
    assert!(matches!(columns[0].items[0], StackItemSet::Single(_)));
    assert!(matches!(columns[0].items[1], StackItemSet::Tabs(_)));
    assert_eq!(
        columns[0].items[1]
            .windows()
            .iter()
            .map(|window| window.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        columns[0]
            .windows()
            .map(|window| window.id)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

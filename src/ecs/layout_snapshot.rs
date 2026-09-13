//! Identity binding for delayed layout operations, shared by snapshot extraction
//! and both immediate and deferred execution paths.

use bevy::ecs::resource::Resource;
use spool_shared_types::windowset::{LayoutSnapshot, WindowIdentity};

use super::params::Windows;

#[derive(Resource)]
pub(crate) struct LayoutSession([u8; 16]);

impl Default for LayoutSession {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4().into_bytes())
    }
}

impl LayoutSession {
    pub(crate) fn capture<'a>(
        &self,
        windows: &Windows,
        strips: impl Iterator<Item = &'a super::layout::LayoutStrip>,
    ) -> LayoutSnapshot {
        let strips = strips.collect::<Vec<_>>();
        LayoutSnapshot {
            structures: strips
                .iter()
                .flat_map(|strip| {
                    strip
                        .columns()
                        .flat_map(|column| column.window_iter())
                        .filter_map(|entity| {
                            Some((
                                windows.get_any(entity)?.id(),
                                (strip.id(), strip.structure_revision()),
                            ))
                        })
                })
                .collect(),
            session: self.0,
            columns: strips
                .iter()
                .flat_map(|strip| {
                    strip.columns().enumerate().flat_map(|(index, column)| {
                        let state = strip.column_state(index);
                        column.window_iter().filter_map(move |entity| {
                            Some((
                                windows.get_any(entity)?.id(),
                                (state?.id.0, state?.intent_revision),
                            ))
                        })
                    })
                })
                .collect(),
            windows: windows
                .iter_any()
                .map(|(window, entity)| {
                    (
                        window.id(),
                        WindowIdentity {
                            entity: entity.to_bits(),
                            incarnation: window.incarnation(),
                        },
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn accepts(
        &self,
        snapshot: &LayoutSnapshot,
        op: spool_shared_types::windowset::LayoutOp,
        windows: &Windows,
    ) -> bool {
        self.0 == snapshot.session && op.targets().all(|id| matches_window(snapshot, id, windows))
    }
}

pub(crate) fn matches_window(
    snapshot: &LayoutSnapshot,
    id: crate::platform::WinID,
    windows: &Windows,
) -> bool {
    let Some(identity) = snapshot.windows.get(&id) else {
        return false;
    };
    windows.find_any(id).is_some_and(|(window, entity)| {
        entity.to_bits() == identity.entity && window.incarnation() == identity.incarnation
    })
}

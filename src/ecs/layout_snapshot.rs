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
    pub(crate) fn capture(&self, windows: &Windows) -> LayoutSnapshot {
        LayoutSnapshot {
            session: self.0,
            windows: windows
                .iter()
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
    windows.find(id).is_some_and(|(window, entity)| {
        entity.to_bits() == identity.entity && window.incarnation() == identity.incarnation
    })
}

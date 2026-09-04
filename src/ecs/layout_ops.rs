//! Replays a script's [`LayoutOp`]s (returned by a Lua handler that
//! transformed a `WindowSet`) against the live world.
//!
//! Unlike other command handlers, each op names a window explicitly rather
//! than acting on the focused one. Because the snapshot a script transformed
//! may be a frame stale, each op is applied independently and best-effort: one
//! that can't be resolved is logged at debug and skipped without affecting
//! the rest.

use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, Without};
use bevy::ecs::system::{Commands, Query};
use bevy::platform::collections::HashSet;
use spool_shared_types::windowset::LayoutOp;
use tracing::debug;

use crate::commands::{Action, MoveFocus};
use crate::ecs::focus::FocusWindow;
use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::params::Windows;
use crate::ecs::{
    ActiveWorkspaceMarker, Floating, SendMessageTrigger, SpawnCommandsExt, WidthRatio, Window,
};
use crate::events::Event;
use crate::manager::{Origin, Size};

/// Applies the layout operations a Lua handler returned.
pub(crate) fn apply_layout_ops(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>), Without<Window>>,
    mut commands: Commands,
) {
    let batches: Vec<Vec<LayoutOp>> = messages
        .read()
        .filter_map(|message| match message {
            Event::ActionRequested {
                action: Action::Layout(ops),
            } => Some(ops.clone()),
            _ => None,
        })
        .collect();

    for ops in batches {
        // `Floating` inserts don't take effect until commands flush, so track
        // what this batch floated to tell a just-floated window from one still
        // genuinely tiled (needed by `SetFrame`).
        let mut floated: HashSet<Entity> = HashSet::new();
        for op in ops {
            apply(op, &windows, &mut workspaces, &mut floated, &mut commands);
        }
    }
}

/// Applies one op, or explains why it could not be.
#[allow(clippy::too_many_lines)]
fn apply(
    op: LayoutOp,
    windows: &Windows,
    workspaces: &mut Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>), Without<Window>>,
    floated: &mut HashSet<Entity>,
    commands: &mut Commands,
) {
    // Resolve up front so every arm below can assume the window still exists.
    let entity = if let Some(window_id) = op.target() {
        let Some((_, entity)) = windows.find(window_id) else {
            debug!(
                target: "spool::lua",
                "skipping {op:?}: window {window_id} is gone since the handler ran"
            );
            return;
        };
        Some(entity)
    } else {
        None
    };

    match op {
        LayoutOp::Focus(_) => {
            let entity = entity.expect("Focus names a window");
            commands.trigger(FocusWindow {
                entity,
                raise: true,
            });
        }

        LayoutOp::Swap(_, other) => {
            let entity = entity.expect("Swap names a window");
            let Some((_, other_entity)) = windows.find(other) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {other} is gone");
                return;
            };
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity) && strip.contains(other_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            let (Ok(left), Ok(right)) = (strip.index_of(entity), strip.index_of(other_entity))
            else {
                return;
            };
            strip.swap(left, right);
            commands.reshuffle_around(entity);
        }

        LayoutOp::MoveToWorkspace {
            space_id, follow, ..
        } => {
            let entity = entity.expect("MoveToWorkspace names a window");
            let Some(window) = windows.get(entity) else {
                return;
            };
            commands.trigger(SendMessageTrigger(Event::action_requested(
                Action::MoveWindowToSpace {
                    window_id: window.id(),
                    space_id,
                    move_focus: if follow {
                        MoveFocus::Follow
                    } else {
                        MoveFocus::Stay
                    },
                },
            )));
        }

        LayoutOp::View { space_id } => {
            commands.trigger(SendMessageTrigger(Event::action_requested(
                Action::FocusSpace { space_id },
            )));
        }

        LayoutOp::SetFloating { floating, .. } => {
            let entity = entity.expect("SetFloating names a window");
            if floating {
                floated.insert(entity);
            } else {
                floated.remove(&entity);
            }
            set_floating(entity, floating, workspaces, commands);
        }

        LayoutOp::SetWidth { ratio, .. } => {
            let entity = entity.expect("SetWidth names a window");
            if !ratio.is_finite() || ratio <= 0.0 {
                debug!(target: "spool::lua", "skipping {op:?}: a width ratio must be positive");
                return;
            }
            // The layout pipeline turns the ratio into an actual width against
            // whichever display the window is on, so this does not have to.
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(WidthRatio(ratio));
            }
            // Stacked siblings share a column, and so its width.
            if let Some((strip, _)) = workspaces.iter().find(|(strip, _)| strip.contains(entity))
                && let Some(Column::Stack(stack)) = strip
                    .index_of(entity)
                    .ok()
                    .and_then(|index| strip.get(index).ok())
            {
                for sibling in stack.iter().flat_map(StackItem::window_iter) {
                    if sibling != entity
                        && let Ok(mut entity_commands) = commands.get_entity(sibling)
                    {
                        entity_commands.try_insert(WidthRatio(ratio));
                    }
                }
            }
            commands.reshuffle_around(entity);
        }

        LayoutOp::SetFrame { frame, .. } => {
            let entity = entity.expect("SetFrame names a window");
            // The layout engine owns a tiled window's geometry and will move
            // it back, so warn if the target isn't floated.
            if !floated.contains(&entity)
                && windows
                    .get_tracked(entity)
                    .is_some_and(|(_, _, state)| state.is_tiled())
            {
                debug!(
                    target: "spool::lua",
                    "{op:?} targets a tiled window; float it first or the layout will move it back"
                );
            }
            let origin = Origin::new(frame.x, frame.y);
            let size = Size::new(frame.width.max(1), frame.height.max(1));
            commands.reposition_entity(entity, origin);
            commands.resize_entity(entity, size);
        }

        LayoutOp::Stack { onto, .. } => {
            let entity = entity.expect("Stack names a window");
            let Some((_, onto_entity)) = windows.find(onto) else {
                debug!(target: "spool::lua", "skipping {op:?}: window {onto} is gone");
                return;
            };
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity) && strip.contains(onto_entity))
            else {
                debug!(target: "spool::lua", "skipping {op:?}: the two windows share no strip");
                return;
            };
            if strip.stack(entity).is_err() {
                debug!(target: "spool::lua", "skipping {op:?}: the layout refused the stack");
                return;
            }
            commands.reshuffle_around(entity);
        }

        LayoutOp::Unstack(_) => {
            let entity = entity.expect("Unstack names a window");
            let Some((mut strip, _)) = workspaces
                .iter_mut()
                .find(|(strip, _)| strip.contains(entity))
            else {
                return;
            };
            if strip.unstack(entity).is_err() {
                debug!(target: "spool::lua", "skipping {op:?}: the window is not in a stack");
                return;
            }
            commands.reshuffle_around(entity);
        }
    }
}

/// Takes a window out of the tiling layout or puts it back. Mirrors
/// `toggle_floating_window`: floating→tiled has to re-append the window to a strip
/// itself, since nothing downstream does it automatically.
fn set_floating(
    entity: Entity,
    floating: bool,
    workspaces: &mut Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>), Without<Window>>,
    commands: &mut Commands,
) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        if floating {
            entity_commands.try_insert(Floating);
        } else {
            entity_commands.try_remove::<Floating>();
        }
    }

    if !floating
        && !workspaces.iter().any(|(strip, _)| strip.contains(entity))
        && let Some((mut strip, _)) = workspaces.iter_mut().find(|(_, active)| *active)
    {
        strip.append(entity);
        commands.reshuffle_around(entity);
    }
}

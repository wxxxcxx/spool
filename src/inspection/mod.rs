//! Read-only resource projections and independent native collection.

pub(crate) mod native;
pub(crate) mod protocol;
pub(crate) mod spool;
pub(crate) mod supervisor;
pub(crate) const WORKER_ARGUMENT: &str = "--spool-native-inspection-worker-v1";

pub(crate) fn worker_main() -> std::io::Result<()> {
    crate::platform::inspection::run()
}

use crate::events::Event;
use bevy::prelude::*;

pub(crate) fn register(app: &mut App) {
    app.add_systems(PreUpdate, serve.after(crate::commands::dispatch_actions));
}

fn serve(mut events: MessageReader<Event>, mut commands: Commands) {
    for event in events.read() {
        let Event::Inspect {
            request,
            respond_to,
        } = event
        else {
            continue;
        };
        let request = request.clone();
        let respond_to = respond_to.clone();
        commands.queue(move |world: &mut World| {
            let result = world.run_system_cached_with(spool::collect, request.clone());
            let report = result.unwrap_or_else(|_| {
                spool_shared_types::inspection::Report::failure(
                    &request,
                    "not_ready",
                    "retained state is unavailable",
                )
            });
            _ = respond_to.try_send(spool_shared_types::wire::Response::Inspection(Box::new(
                report,
            )));
        });
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}

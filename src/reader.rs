//! Where requests from other processes enter the daemon.
//!
//! [`spool_local_ipc`] owns singleton locking, Unix socket lifecycle,
//! authentication, framing, and deadlines. This module is the adapter from a
//! decoded request into Spool's event stream.

use bevy::tasks::{IoTaskPool, TaskPool};
use spool_local_ipc::{Delivery, Reply as IpcReply, Server, ServerGuard};
use spool_shared_types::wire::{Request, service_name};
use std::sync::Arc;
use std::thread;
use tracing::{error, warn};

use crate::errors::Result;
use crate::events::{Event, EventSender, Reply};

/// Starts the local IPC adapter and feeds decoded requests into the world.
pub struct CommandReader {
    events: EventSender,
}

/// Keeps the singleton lock and Unix listener alive for the main loop.
pub struct CommandReaderGuard {
    _server: ServerGuard,
}

impl CommandReader {
    /// Creates a reader that dispatches received requests through `events`.
    #[must_use]
    pub fn new(events: EventSender) -> Self {
        Self { events }
    }

    /// Claims the unique Spool instance and starts its Unix socket listener.
    ///
    /// # Errors
    ///
    /// Returns an error if another Spool instance is already running or the
    /// endpoint cannot be created safely.
    pub fn start(self) -> Result<CommandReaderGuard> {
        self.start_with_service(&service_name())
    }

    fn start_with_service(self, service: &str) -> Result<CommandReaderGuard> {
        let server = Server::bind(service)?;
        let (incoming, guard) = server.into_parts();
        thread::Builder::new()
            .name("spool-command-reader".to_string())
            .spawn(move || {
                loop {
                    match incoming.recv_blocking() {
                        Ok(delivery) => self.dispatch(delivery),
                        Err(spool_local_ipc::Error::PeerGone) => break,
                        // A malformed or unauthorized client is isolated to its
                        // connection; it must not stop the daemon listener.
                        Err(error) => warn!(%error, "reading IPC request"),
                    }
                }
            })?;

        Ok(CommandReaderGuard { _server: guard })
    }

    fn dispatch(&self, delivery: Delivery) {
        let Delivery {
            request,
            acknowledgement,
            reply,
            subscriber,
        } = delivery;

        match request {
            Request::Command(command) => {
                acknowledge(
                    send(&self.events, Event::Command { command }),
                    acknowledgement,
                    "command",
                );
            }
            Request::WindowSetApply(ops) => {
                acknowledge(
                    send(
                        &self.events,
                        Event::Command {
                            command: crate::commands::Command::Layout(ops),
                        },
                    ),
                    acknowledgement,
                    "window set apply",
                );
            }
            Request::Query(kind) => {
                answer(
                    self.events.clone(),
                    reply,
                    "state query",
                    move |respond_to| Event::StateQuery { kind, respond_to },
                );
            }
            Request::WindowSet => {
                answer(
                    self.events.clone(),
                    reply,
                    "window set query",
                    |respond_to| Event::WindowSetQuery { respond_to },
                );
            }
            Request::ScriptState(request) => {
                answer(
                    self.events.clone(),
                    reply,
                    "script state request",
                    move |respond_to| Event::ScriptState {
                        request,
                        respond_to,
                    },
                );
            }
            Request::Subscribe => {
                if let Some(subscriber) = subscriber {
                    _ = send(
                        &self.events,
                        Event::StateSubscribe {
                            subscriber: Arc::new(subscriber),
                        },
                    );
                } else {
                    warn!("subscribe request did not use subscription mode");
                }
            }
        }
    }
}

fn send(events: &EventSender, event: Event) -> std::result::Result<(), String> {
    events.send(event).map_err(|error| {
        error!(%error, "sending IPC event");
        error.to_string()
    })
}

fn acknowledge(
    result: std::result::Result<(), String>,
    acknowledgement: Option<spool_local_ipc::Acknowledgement>,
    what: &'static str,
) {
    let Some(acknowledgement) = acknowledgement else {
        warn!("{what} did not use send mode");
        return;
    };
    let sent = match result {
        Ok(()) => acknowledgement.accepted(),
        Err(message) => acknowledgement.rejected(message),
    };
    if let Err(error) = sent {
        warn!(%error, "acknowledging {what}");
    }
}

/// Queues a request in the world and completes its socket reply from the IO
/// pool, so a delayed ECS answer never stalls the listener.
fn answer(
    events: EventSender,
    reply: Option<IpcReply>,
    what: &'static str,
    request: impl FnOnce(Reply) -> Event + Send + 'static,
) {
    let Some(reply) = reply else {
        warn!("{what} did not use call mode");
        return;
    };

    let (tx, rx) = async_channel::bounded(1);
    if events
        .send(request(tx))
        .inspect_err(|error| error!(%error, "sending {what}"))
        .is_err()
    {
        return;
    }

    IoTaskPool::get_or_init(TaskPool::default)
        .spawn(async move {
            match rx.recv().await {
                Ok(response) => {
                    if let Err(error) = reply.send(&response) {
                        warn!(%error, "answering {what}");
                    }
                }
                Err(error) => error!(%error, "waiting for {what} response"),
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::CommandReader;
    use crate::events::{Event, EventSender};
    use spool_local_ipc::Client;
    use spool_shared_types::commands::Command;
    use spool_shared_types::state::StateQueryKind;
    use spool_shared_types::wire::{QueryPayload, Request, Response};
    use std::thread;

    fn service(test: &str) -> String {
        format!(
            "com.wxxxcxx.spool.reader-test.{test}.{}",
            std::process::id()
        )
    }

    #[test]
    fn a_shell_launch_publishes_the_same_ipc_as_a_service_launch() {
        let (events, _receiver) = EventSender::new();
        let _guard = CommandReader::new(events)
            .start_with_service(&service("foreground"))
            .expect("foreground IPC listener");
    }

    #[test]
    fn a_second_daemon_is_refused_until_the_first_exits() {
        let service = service("singleton");
        let (first_events, _first_receiver) = EventSender::new();
        let first = CommandReader::new(first_events)
            .start_with_service(&service)
            .expect("first daemon");

        let (second_events, _second_receiver) = EventSender::new();
        assert!(
            CommandReader::new(second_events)
                .start_with_service(&service)
                .is_err()
        );

        drop(first);
        let (third_events, _third_receiver) = EventSender::new();
        let _third = CommandReader::new(third_events)
            .start_with_service(&service)
            .expect("released daemon lock");
    }

    #[test]
    fn a_command_is_acknowledged_after_entering_the_event_queue() {
        let service = service("command-round-trip");
        let (events, receiver) = EventSender::new();
        let _guard = CommandReader::new(events)
            .start_with_service(&service)
            .expect("daemon");
        let client_service = service.clone();
        let client = thread::spawn(move || {
            Client::connect(&client_service)
                .expect("connect")
                .send(&Request::Command(Command::Quit))
        });

        assert!(matches!(
            receiver.recv().expect("queued event"),
            Event::Command {
                command: Command::Quit
            }
        ));
        client.join().expect("client thread").expect("acknowledged");
    }

    #[test]
    fn a_query_round_trips_through_the_ecs_reply_channel() {
        let service = service("query-round-trip");
        let (events, receiver) = EventSender::new();
        let _guard = CommandReader::new(events)
            .start_with_service(&service)
            .expect("daemon");
        let client_service = service.clone();
        let client = thread::spawn(move || {
            Client::connect(&client_service)
                .expect("connect")
                .call(&Request::Query(StateQueryKind::Spaces))
        });

        let Event::StateQuery { kind, respond_to } = receiver.recv().expect("query event") else {
            panic!("expected state query");
        };
        assert_eq!(kind, StateQueryKind::Spaces);
        respond_to
            .try_send(Response::Query(QueryPayload::Spaces(Vec::new())))
            .expect("ECS response");

        assert_eq!(
            client
                .join()
                .expect("client thread")
                .expect("query response"),
            Response::Query(QueryPayload::Spaces(Vec::new()))
        );
    }
}

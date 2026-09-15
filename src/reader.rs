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
pub struct RequestReader {
    events: EventSender,
    lifecycle: crate::lifecycle::Lifecycle,
}

/// Keeps the singleton lock and Unix listener alive for the main loop.
pub struct RequestReaderGuard {
    _server: ServerGuard,
}

impl RequestReader {
    /// Creates a reader that dispatches received requests through `events`.
    #[must_use]
    #[cfg(test)]
    pub fn new(events: EventSender) -> Self {
        Self::with_lifecycle(events, crate::lifecycle::Lifecycle::default())
    }

    pub(crate) fn with_lifecycle(
        events: EventSender,
        lifecycle: crate::lifecycle::Lifecycle,
    ) -> Self {
        Self { events, lifecycle }
    }

    /// Claims the unique Spool instance and starts its Unix socket listener.
    ///
    /// # Errors
    ///
    /// Returns an error if another Spool instance is already running or the
    /// endpoint cannot be created safely.
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive command/source branches share one admission or capture boundary"
    )]
    pub fn start(self) -> Result<RequestReaderGuard> {
        self.start_with_service(&service_name())
    }

    fn start_with_service(self, service: &str) -> Result<RequestReaderGuard> {
        let server = Server::bind(service)?;
        let (incoming, guard) = server.into_parts();
        thread::Builder::new()
            .name("spool-request-reader".to_string())
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

        Ok(RequestReaderGuard { _server: guard })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "readiness and response routing share a single delivery boundary"
    )]
    fn dispatch(&self, delivery: Delivery) {
        let Delivery {
            request,
            acknowledgement,
            reply,
            subscriber,
        } = delivery;

        if let Some(reason) = self.lifecycle.rejection() {
            use crate::commands::Action;
            use crate::lifecycle::Phase;
            use spool_shared_types::wire::{AdmissionReceipt, Response};
            let can_quit = self.lifecycle.phase() != Phase::Stopping;
            let quitting = can_quit
                && matches!(&request, Request::Command(command) if command.action == Action::Quit);
            if let Request::Command(command) = &request {
                if let Some(reply) = reply {
                    let result = if quitting {
                        self.lifecycle.set(Phase::Stopping);
                        Ok(())
                    } else {
                        Err(reason.into())
                    };
                    _ = reply.send(&Response::Admission(AdmissionReceipt::from_result(
                        command.request_id.clone(),
                        result,
                    )));
                    if quitting {
                        _ = self.events.send(Event::Exit);
                    }
                }
            } else if let Request::Inspect(request) = &request {
                if let Some(reply) = reply {
                    _ = reply.send(&Response::Inspection(Box::new(
                        spool_shared_types::inspection::Report::failure(request, reason, reason),
                    )));
                }
            } else if can_quit && matches!(&request, Request::Dispatch(Action::Quit)) {
                if let Some(acknowledgement) = acknowledgement {
                    self.lifecycle.set(Phase::Stopping);
                    _ = acknowledgement.accepted();
                    _ = self.events.send(Event::Exit);
                }
            } else {
                if let Some(reply) = reply {
                    _ = reply.send(&Response::Error(reason.into()));
                }
                if let Some(acknowledgement) = acknowledgement {
                    _ = acknowledgement.rejected(reason);
                }
            }
            return;
        }

        match request {
            Request::Inspect(request) => answer(
                self.events.clone(),
                reply,
                "resource inspection",
                move |respond_to| Event::Inspect {
                    request,
                    respond_to,
                },
            ),
            Request::Command(request) => {
                let quitting = matches!(request.action, crate::commands::Action::Quit);
                let restarting = matches!(request.action, crate::commands::Action::Restart);
                let events = self.events.clone();
                answer_then(
                    self.events.clone(),
                    reply,
                    "command admission",
                    move |respond_to| Event::CheckedActionRequested {
                        request,
                        respond_to,
                    },
                    move |response| {
                        if matches!(response, spool_shared_types::wire::Response::Admission(receipt) if receipt.status == spool_shared_types::wire::AdmissionStatus::Accepted)
                        {
                            if quitting {
                                _ = events.send(Event::Exit);
                            }
                            if restarting
                                && let Err(error) =
                                    crate::platform::service::Service::request_restart()
                            {
                                error!(%error, "unable to start admitted service restart");
                            }
                        }
                    },
                );
            }
            Request::Dispatch(action) => {
                acknowledge(dispatch(&self.events, action), acknowledgement, "action");
            }
            Request::WindowSetApply(ops) => {
                acknowledge(
                    send(
                        &self.events,
                        Event::ActionRequested {
                            action: crate::commands::Action::Layout(ops),
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
            Request::Subscribe { raw } => {
                if let Some(subscriber) = subscriber {
                    if let Some(acknowledgement) = acknowledgement
                        && acknowledgement.accepted().is_err()
                    {
                        return;
                    }
                    _ = send(
                        &self.events,
                        Event::StateSubscribe {
                            subscriber: Arc::new(subscriber),
                            raw,
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

fn dispatch(
    events: &EventSender,
    action: crate::commands::Action,
) -> std::result::Result<(), String> {
    events.dispatch(action).map_err(|error| {
        error!(%error, "dispatching IPC action");
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
    answer_then(events, reply, what, request, |_| {});
}

fn answer_then(
    events: EventSender,
    reply: Option<IpcReply>,
    what: &'static str,
    request: impl FnOnce(Reply) -> Event + Send + 'static,
    after_reply: impl FnOnce(&spool_shared_types::wire::Response) + Send + 'static,
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
                    after_reply(&response);
                }
                Err(error) => error!(%error, "waiting for {what} response"),
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::{RequestReader, RequestReaderGuard};
    use crate::events::{Event, EventSender};
    use spool_local_ipc::Client;
    use spool_shared_types::commands::Action;
    use spool_shared_types::state::{StateEvent, StateQueryKind};
    use spool_shared_types::wire::{QueryPayload, Request, Response};
    use std::thread;

    #[test]
    fn permission_wait_rejects_control_and_receipts_quit_before_exit() {
        use crate::lifecycle::{Lifecycle, Phase};
        use spool_shared_types::wire::{AdmissionStatus, CheckedAction};
        let (events, receiver) = EventSender::new();
        let lifecycle = Lifecycle::starting();
        lifecycle.set(Phase::WaitingForPermission);
        let endpoint = service("permission-admission");
        let _guard = RequestReader::with_lifecycle(events, lifecycle.clone())
            .start_with_service(&endpoint)
            .unwrap();
        let result = Client::connect(&endpoint)
            .unwrap()
            .call(&Request::Command(CheckedAction {
                request_id: "control".into(),
                action: Action::FocusWindow { window_id: 1 },
            }))
            .unwrap();
        let Response::Admission(receipt) = result else {
            panic!("admission")
        };
        assert_eq!(receipt.status, AdmissionStatus::Rejected);
        assert_eq!(receipt.code.as_deref(), Some("not_ready"));
        assert!(receiver.try_recv().is_err());
        let query = Client::connect(&endpoint)
            .unwrap()
            .call(&Request::Query(StateQueryKind::State))
            .unwrap();
        assert_eq!(query, Response::Error("not_ready".into()));
        let watch = Client::connect(&endpoint)
            .unwrap()
            .subscribe(&Request::Subscribe { raw: false });
        assert!(
            matches!(watch, Err(spool_local_ipc::Error::Remote(message)) if message == "not_ready")
        );
        assert!(receiver.try_recv().is_err());
        let result = Client::connect(&endpoint)
            .unwrap()
            .call(&Request::Command(CheckedAction {
                request_id: "quit".into(),
                action: Action::Quit,
            }))
            .unwrap();
        let Response::Admission(receipt) = result else {
            panic!("admission")
        };
        assert_eq!(receipt.status, AdmissionStatus::Accepted);
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(Event::Exit)
        ));
        assert_eq!(lifecycle.phase(), Phase::Stopping);
    }

    fn service(test: &str) -> String {
        format!(
            "com.wxxxcxx.spool.reader-test.{test}.{}",
            std::process::id()
        )
    }

    /// Starts a reader on an endpoint whose previous owner has just exited.
    ///
    /// Dropping the first daemon releases its `flock`, but an immediate
    /// re-acquire on the same path can still fail while the rest of the suite
    /// runs on parallel threads. Retrying against a deadline keeps the
    /// assertion honest without serializing the whole suite: the test still
    /// fails if the lock never comes back.
    fn start_after_release(service: &str) -> RequestReaderGuard {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (events, _receiver) = EventSender::new();
            match RequestReader::new(events).start_with_service(service) {
                Ok(guard) => return guard,
                Err(error) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the dropped daemon's lock was never released: {error}"
                    );
                    thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    fn a_shell_launch_publishes_the_same_ipc_as_a_service_launch() {
        let (events, _receiver) = EventSender::new();
        let _guard = RequestReader::new(events)
            .start_with_service(&service("foreground"))
            .expect("foreground IPC listener");
    }

    #[test]
    fn a_second_daemon_is_refused_until_the_first_exits() {
        let service = service("singleton");
        let (first_events, _first_receiver) = EventSender::new();
        let first = RequestReader::new(first_events)
            .start_with_service(&service)
            .expect("first daemon");

        let (second_events, _second_receiver) = EventSender::new();
        assert!(
            RequestReader::new(second_events)
                .start_with_service(&service)
                .is_err()
        );

        drop(first);
        let _third = start_after_release(&service);
    }

    #[test]
    fn an_action_is_acknowledged_after_entering_the_event_queue() {
        let service = service("action-round-trip");
        let (events, receiver) = EventSender::new();
        let _guard = RequestReader::new(events)
            .start_with_service(&service)
            .expect("daemon");
        let client_service = service.clone();
        let client = thread::spawn(move || {
            Client::connect(&client_service)
                .expect("connect")
                .send(&Request::Dispatch(Action::Quit))
        });

        assert!(matches!(
            receiver.recv().expect("queued event"),
            Event::ActionRequested {
                action: Action::Quit
            }
        ));
        client.join().expect("client thread").expect("acknowledged");
    }

    #[test]
    fn a_query_round_trips_through_the_ecs_reply_channel() {
        let service = service("query-round-trip");
        let (events, receiver) = EventSender::new();
        let _guard = RequestReader::new(events)
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

    #[test]
    fn a_raw_subscription_reaches_the_world_with_its_mode_intact() {
        let service = service("raw-subscription");
        let (events, receiver) = EventSender::new();
        let _guard = RequestReader::new(events)
            .start_with_service(&service)
            .expect("daemon");
        let client_service = service.clone();
        let client = thread::spawn(move || {
            let mut stream = Client::connect(&client_service)
                .expect("connect")
                .subscribe(&Request::Subscribe { raw: true })
                .expect("subscribe");
            stream.recv_blocking().expect("event")
        });

        let Event::StateSubscribe { subscriber, raw } =
            receiver.recv().expect("subscription event")
        else {
            panic!("expected subscription event");
        };
        assert!(raw);
        subscriber
            .try_send(&StateEvent::RawEvent {
                name: "window_moved".to_string(),
                display_id: None,
                space_id: None,
                window_id: Some(7),
                details: "incarnation=2".to_string(),
            })
            .expect("push raw event");

        assert_eq!(
            client.join().expect("client thread"),
            StateEvent::RawEvent {
                name: "window_moved".to_string(),
                display_id: None,
                space_id: None,
                window_id: Some(7),
                details: "incarnation=2".to_string(),
            }
        );
    }
}

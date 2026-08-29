//! Where requests from other processes come in.
//!
//! Spool publishes a Mach service under a well-known name; the CLI, the
//! loadable Lua module and anything else that wants to drive the window manager
//! connect to that name and send it a [`Request`]. Each one is turned into an
//! [`Event`] for the world, and the ones that expect an answer carry a reply
//! channel the answering system fills in.

use bevy::tasks::{IoTaskPool, TaskPool};
use futures_lite::StreamExt;
use spool_mach_ipc::{Delivery, Receiver, Reply as MachReply};
use spool_shared_types::wire::{Request, service_name};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use tracing::{error, warn};

use crate::errors::{Error, Result};
use crate::events::{Event, EventSender, Reply};

/// `CommandReader` owns the service port and feeds what arrives on it into the
/// world.
pub struct CommandReader {
    events: EventSender,
}

/// Keeps the daemon's process-level singleton lock alive for the main loop.
pub struct CommandReaderGuard {
    _instance_lock: InstanceLock,
}

struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn acquire(service: &str) -> Result<Self> {
        Self::acquire_path(&lock_path(service))
    }

    fn acquire_path(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Self { _file: file });
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Err(Error::Generic(
                "another Spool instance is already running".to_string(),
            ))
        } else {
            Err(error.into())
        }
    }
}

fn lock_path(service: &str) -> PathBuf {
    let filename = service
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    std::env::temp_dir().join(format!("{filename}.lock"))
}

impl CommandReader {
    /// Creates a new `CommandReader`.
    ///
    /// # Arguments
    ///
    /// * `events` - An `EventSender` to dispatch received requests on.
    #[must_use]
    pub fn new(events: EventSender) -> Self {
        CommandReader { events }
    }

    /// Claims the service name and starts serving it on a thread of its own.
    ///
    /// # Errors
    ///
    /// Returns an error if another Spool daemon already owns the name.
    pub fn start(self) -> Result<CommandReaderGuard> {
        self.start_with_service(&service_name())
    }

    fn start_with_service(self, service: &str) -> Result<CommandReaderGuard> {
        let instance_lock = InstanceLock::acquire(service)?;
        let receiver = match Receiver::<Request>::bind(service) {
            Ok(receiver) => Some(receiver),
            Err(spool_mach_ipc::Error::NotPrivileged) => {
                warn!(
                    service,
                    "Mach IPC is unavailable for a shell-launched Spool; continuing without CLI access"
                );
                None
            }
            Err(error) => {
                error!(%error, service, "can not register the Spool Mach service");
                return Err(error.into());
            }
        };

        if let Some(receiver) = receiver {
            thread::spawn(move || {
                // Parks this one thread for the process lifetime; each request
                // is handed to the IO pool rather than served inline.
                futures_lite::future::block_on(async move {
                    let mut requests = std::pin::pin!(receiver);
                    while let Some(delivery) = requests.next().await {
                        match delivery {
                            Ok(delivery) => self.dispatch(delivery),
                            // A request that fails to decode is a bad client,
                            // not a reason to stop serving.
                            Err(err) => warn!("reading request: {err}"),
                        }
                    }
                });
            });
        }

        Ok(CommandReaderGuard {
            _instance_lock: instance_lock,
        })
    }

    /// Turns one request into an event, and arranges for its answer.
    fn dispatch(&self, delivery: Delivery<Request>) {
        let events = self.events.clone();
        let Delivery {
            value,
            reply,
            subscriber,
        } = delivery;

        match value {
            // Fire-and-forget: no reply is sent for this request.
            Request::Command(command) => {
                send(&events, Event::Command { command });
            }
            Request::WindowSetApply(ops) => {
                send(
                    &events,
                    Event::Command {
                        command: crate::commands::Command::Layout(ops),
                    },
                );
            }

            Request::Query(kind) => {
                answer(events, reply, "state query", move |respond_to| {
                    Event::StateQuery { kind, respond_to }
                });
            }
            Request::WindowSet => {
                answer(events, reply, "window set query", |respond_to| {
                    Event::WindowSetQuery { respond_to }
                });
            }
            Request::ScriptState(request) => {
                answer(events, reply, "script state request", move |respond_to| {
                    Event::ScriptState {
                        request,
                        respond_to,
                    }
                });
            }

            Request::Subscribe => {
                if let Some(subscriber) = subscriber {
                    send(
                        &events,
                        Event::StateSubscribe {
                            subscriber: Arc::new(subscriber),
                        },
                    );
                } else {
                    // No channel to push events to; the sender built the
                    // request by hand and got it wrong.
                    warn!("subscribe request carried no event channel");
                }
            }
        }
    }
}

/// Sends an event that expects no answer.
fn send(events: &EventSender, event: Event) {
    _ = events
        .send(event)
        .inspect_err(|err| error!("sending event: {err}"));
}

/// Sends a request-carrying event to the world and answers the client with
/// whatever comes back. `what` only names the request in the log.
///
/// The wait runs on the IO pool, not the receive loop, so a slow client blocks
/// only itself. There is no timeout: the reply sender travels inside the
/// event, so if the world never answers, the event is dropped, the channel
/// closes, and `recv` resolves anyway.
fn answer(
    events: EventSender,
    reply: Option<MachReply>,
    what: &'static str,
    request: impl FnOnce(Reply) -> Event + Send + 'static,
) {
    let Some(reply) = reply else {
        warn!("{what} arrived without a reply channel");
        return;
    };

    let (tx, rx) = async_channel::bounded(1);
    if events
        .send(request(tx))
        .inspect_err(|err| error!("sending {what}: {err}"))
        .is_err()
    {
        return;
    }

    // `get_or_init`, not `get`: the reader starts before the Bevy app is built,
    // so an early client would otherwise find no pool at all and panic.
    IoTaskPool::get_or_init(TaskPool::default)
        .spawn(async move {
            match rx.recv().await {
                Ok(response) => {
                    if let Err(err) = reply.send(&response) {
                        // A client that stopped waiting is normal — an
                        // interrupted `spool query` does exactly this.
                        warn!("answering {what}: {err}");
                    }
                }
                Err(err) => error!("waiting for {what} response: {err}"),
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::{CommandReader, InstanceLock};
    use crate::events::EventSender;

    #[test]
    fn shell_launch_continues_when_launchd_check_in_is_not_permitted() {
        let service = format!("com.wxxxcxx.spool.reader-test.{}", std::process::id());
        let (events, _receiver) = EventSender::new();

        let _guard = CommandReader::new(events)
            .start_with_service(&service)
            .expect("a shell launch should continue without Mach IPC");
    }

    #[test]
    fn instance_lock_refuses_a_second_daemon_and_releases_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "com.wxxxcxx.spool.lock-test-{}",
            std::process::id()
        ));
        let first = InstanceLock::acquire_path(&path).expect("first daemon lock");

        assert!(InstanceLock::acquire_path(&path).is_err());

        drop(first);
        let second = InstanceLock::acquire_path(&path).expect("released daemon lock");
        drop(second);
        std::fs::remove_file(path).expect("remove test lock file");
    }
}

//! Spool's single local IPC module.
//!
//! A Unix domain socket carries requests, replies, and subscriptions. A
//! separate advisory lock owns process singleton status: socket files can
//! survive a crash, while the kernel always releases `flock` when the owning
//! process exits. Callers do not need to coordinate either lifecycle.

use serde::{Deserialize, Serialize};
use spool_shared_types::state::StateEvent;
use spool_shared_types::wire::{Request, Response};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver as MessageReceiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: u16 = 2;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_DEADLINE: Duration = Duration::from_secs(2);
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(1);
const SUBSCRIBER_QUEUE_CAPACITY: usize = 64;

/// A result produced by this IPC module.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable error meanings callers can act on without knowing socket details.
#[derive(Debug)]
pub enum Error {
    /// No socket is published at the requested endpoint.
    NotRunning,
    /// Another process owns either the singleton lock or a live endpoint.
    AlreadyRunning,
    /// The process on the other end exited or closed the connection.
    PeerGone,
    /// A subscriber is alive but its bounded outgoing queue is full.
    WouldBlock,
    /// A bounded request did not complete before its deadline.
    TimedOut,
    /// The endpoint path exists but is unsafe to reuse or connect to.
    UnsafeEndpoint(String),
    /// The peer speaks an incompatible or malformed protocol.
    Protocol(String),
    /// The daemon explicitly rejected a request.
    Remote(String),
    /// A value could not be encoded for the wire.
    Encode,
    /// A frame could not be decoded into the expected value.
    Decode,
    /// A filesystem or socket operation failed.
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => write!(formatter, "spool is not running"),
            Self::AlreadyRunning => write!(formatter, "another Spool instance is already running"),
            Self::PeerGone => write!(formatter, "the peer has exited"),
            Self::WouldBlock => write!(formatter, "the subscriber is not keeping up"),
            Self::TimedOut => write!(formatter, "the IPC request timed out"),
            Self::UnsafeEndpoint(message) => write!(formatter, "unsafe IPC endpoint: {message}"),
            Self::Protocol(message) => write!(formatter, "IPC protocol error: {message}"),
            Self::Remote(message) => write!(formatter, "{message}"),
            Self::Encode => write!(formatter, "the value could not be encoded"),
            Self::Decode => write!(formatter, "the frame could not be decoded"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum RequestMode {
    Send,
    Call,
    Subscribe,
}

#[derive(Debug, Deserialize, Serialize)]
struct ClientFrame {
    version: u16,
    mode: RequestMode,
    request: Request,
}

#[derive(Debug, Deserialize, Serialize)]
enum ServerFrame {
    Ack,
    Response(Response),
    Event(StateEvent),
    Error(String),
}

/// One decoded request plus the one response mechanism selected by its client.
#[derive(Debug)]
pub struct Delivery {
    pub request: Request,
    pub acknowledgement: Option<Acknowledgement>,
    pub reply: Option<Reply>,
    pub subscriber: Option<Subscriber>,
}

/// Completes a fire-and-forget request after it has reached the daemon queue.
#[derive(Debug)]
pub struct Acknowledgement {
    stream: UnixStream,
}

impl Acknowledgement {
    /// Tells the client the request was accepted by the daemon.
    ///
    /// # Errors
    ///
    /// Returns an error if the client disconnected or the bounded write timed
    /// out.
    pub fn accepted(mut self) -> Result<()> {
        write_frame(&mut self.stream, &ServerFrame::Ack)
    }

    /// Tells the client why the daemon could not accept the request.
    ///
    /// # Errors
    ///
    /// Returns an error if the client disconnected or the bounded write timed
    /// out.
    pub fn rejected(mut self, message: impl Into<String>) -> Result<()> {
        write_frame(&mut self.stream, &ServerFrame::Error(message.into()))
    }
}

/// Completes one query connection with exactly one response.
#[derive(Debug)]
pub struct Reply {
    stream: UnixStream,
}

impl Reply {
    /// Sends the query response and consumes this one-shot reply.
    ///
    /// # Errors
    ///
    /// Returns an error if encoding fails, the client disconnected, or the
    /// bounded write timed out.
    pub fn send(mut self, response: &Response) -> Result<()> {
        write_frame(&mut self.stream, &ServerFrame::Response(response.clone()))
    }
}

/// A non-blocking producer for one long-lived subscription connection.
#[derive(Debug)]
pub struct Subscriber {
    outgoing: SyncSender<Vec<u8>>,
}

impl Subscriber {
    fn new(mut stream: UnixStream) -> Result<Self> {
        write_frame(&mut stream, &ServerFrame::Ack)?;
        stream
            .set_write_timeout(Some(DEFAULT_DEADLINE))
            .map_err(Error::Io)?;

        let (outgoing, incoming) = mpsc::sync_channel::<Vec<u8>>(SUBSCRIBER_QUEUE_CAPACITY);
        thread::Builder::new()
            .name("spool-ipc-subscriber".to_string())
            .spawn(move || {
                while let Ok(frame) = incoming.recv() {
                    if stream.write_all(&frame).is_err() {
                        break;
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { outgoing })
    }

    /// Queues one event without ever blocking the window-manager thread.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WouldBlock`] when the subscriber is behind,
    /// [`Error::PeerGone`] after its writer exits, or [`Error::Encode`] if the
    /// event cannot be framed.
    pub fn try_send(&self, event: &StateEvent) -> Result<()> {
        let frame = encode_frame(&ServerFrame::Event(event.clone()))?;
        match self.outgoing.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Error::WouldBlock),
            Err(TrySendError::Disconnected(_)) => Err(Error::PeerGone),
        }
    }
}

/// The daemon side of the IPC seam.
///
/// Binding acquires singleton ownership before touching the socket path. The
/// listener and lock remain owned until this value is dropped.
pub struct Server {
    incoming: MessageReceiver<Result<Delivery>>,
    guard: ServerGuard,
}

impl Server {
    /// Claims one logical Spool instance name and starts accepting clients.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyRunning`] when the singleton is owned, or a
    /// path/socket error when the endpoint cannot be created safely.
    pub fn bind(name: &str) -> Result<Self> {
        let paths = EndpointPaths::new(name);
        paths.prepare_directory()?;
        validate_socket_path_length(&paths.socket)?;
        let instance_lock = InstanceLock::acquire(&paths.lock)?;
        let legacy_instance_lock = InstanceLock::acquire(&legacy_lock_path(name))?;
        prepare_socket_path(&paths.socket)?;

        let listener = UnixListener::bind(&paths.socket).map_err(map_bind_error)?;
        std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::Io)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let (sender, incoming) = mpsc::channel();
        let thread_shutdown = Arc::clone(&shutdown);
        let accept_thread = thread::Builder::new()
            .name("spool-ipc-listener".to_string())
            .spawn(move || accept_connections(&listener, &sender, &thread_shutdown))
            .map_err(Error::Io)?;

        Ok(Self {
            incoming,
            guard: ServerGuard {
                shutdown,
                socket_path: paths.socket,
                accept_thread: Some(accept_thread),
                _instance_lock: instance_lock,
                _legacy_instance_lock: legacy_instance_lock,
            },
        })
    }

    /// Waits for the next complete client handshake.
    ///
    /// # Errors
    ///
    /// Returns a per-connection protocol/authentication error, or
    /// [`Error::PeerGone`] after the server has shut down.
    pub fn recv_blocking(&self) -> Result<Delivery> {
        self.incoming.recv().unwrap_or(Err(Error::PeerGone))
    }

    /// Separates the blocking receive loop from the lifetime guard so the
    /// caller can run them on different threads.
    #[must_use]
    pub fn into_parts(self) -> (Incoming, ServerGuard) {
        (
            Incoming {
                incoming: self.incoming,
            },
            self.guard,
        )
    }

    /// The filesystem path clients connect to for this server.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.guard.socket_path
    }
}

/// Keeps singleton ownership and the listener alive.
pub struct ServerGuard {
    shutdown: Arc<AtomicBool>,
    socket_path: PathBuf,
    accept_thread: Option<JoinHandle<()>>,
    _instance_lock: InstanceLock,
    _legacy_instance_lock: InstanceLock,
}

fn legacy_lock_path(name: &str) -> PathBuf {
    let safe_name = sanitize_name(name);
    std::env::temp_dir().join(format!("{safe_name}.lock"))
}

/// Blocking delivery stream, intended to live on the daemon's IPC thread.
pub struct Incoming {
    incoming: MessageReceiver<Result<Delivery>>,
}

impl Incoming {
    /// Waits for the next complete client handshake.
    ///
    /// # Errors
    ///
    /// Returns a per-connection protocol/authentication error, or
    /// [`Error::PeerGone`] after the server has shut down.
    pub fn recv_blocking(&self) -> Result<Delivery> {
        self.incoming.recv().unwrap_or(Err(Error::PeerGone))
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        _ = UnixStream::connect(&self.socket_path);
        if let Some(thread) = self.accept_thread.take() {
            _ = thread.join();
        }
        _ = remove_owned_socket(&self.socket_path);
    }
}

fn accept_connections(
    listener: &UnixListener,
    sender: &mpsc::Sender<Result<Delivery>>,
    shutdown: &AtomicBool,
) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let sender = sender.clone();
                _ = thread::Builder::new()
                    .name("spool-ipc-handshake".to_string())
                    .spawn(move || {
                        let delivery = read_delivery(stream);
                        _ = sender.send(delivery);
                    });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                _ = sender.send(Err(Error::Io(error)));
                break;
            }
        }
    }
}

fn read_delivery(mut stream: UnixStream) -> Result<Delivery> {
    verify_peer(&stream)?;
    stream
        .set_read_timeout(Some(HANDSHAKE_DEADLINE))
        .map_err(Error::Io)?;
    stream
        .set_write_timeout(Some(DEFAULT_DEADLINE))
        .map_err(Error::Io)?;

    let frame: ClientFrame = read_frame(&mut stream)?;
    if frame.version != PROTOCOL_VERSION {
        let message = format!(
            "protocol version {} is unsupported; daemon expects {PROTOCOL_VERSION}",
            frame.version
        );
        _ = write_frame(&mut stream, &ServerFrame::Error(message.clone()));
        return Err(Error::Protocol(message));
    }

    Ok(match frame.mode {
        RequestMode::Send => Delivery {
            request: frame.request,
            acknowledgement: Some(Acknowledgement { stream }),
            reply: None,
            subscriber: None,
        },
        RequestMode::Call => Delivery {
            request: frame.request,
            acknowledgement: None,
            reply: Some(Reply { stream }),
            subscriber: None,
        },
        RequestMode::Subscribe => Delivery {
            request: frame.request,
            acknowledgement: None,
            reply: None,
            subscriber: Some(Subscriber::new(stream)?),
        },
    })
}

/// A single-exchange client connection.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connects to the unique daemon published under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] when no daemon is listening, or an
    /// authentication/path error if the endpoint is unsafe.
    pub fn connect(name: &str) -> Result<Self> {
        Self::connect_with_deadline(name, DEFAULT_DEADLINE)
    }

    fn connect_with_deadline(name: &str, deadline: Duration) -> Result<Self> {
        let paths = EndpointPaths::new(name);
        paths.prepare_directory()?;
        let path = paths.socket;
        validate_client_socket(&path)?;
        let stream = connect_with_timeout(&path, deadline)?;
        verify_peer(&stream)?;
        stream.set_read_timeout(Some(deadline)).map_err(Error::Io)?;
        stream
            .set_write_timeout(Some(deadline))
            .map_err(Error::Io)?;
        Ok(Self { stream })
    }

    /// Sends a command and waits until the daemon has accepted it.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be framed, transferred, or
    /// acknowledged within the deadline.
    pub fn send(mut self, request: &Request) -> Result<()> {
        self.write_request(RequestMode::Send, request)?;
        match read_frame::<ServerFrame>(&mut self.stream)? {
            ServerFrame::Ack => Ok(()),
            ServerFrame::Error(message) => Err(Error::Remote(message)),
            other => Err(unexpected_server_frame("acknowledgement", &other)),
        }
    }

    /// Sends a query and waits for its bounded response.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be transferred, the daemon
    /// rejects it, or the response deadline expires.
    pub fn call(mut self, request: &Request) -> Result<Response> {
        self.write_request(RequestMode::Call, request)?;
        match read_frame::<ServerFrame>(&mut self.stream)? {
            ServerFrame::Response(response) => Ok(response),
            ServerFrame::Error(message) => Err(Error::Remote(message)),
            other => Err(unexpected_server_frame("response", &other)),
        }
    }

    /// Converts this connection into a long-lived event stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the subscription handshake is rejected or does
    /// not complete within the deadline.
    pub fn subscribe(mut self, request: &Request) -> Result<EventStream> {
        self.write_request(RequestMode::Subscribe, request)?;
        match read_frame::<ServerFrame>(&mut self.stream)? {
            ServerFrame::Ack => {
                self.stream.set_read_timeout(None).map_err(Error::Io)?;
                Ok(EventStream {
                    stream: self.stream,
                })
            }
            ServerFrame::Error(message) => Err(Error::Remote(message)),
            other => Err(unexpected_server_frame(
                "subscription acknowledgement",
                &other,
            )),
        }
    }

    fn write_request(&mut self, mode: RequestMode, request: &Request) -> Result<()> {
        write_frame(
            &mut self.stream,
            &ClientFrame {
                version: PROTOCOL_VERSION,
                mode,
                request: request.clone(),
            },
        )
    }
}

/// The receiving half of a subscribed client connection.
#[derive(Debug)]
pub struct EventStream {
    stream: UnixStream,
}

impl EventStream {
    /// Blocks until the next event arrives or the daemon disconnects.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PeerGone`] when the daemon exits, or a protocol error
    /// when the incoming frame is not a state event.
    pub fn recv_blocking(&mut self) -> Result<StateEvent> {
        match read_frame::<ServerFrame>(&mut self.stream)? {
            ServerFrame::Event(event) => Ok(event),
            ServerFrame::Error(message) => Err(Error::Remote(message)),
            other => Err(unexpected_server_frame("state event", &other)),
        }
    }
}

fn unexpected_server_frame(expected: &str, frame: &ServerFrame) -> Error {
    Error::Protocol(format!("expected {expected}, received {frame:?}"))
}

#[derive(Debug)]
struct EndpointPaths {
    directory: PathBuf,
    socket: PathBuf,
    lock: PathBuf,
}

impl EndpointPaths {
    fn new(name: &str) -> Self {
        let safe_name = sanitize_name(name);
        // Do not derive singleton identity from TMPDIR: launchd and a terminal
        // can have different environments. The uid-scoped directory is stable
        // across both launch methods and is private before any file is used.
        let base = PathBuf::from("/tmp").join(format!("spool-{}", current_uid()));
        Self {
            directory: base.clone(),
            socket: base.join(format!("{safe_name}.sock")),
            lock: base.join(format!("{safe_name}.lock")),
        }
    }

    fn prepare_directory(&self) -> Result<()> {
        match std::fs::create_dir(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Error::Io(error)),
        }

        let metadata = std::fs::symlink_metadata(&self.directory).map_err(Error::Io)?;
        if !metadata.is_dir() || metadata.uid() != current_uid() {
            return Err(Error::UnsafeEndpoint(format!(
                "runtime directory `{}` is not a directory owned by the current user",
                self.directory.display()
            )));
        }
        std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
            .map_err(Error::Io)
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(Error::Io)?;
        let metadata = file.metadata().map_err(Error::Io)?;
        if !metadata.is_file() || metadata.uid() != current_uid() {
            return Err(Error::UnsafeEndpoint(format!(
                "lock `{}` is not a regular file owned by the current user",
                path.display()
            )));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(Error::Io)?;

        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status == 0 {
            Ok(Self { _file: file })
        } else {
            let error = io::Error::last_os_error();
            if error
                .raw_os_error()
                .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
            {
                Err(Error::AlreadyRunning)
            } else {
                Err(Error::Io(error))
            }
        }
    }
}

fn prepare_socket_path(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error)),
    };
    validate_owned_socket(path, &metadata)?;

    match UnixStream::connect(path) {
        Ok(_) => Err(Error::AlreadyRunning),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            std::fs::remove_file(path).map_err(Error::Io)
        }
        Err(error) => Err(Error::Io(error)),
    }
}

fn remove_owned_socket(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_owned_socket(path, &metadata)?;
            std::fs::remove_file(path).map_err(Error::Io)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

fn validate_client_socket(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_owned_socket(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(Error::NotRunning),
        Err(error) => Err(Error::Io(error)),
    }
}

fn validate_owned_socket(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_socket() || metadata.uid() != current_uid() {
        return Err(Error::UnsafeEndpoint(format!(
            "`{}` is not a Unix socket owned by the current user",
            path.display()
        )));
    }
    Ok(())
}

fn validate_socket_path_length(path: &Path) -> Result<()> {
    let sockaddr = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    let maximum = sockaddr.sun_path.len().saturating_sub(1);
    let actual = path.as_os_str().as_bytes().len();
    if actual > maximum {
        return Err(Error::UnsafeEndpoint(format!(
            "socket path is {actual} bytes, but macOS allows at most {maximum}: `{}`",
            path.display()
        )));
    }
    Ok(())
}

fn verify_peer(stream: &UnixStream) -> Result<()> {
    let mut uid = 0;
    let mut gid = 0;
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if status != 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    if uid != current_uid() {
        return Err(Error::UnsafeEndpoint(format!(
            "peer uid {uid} does not match current uid {}",
            current_uid()
        )));
    }
    Ok(())
}

fn connect_with_timeout(path: &Path, deadline: Duration) -> Result<UnixStream> {
    let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw_fd < 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    set_nonblocking(&fd, true)?;

    let bytes = path.as_os_str().as_bytes();
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX)
        .map_err(|_| Error::Protocol("AF_UNIX does not fit sa_family_t".to_string()))?;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        address.sun_len =
            u8::try_from(std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
                .map_err(|_| {
                    Error::Protocol("socket address length does not fit u8".to_string())
                })?;
    }
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = (*source).cast_signed();
    }
    let address_length = libc::socklen_t::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1,
    )
    .map_err(|_| Error::Protocol("socket address length does not fit socklen_t".to_string()))?;

    let status = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if status != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(map_connect_error(error));
        }
        wait_for_connection(&fd, deadline)?;
    }

    set_nonblocking(&fd, false)?;
    Ok(UnixStream::from(fd))
}

fn set_nonblocking(fd: &OwnedFd, enabled: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    let flags = if enabled {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags) } < 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    Ok(())
}

fn wait_for_connection(fd: &OwnedFd, deadline: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Error::TimedOut);
        }
        let timeout = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let status = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
        if status == 0 {
            return Err(Error::TimedOut);
        }
        if status < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::Io(error));
        }

        let mut socket_error: libc::c_int = 0;
        let mut length = libc::socklen_t::try_from(std::mem::size_of_val(&socket_error))
            .expect("socket error size fits socklen_t");
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut socket_error).cast(),
                &raw mut length,
            )
        } < 0
        {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        if socket_error == 0 {
            return Ok(());
        }
        return Err(map_connect_error(io::Error::from_raw_os_error(
            socket_error,
        )));
    }
}

fn current_uid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let frame = encode_frame(value)?;
    writer.write_all(&frame).map_err(map_io_error)
}

fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let payload = postcard::to_allocvec(value).map_err(|_| Error::Encode)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "frame is {} bytes; maximum is {MAX_FRAME_BYTES}",
            payload.len()
        )));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| Error::Protocol("frame length does not fit u32".to_string()))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).map_err(map_io_error)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "incoming frame is {length} bytes; maximum is {MAX_FRAME_BYTES}"
        )));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).map_err(map_io_error)?;
    postcard::from_bytes(&payload).map_err(|_| Error::Decode)
}

fn map_bind_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::AddrInUse {
        Error::AlreadyRunning
    } else {
        Error::Io(error)
    }
}

fn map_connect_error(error: io::Error) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        Error::NotRunning
    } else {
        map_io_error(error)
    }
}

fn map_io_error(error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::UnexpectedEof => Error::PeerGone,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::TimedOut,
        _ => Error::Io(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spool_shared_types::state::StateQueryKind;
    use spool_shared_types::wire::QueryPayload;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_NAME: AtomicU64 = AtomicU64::new(1);

    fn name(test: &str) -> String {
        format!(
            "com.wxxxcxx.spool.local-ipc.{test}.{}.{}",
            std::process::id(),
            NEXT_NAME.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn a_call_gets_one_response() {
        let name = name("call");
        let server = Server::bind(&name).expect("bind");
        let client_name = name.clone();
        let client = thread::spawn(move || {
            Client::connect(&client_name)
                .expect("connect")
                .call(&Request::Query(StateQueryKind::Active))
                .expect("response")
        });

        let delivery = server.recv_blocking().expect("delivery");
        assert_eq!(delivery.request, Request::Query(StateQueryKind::Active));
        delivery
            .reply
            .expect("reply")
            .send(&Response::Query(QueryPayload::Active(Box::default())))
            .expect("send response");

        assert!(matches!(
            client.join().expect("client thread"),
            Response::Query(QueryPayload::Active(_))
        ));
    }

    #[test]
    fn a_send_waits_for_daemon_acceptance() {
        let name = name("send");
        let server = Server::bind(&name).expect("bind");
        let client_name = name.clone();
        let client = thread::spawn(move || {
            Client::connect(&client_name)
                .expect("connect")
                .send(&Request::Dispatch(
                    spool_shared_types::commands::Action::Quit,
                ))
        });

        let delivery = server.recv_blocking().expect("delivery");
        delivery
            .acknowledgement
            .expect("acknowledgement")
            .accepted()
            .expect("acknowledge");
        client
            .join()
            .expect("client thread")
            .expect("client accepted");
    }

    #[test]
    fn a_subscriber_receives_events() {
        let name = name("subscribe");
        let server = Server::bind(&name).expect("bind");
        let client_name = name.clone();
        let client = thread::spawn(move || {
            let mut events = Client::connect(&client_name)
                .expect("connect")
                .subscribe(&Request::Subscribe { raw: false })
                .expect("subscribe");
            events.recv_blocking().expect("event")
        });

        let delivery = server.recv_blocking().expect("delivery");
        delivery
            .subscriber
            .expect("subscriber")
            .try_send(&StateEvent::DisplayChanged {
                display_id: Some(7),
            })
            .expect("push event");

        assert_eq!(
            client.join().expect("client thread"),
            StateEvent::DisplayChanged {
                display_id: Some(7)
            }
        );
    }

    #[test]
    fn only_one_server_can_own_an_instance() {
        let name = name("singleton");
        let first = Server::bind(&name).expect("first server");
        assert!(matches!(Server::bind(&name), Err(Error::AlreadyRunning)));
        drop(first);
        let second = Server::bind(&name).expect("lock released");
        drop(second);
    }

    #[test]
    fn concurrent_startup_has_exactly_one_winner() {
        const CONTENDERS: usize = 20;

        let name = name("concurrent-singleton");
        let start = Arc::new(Barrier::new(CONTENDERS + 1));
        let release = Arc::new(Barrier::new(CONTENDERS + 1));
        let (outcomes, received) = mpsc::channel();
        let mut threads = Vec::new();

        for _ in 0..CONTENDERS {
            let name = name.clone();
            let start = Arc::clone(&start);
            let release = Arc::clone(&release);
            let outcomes = outcomes.clone();
            threads.push(thread::spawn(move || {
                start.wait();
                let server = Server::bind(&name);
                outcomes.send(server.is_ok()).expect("report outcome");
                release.wait();
                drop(server);
            }));
        }
        drop(outcomes);

        start.wait();
        let winners = (0..CONTENDERS)
            .map(|_| received.recv().expect("contender outcome"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        release.wait();
        for thread in threads {
            thread.join().expect("contender thread");
        }
    }

    #[test]
    fn a_live_socket_is_not_unlinked_even_without_the_advisory_lock() {
        let name = name("live-socket");
        let paths = EndpointPaths::new(&name);
        paths.prepare_directory().expect("runtime directory");
        let live = UnixListener::bind(&paths.socket).expect("live socket");

        assert!(matches!(Server::bind(&name), Err(Error::AlreadyRunning)));
        assert!(paths.socket.exists());

        drop(live);
        std::fs::remove_file(paths.socket).expect("remove fixture");
    }

    #[test]
    fn a_stale_socket_is_recovered_while_holding_the_lock() {
        let name = name("stale");
        let paths = EndpointPaths::new(&name);
        paths.prepare_directory().expect("runtime directory");
        let path = paths.socket;
        let stale = UnixListener::bind(&path).expect("stale socket");
        drop(stale);

        let server = Server::bind(&name).expect("recover stale socket");
        assert_eq!(server.socket_path(), path);
    }

    #[test]
    fn a_regular_file_at_the_socket_path_is_never_removed() {
        let name = name("regular-file");
        let paths = EndpointPaths::new(&name);
        paths.prepare_directory().expect("runtime directory");
        File::create(&paths.socket).expect("regular file");

        assert!(matches!(Server::bind(&name), Err(Error::UnsafeEndpoint(_))));
        assert!(paths.socket.is_file());
        std::fs::remove_file(paths.socket).expect("remove fixture");
    }

    #[test]
    fn a_symlink_at_the_socket_path_is_never_followed_or_removed() {
        let name = name("symlink");
        let paths = EndpointPaths::new(&name);
        paths.prepare_directory().expect("runtime directory");
        let target = paths
            .directory
            .join(format!("target-{}", std::process::id()));
        File::create(&target).expect("target file");
        std::os::unix::fs::symlink(&target, &paths.socket).expect("socket symlink");

        assert!(matches!(Server::bind(&name), Err(Error::UnsafeEndpoint(_))));
        assert!(paths.socket.is_symlink());
        assert!(target.is_file());

        std::fs::remove_file(paths.socket).expect("remove symlink");
        std::fs::remove_file(target).expect("remove target");
    }

    #[test]
    fn a_query_has_a_bounded_response_deadline() {
        let name = name("timeout");
        let paths = EndpointPaths::new(&name);
        paths.prepare_directory().expect("runtime directory");
        let listener = UnixListener::bind(&paths.socket).expect("raw listener");
        std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))
            .expect("socket permissions");
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _: ClientFrame = read_frame(&mut stream).expect("request");
            thread::sleep(Duration::from_millis(150));
        });

        let result = Client::connect_with_deadline(&name, Duration::from_millis(30))
            .expect("connect")
            .call(&Request::Query(StateQueryKind::State));
        assert!(matches!(result, Err(Error::TimedOut)));

        peer.join().expect("peer thread");
        std::fs::remove_file(paths.socket).expect("remove socket");
    }

    #[test]
    fn an_incompatible_protocol_version_fails_fast() {
        let name = name("version");
        let server = Server::bind(&name).expect("bind");
        let path = EndpointPaths::new(&name).socket;
        let mut stream = UnixStream::connect(path).expect("connect");
        stream
            .set_read_timeout(Some(DEFAULT_DEADLINE))
            .expect("deadline");
        write_frame(
            &mut stream,
            &ClientFrame {
                version: PROTOCOL_VERSION + 1,
                mode: RequestMode::Call,
                request: Request::Query(StateQueryKind::State),
            },
        )
        .expect("request");

        assert!(matches!(
            read_frame::<ServerFrame>(&mut stream).expect("error frame"),
            ServerFrame::Error(message) if message.contains("unsupported")
        ));
        assert!(matches!(
            server.recv_blocking(),
            Err(Error::Protocol(message)) if message.contains("unsupported")
        ));
    }
}

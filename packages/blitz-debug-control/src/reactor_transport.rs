//! Debug-only, loopback WebDriver transport for Blitz renderers.
//!
//! The server owns networking and session authentication. Renderer commands
//! cross a typed Arc queue and must be executed by the UI/runtime thread.
//!
//! # Security boundary
//!
//! Enabling this server grants the holder of its per-process discovery token
//! debugger-level control, including arbitrary JavaScript execution in the
//! document. It deliberately has the same trust posture as a browser remote
//! debugging port: loopback-only transport, an unpredictable token, and a
//! descriptor created with owner-only permissions on Unix. It is not a sandbox
//! or an authorization boundary against another process running as the same
//! OS user and able to read that user's files. Production builds should leave
//! the feature and its environment variables disabled.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Poll, Waker};
use std::time::Duration;

use futures_util::{Stream, stream::FuturesUnordered};
use nagoya::reactor::{Addr, Reactor, TcpListener, TcpStream};
use serde::Serialize;
use serde_json::{Value, json};

mod handoff;
pub use handoff::CommandReceiver;
use handoff::{CommandQueue, QueueError, Reply, ResponseWait};

const PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING_COMMANDS: usize = 64;
const MAX_CONNECTIONS: usize = 64;

#[derive(Default)]
struct ActiveSession(AtomicU64);

impl ActiveSession {
    fn create(&self) -> io::Result<Option<String>> {
        let value = loop {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes).map_err(io::Error::other)?;
            let value = u64::from_ne_bytes(bytes);
            if value != 0 {
                break value;
            }
        };
        match self
            .0
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(Some(format!("{value:016x}"))),
            Err(_) => Ok(None),
        }
    }

    fn matches(&self, supplied: &str) -> bool {
        u64::from_str_radix(supplied, 16)
            .ok()
            .is_some_and(|value| value != 0 && self.0.load(Ordering::Acquire) == value)
    }

    fn clear(&self) {
        self.0.store(0, Ordering::Release);
    }
}

/// Configuration for a loopback debug-control server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Loopback address to bind. Port zero asks the OS to choose a free port.
    pub bind_address: SocketAddr,
    /// Atomically written once the server is accepting connections.
    pub descriptor_path: PathBuf,
    /// Git revision or build identifier for the renderer.
    pub renderer_revision: String,
}

/// Wakes whichever thread services requests, so it does not have to poll for
/// them.
///
/// The server cannot know how to wake its embedder, and the embedder's event
/// loop does not exist yet when the server starts, so the callback is installed
/// later. A `OnceLock` rather than a lock because it is written exactly once
/// and read on the path of every request.
///
/// Without one installed the embedder must poll, which is what the Blitz shell
/// used to do on a 10ms timer: 100 wakeups a second while idle, up to 10ms of
/// latency on every command, and enough of both to show up in any measurement
/// taken with the driver attached.
#[derive(Clone, Default)]
pub struct ServiceWaker(Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>);

impl ServiceWaker {
    /// Install the wake callback. Later calls are ignored.
    pub fn set(&self, wake: impl Fn() + Send + Sync + 'static) {
        if self.0.set(Box::new(wake)).is_ok() {
            // A request may have arrived before the embedder installed us.
            self.wake();
        }
    }

    fn wake(&self) {
        if let Some(wake) = self.0.get() {
            wake();
        }
    }
}

impl std::fmt::Debug for ServiceWaker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceWaker")
            .field("installed", &self.0.get().is_some())
            .finish()
    }
}

/// A command forwarded from the HTTP server to the renderer thread.
#[derive(Debug)]
pub struct ControlRequest {
    pub method: String,
    pub path: String,
    pub body: Value,
    reply: Arc<Reply>,
}

impl ControlRequest {
    /// Hand the finished pointer to the waiting transport task.
    pub fn respond_arc(&self, response: Arc<ControlResponse>) -> Result<(), Arc<ControlResponse>> {
        self.reply.complete(response)
    }

    /// Complete this request. Failure means the client already disconnected.
    pub fn respond(self, response: ControlResponse) -> Result<(), ControlResponse> {
        self.respond_arc(Arc::new(response)).map_err(|response| {
            Arc::try_unwrap(response).expect("failed response is uniquely owned")
        })
    }
}

/// A renderer response represented using W3C WebDriver success/error values.
#[derive(Debug)]
pub enum ControlResponse {
    Success(Value),
    Error {
        error: String,
        message: String,
        stacktrace: String,
    },
}

impl ControlResponse {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Error {
            error: "unsupported operation".into(),
            message: message.into(),
            stacktrace: String::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor<'a> {
    pid: u32,
    address: String,
    token: &'a str,
    protocol_version: u32,
    renderer: &'static str,
    renderer_revision: &'a str,
}

/// Running server. Dropping it shuts down the listener and removes discovery.
pub struct DebugServer {
    address: SocketAddr,
    token: String,
    descriptor_path: PathBuf,
    waker: ServiceWaker,
    state: Option<Arc<ServerState>>,
    task: Option<nagoya::JoinHandle<()>>,
    reactor: Option<Reactor>,
}

enum CommandSink {
    Typed(Arc<CommandQueue>),
    // Keep the published Receiver<ControlRequest> API for existing consumers.
    Compatible(SyncSender<ControlRequest>),
}

impl CommandSink {
    fn enqueue(&self, request: Arc<ControlRequest>) -> Result<(), &'static str> {
        match self {
            Self::Typed(queue) => queue.enqueue(request).map_err(|error| match error {
                QueueError::Full => "renderer command queue is full",
                QueueError::Closed => "renderer command channel is closed",
            }),
            Self::Compatible(sender) => {
                // The legacy adapter takes the uniquely owned value without
                // copying any request data. It creates no response channel.
                let request = Arc::try_unwrap(request).expect("unqueued request has one owner");
                sender.try_send(request).map_err(|error| match error {
                    TrySendError::Full(_) => "renderer command queue is full",
                    TrySendError::Disconnected(_) => "renderer command channel is closed",
                })
            }
        }
    }

    fn close(&self) {
        if let Self::Typed(queue) = self {
            queue.close();
        }
    }
}

struct ServerState {
    token: Arc<String>,
    active_session: ActiveSession,
    commands: CommandSink,
    waker: ServiceWaker,
    shutdown: AtomicBool,
    stop_waker: Mutex<Option<Waker>>,
}

impl DebugServer {
    /// Bind a loopback port and retain the published value-receiver adapter.
    pub fn start(config: ServerConfig) -> io::Result<(Self, Receiver<ControlRequest>)> {
        let (command_tx, command_rx) = mpsc::sync_channel(MAX_PENDING_COMMANDS);
        let server = Self::start_with_sink(config, CommandSink::Compatible(command_tx))?;
        Ok((server, command_rx))
    }

    /// Bind a loopback port and hand typed request pointers to the renderer.
    pub fn start_typed(config: ServerConfig) -> io::Result<(Self, CommandReceiver)> {
        let queue = CommandQueue::new(MAX_PENDING_COMMANDS);
        let server = Self::start_with_sink(config, CommandSink::Typed(Arc::clone(&queue)))?;
        Ok((server, CommandReceiver(queue)))
    }

    fn start_with_sink(config: ServerConfig, commands: CommandSink) -> io::Result<Self> {
        if !config.bind_address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "debug control must bind to a loopback address",
            ));
        }
        let reactor = Reactor::start().map_err(reactor_error)?;
        let listener = TcpListener::bind(reactor_addr(config.bind_address), &reactor.handle())
            .map_err(reactor_error)?;
        let address = socket_addr(listener.local_addr().map_err(reactor_error)?)?;
        let token = random_hex(32)?;
        let waker = ServiceWaker::default();
        let state = Arc::new(ServerState {
            token: Arc::new(token.clone()),
            active_session: ActiveSession::default(),
            commands,
            waker: waker.clone(),
            shutdown: AtomicBool::new(false),
            stop_waker: Mutex::new(None),
        });
        write_descriptor(&config, address, &token)?;
        let task = nagoya::runtime::background().spawn(server_loop(listener, Arc::clone(&state)));
        Ok(Self {
            address,
            token,
            descriptor_path: config.descriptor_path,
            waker,
            state: Some(state),
            task: Some(task),
            reactor: Some(reactor),
        })
    }

    /// Handle for waking the renderer after a request has been queued.
    pub fn waker(&self) -> ServiceWaker {
        self.waker.clone()
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(state) = self.state.take() {
            state.commands.close();
            state.shutdown.store(true, Ordering::Release);
            let wake = state.stop_waker.lock().unwrap().take();
            if let Some(wake) = wake {
                wake.wake();
            }
        }
        if let Some(task) = self.task.take() {
            let _ = nagoya::block_on(task);
        }
        if let Some(reactor) = self.reactor.take() {
            reactor.shutdown();
        }
        let _ = fs::remove_file(&self.descriptor_path);
    }
}

impl Drop for DebugServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reactor_error(error: nagoya::reactor::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.0)
}

fn reactor_addr(address: SocketAddr) -> Addr {
    match address {
        SocketAddr::V4(address) => Addr::V4(address.ip().octets(), address.port()),
        SocketAddr::V6(address) => {
            Addr::V6Scoped(address.ip().octets(), address.port(), address.scope_id())
        }
    }
}

fn socket_addr(address: Addr) -> io::Result<SocketAddr> {
    match address {
        Addr::V4(ip, port) => Ok((std::net::Ipv4Addr::from(ip), port).into()),
        Addr::V6(ip, port) => Ok((std::net::Ipv6Addr::from(ip), port).into()),
        Addr::V6Scoped(ip, port, scope) => {
            Ok(std::net::SocketAddrV6::new(std::net::Ipv6Addr::from(ip), port, 0, scope).into())
        }
        Addr::Path(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a TCP address",
        )),
    }
}

fn random_hex(byte_len: usize) -> io::Result<String> {
    let mut bytes = vec![0; byte_len];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut output = String::with_capacity(byte_len * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").unwrap();
    }
    Ok(output)
}

fn write_descriptor(config: &ServerConfig, address: SocketAddr, token: &str) -> io::Result<()> {
    let descriptor = Descriptor {
        pid: std::process::id(),
        address: address.to_string(),
        token,
        protocol_version: PROTOCOL_VERSION,
        renderer: "blitz",
        renderer_revision: &config.renderer_revision,
    };
    let bytes = serde_json::to_vec_pretty(&descriptor).map_err(io::Error::other)?;
    if let Some(parent) = config.descriptor_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = config
        .descriptor_path
        .with_extension(format!("tmp-{}", random_hex(8)?));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &config.descriptor_path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

async fn server_loop(listener: TcpListener, state: Arc<ServerState>) {
    let mut connections = FuturesUnordered::new();
    std::future::poll_fn(|cx| {
        {
            let mut wake = state.stop_waker.lock().unwrap();
            if state.shutdown.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            *wake = Some(cx.waker().clone());
        }
        while let Poll::Ready(Some(())) = Pin::new(&mut connections).poll_next(cx) {}
        // One extra slot reports the connection limit. Until it finishes,
        // accept is backpressured, keeping all connection work bounded.
        if connections.len() <= MAX_CONNECTIONS {
            match listener.poll_accept(cx) {
                Poll::Ready(Ok((stream, _))) => {
                    let limited = connections.len() == MAX_CONNECTIONS;
                    connections.push(serve_connection(stream, Arc::clone(&state), limited));
                    cx.waker().wake_by_ref();
                }
                Poll::Ready(Err(_)) => {
                    state.commands.close();
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }
        Poll::Pending
    })
    .await;
    // Dropping these futures releases every socket and response waiter.
    drop(connections);
}

async fn serve_connection(mut stream: TcpStream, state: Arc<ServerState>, limited: bool) {
    let response = if limited {
        webdriver_error("unknown error", "debug-control connection limit reached")
    } else {
        match read_request(&mut stream).await {
            Ok(request) => route(request, &state).await,
            Err(error) => webdriver_error("invalid argument", error.to_string()),
        }
    };
    let _ = nagoya::time::timeout(COMMAND_TIMEOUT, write_response(&mut stream, response)).await;
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    body: Value,
}

async fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    read_request_within(stream, COMMAND_TIMEOUT).await
}

async fn read_request_within(stream: &mut TcpStream, within: Duration) -> io::Result<HttpRequest> {
    nagoya::time::timeout(within, read_request_inner(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request timed out"))?
}

async fn read_request_inner(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    // Read directly into uninitialised receive storage, with a fixed bound.
    let mut bytes = bytes::BytesMut::with_capacity(MAX_REQUEST_BYTES);
    let mut scan_from = 0;
    let header_end = loop {
        if bytes.len() >= MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request is too large",
            ));
        }
        let prior_len = bytes.len();
        let count = stream.read_buf(&mut bytes).await.map_err(reactor_error)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        scan_from = scan_from.min(prior_len.saturating_sub(3));
        if let Some(index) = bytes[scan_from..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            break scan_from + index + 4;
        }
        scan_from = bytes.len().saturating_sub(3);
    };

    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = headers.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    if method.is_empty() || path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }
    let header_fields: Vec<_> = lines.filter_map(|line| line.split_once(':')).collect();
    let content_length = header_fields
        .iter()
        .copied()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .unwrap_or(0);
    if header_fields.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && !value.trim().eq_ignore_ascii_case("identity")
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transfer-encoding is unsupported; send Content-Length",
        ));
    }
    let body_end = header_end
        .checked_add(content_length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request size overflow"))?;
    if body_end > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request is too large",
        ));
    }
    while bytes.len() < body_end {
        let count = stream.read_buf(&mut bytes).await.map_err(reactor_error)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before body",
            ));
        }
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..body_end])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    Ok(HttpRequest { method, path, body })
}

async fn route(request: HttpRequest, state: &ServerState) -> Value {
    let token = state.token.as_str();
    let active_session = &state.active_session;
    if request.method == "GET" && request.path == "/status" {
        return json!({"value": {
            "ready": true,
            "message": "Blitz debug control is ready",
            "protocolVersion": PROTOCOL_VERSION,
        }});
    }

    if request.method == "POST" && request.path == "/session" {
        let supplied_token = request
            .body
            .pointer("/capabilities/alwaysMatch/blitz:token")
            .and_then(Value::as_str);
        if !token_matches(supplied_token, token) {
            return webdriver_error("invalid argument", "invalid blitz:token capability");
        }
        let session_id = match active_session.create() {
            Ok(Some(value)) => value,
            Ok(None) => {
                return webdriver_error("session not created", "only one session is supported");
            }
            Err(error) => return webdriver_error("unknown error", error.to_string()),
        };
        return json!({"value": {
            "sessionId": session_id,
            "capabilities": {
                "browserName": "blitz",
                "blitz:protocolVersion": PROTOCOL_VERSION,
            }
        }});
    }

    let Some((session_id, command_path)) = session_path(&request.path) else {
        return webdriver_error("unknown command", "unknown debug-control route");
    };
    if !active_session.matches(session_id) {
        return webdriver_error("invalid session id", "session is not active");
    }
    if request.method == "DELETE" && command_path.is_empty() {
        active_session.clear();
        return json!({"value": null});
    }

    let reply = Reply::new();
    let response = ResponseWait::new(&reply);
    let control_request = Arc::new(ControlRequest {
        method: request.method,
        path: command_path.to_string(),
        body: request.body,
        reply,
    });
    if let Err(message) = state.commands.enqueue(control_request) {
        return webdriver_error("unknown error", message);
    }
    // The renderer must be able to take the request as soon as it wakes.
    state.waker.wake();
    match nagoya::time::timeout(COMMAND_TIMEOUT, response).await {
        Ok(Ok(response)) => match response.as_ref() {
            ControlResponse::Success(value) => json!({"value": value}),
            ControlResponse::Error {
                error,
                message,
                stacktrace,
            } => json!({"value": {
                "error": error,
                "message": message,
                "stacktrace": stacktrace,
            }}),
        },
        Err(_) => webdriver_error("timeout", "renderer command timed out"),
        Ok(Err(())) => webdriver_error("unknown error", "renderer response channel is closed"),
    }
}

/// Compare an attacker-controlled capability with the fixed-size discovery
/// token without leaking how many prefix bytes matched.
fn token_matches(supplied: Option<&str>, expected: &str) -> bool {
    let supplied = supplied.unwrap_or_default().as_bytes();
    let expected = expected.as_bytes();
    let mut difference = supplied.len() ^ expected.len();
    for (index, expected_byte) in expected.iter().enumerate() {
        difference |= usize::from(*expected_byte ^ supplied.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

fn session_path(path: &str) -> Option<(&str, &str)> {
    let remainder = path.strip_prefix("/session/")?;
    let (session_id, command) = remainder.split_once('/').unwrap_or((remainder, ""));
    Some((session_id, command))
}

fn webdriver_error(error: &str, message: impl Into<String>) -> Value {
    json!({"value": {
        "error": error,
        "message": message.into(),
        "stacktrace": "",
    }})
}

async fn write_response(stream: &mut TcpStream, body: Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(&body).map_err(io::Error::other)?;
    let status = match body.pointer("/value/error").and_then(Value::as_str) {
        None => "200 OK",
        Some("unknown command" | "invalid session id") => "404 Not Found",
        Some("timeout" | "unknown error") => "500 Internal Server Error",
        Some(_) => "400 Bad Request",
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream
        .write_all_vectored(header.as_bytes(), &bytes)
        .await
        .map_err(reactor_error)
}

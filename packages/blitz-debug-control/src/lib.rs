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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use futures_util::{Stream, stream::FuturesUnordered};
use nagoya::reactor::{Addr, Reactor, TcpListener, TcpStream};
use serde::Serialize;
use serde_json::{Value, json};

mod handoff;
use handoff::{CommandQueue, QueueError, Reply, ResponseWait};
pub use handoff::{
    CommandReceiver, IntoValueIter, RecvError, RecvTimeoutError, TryRecvError, TryValueIter,
    ValueIter, ValueReceiver,
};

const PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_SESSION_REQUEST_BYTES: usize = 16 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_TIMEOUT: Duration = Duration::from_secs(1);
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_PENDING_COMMANDS: usize = 64;
const MAX_CONNECTIONS: usize = 64;

struct Session {
    id: String,
    last_used: Instant,
}

#[derive(Default)]
struct ActiveSession(Mutex<Option<Session>>);

impl ActiveSession {
    fn create(&self) -> io::Result<Option<String>> {
        let mut active = self.0.lock().unwrap();
        let now = Instant::now();
        let expired = active
            .as_ref()
            .is_some_and(|session| now.duration_since(session.last_used) >= SESSION_IDLE_TIMEOUT);
        if expired {
            *active = None;
        }
        if active.is_some() {
            return Ok(None);
        }
        let id = random_hex(32)?;
        let session = Session {
            id,
            last_used: Instant::now(),
        };
        let id = session.id.clone();
        *active = Some(session);
        Ok(Some(id))
    }

    fn matches(&self, supplied: &str) -> bool {
        let mut active = self.0.lock().unwrap();
        let now = Instant::now();
        let expired = active
            .as_ref()
            .is_some_and(|session| now.duration_since(session.last_used) >= SESSION_IDLE_TIMEOUT);
        if expired {
            *active = None;
            return false;
        }
        if supplied.len() != 64
            || !supplied
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return false;
        }
        let Some(session) = active.as_mut() else {
            return false;
        };
        if token_matches(Some(supplied), &session.id) {
            session.last_used = now;
            true
        } else {
            false
        }
    }

    fn clear(&self) {
        *self.0.lock().unwrap() = None;
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

impl Drop for ControlRequest {
    fn drop(&mut self) {
        self.reply.close_sender();
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

struct CommandSink(Arc<CommandQueue>);

impl CommandSink {
    fn enqueue(&self, request: Arc<ControlRequest>) -> Result<(), &'static str> {
        self.0.enqueue(request).map_err(|error| match error {
            QueueError::Full => "renderer command queue is full",
            QueueError::Closed => "renderer command channel is closed",
        })
    }

    fn close(&self) {
        self.0.close();
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
    /// Bind a loopback port and deliver owned values through the typed ring.
    pub fn start(config: ServerConfig) -> io::Result<(Self, ValueReceiver)> {
        let (server, receiver) = Self::start_typed(config)?;
        Ok((server, ValueReceiver(receiver)))
    }

    /// Bind a loopback port and hand typed request pointers to the renderer.
    pub fn start_typed(config: ServerConfig) -> io::Result<(Self, CommandReceiver)> {
        let queue = CommandQueue::new(MAX_PENDING_COMMANDS);
        let server = Self::start_with_sink(config, CommandSink(Arc::clone(&queue)))?;
        Ok((server, CommandReceiver(queue)))
    }

    /// Bind a loopback port with an authentication token supplied by the embedding process.
    pub fn start_typed_with_token(
        config: ServerConfig,
        token: impl Into<String>,
    ) -> io::Result<(Self, CommandReceiver)> {
        let queue = CommandQueue::new(MAX_PENDING_COMMANDS);
        let server =
            Self::start_with_sink_and_token(config, CommandSink(Arc::clone(&queue)), token.into())?;
        Ok((server, CommandReceiver(queue)))
    }

    fn start_with_sink(config: ServerConfig, commands: CommandSink) -> io::Result<Self> {
        Self::start_with_sink_and_token(config, commands, random_hex(32)?)
    }

    fn start_with_sink_and_token(
        config: ServerConfig,
        commands: CommandSink,
        token: String,
    ) -> io::Result<Self> {
        if token.len() < 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "debug-control token must contain at least 32 bytes",
            ));
        }
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
        let waker = ServiceWaker::default();
        let state = Arc::new(ServerState {
            token: Arc::new(token.clone()),
            active_session: ActiveSession::default(),
            commands,
            waker: waker.clone(),
            shutdown: AtomicBool::new(false),
            stop_waker: Mutex::new(None),
        });
        write_descriptor(&config, address)?;
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

fn write_descriptor(config: &ServerConfig, address: SocketAddr) -> io::Result<()> {
    let descriptor = Descriptor {
        pid: std::process::id(),
        address: address.to_string(),
        protocol_version: PROTOCOL_VERSION,
        renderer: "blitz",
        renderer_revision: &config.renderer_revision,
    };
    let bytes = serde_json::to_vec_pretty(&descriptor).map_err(io::Error::other)?;
    if contains_environment_secret(config.descriptor_path.to_string_lossy().as_bytes())
        || contains_environment_secret(&bytes)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to write a debug-control descriptor containing an environment secret",
        ));
    }
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

fn contains_environment_secret(content: &[u8]) -> bool {
    std::env::vars_os().any(|(name, value)| {
        let name = name.to_string_lossy().to_ascii_uppercase();
        if !is_secret_environment_name(&name) {
            return false;
        }
        let value = value.to_string_lossy();
        if value.is_empty() {
            return false;
        }
        let raw = value.as_bytes();
        let encoded = serde_json::to_vec(value.as_ref()).unwrap_or_default();
        content.windows(raw.len()).any(|part| part == raw)
            || (!encoded.is_empty()
                && content
                    .windows(encoded.len())
                    .any(|part| part == encoded.as_slice()))
    })
}

fn is_secret_environment_name(name: &str) -> bool {
    [
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "SECRET",
        "SEED",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "CREDENTIAL",
        "COOKIE",
        "JWT",
        "SESSION",
    ]
    .iter()
    .any(|needle| name.contains(needle))
        || name.contains("_KEY")
        || name.ends_with("KEY")
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
        match read_request(&mut stream, &state).await {
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

async fn read_request(stream: &mut TcpStream, state: &ServerState) -> io::Result<HttpRequest> {
    read_request_within(stream, state, COMMAND_TIMEOUT).await
}

async fn read_request_within(
    stream: &mut TcpStream,
    state: &ServerState,
    within: Duration,
) -> io::Result<HttpRequest> {
    nagoya::time::timeout(within, read_request_inner(stream, state))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request timed out"))?
}

async fn read_request_inner(
    stream: &mut TcpStream,
    state: &ServerState,
) -> io::Result<HttpRequest> {
    let mut bytes = bytes::BytesMut::new();
    let mut scratch = [0_u8; 4096];
    let header_end = nagoya::time::timeout(
        HEADER_TIMEOUT,
        read_headers(stream, &mut bytes, &mut scratch),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "header timed out"))??;
    let headers = std::str::from_utf8(&bytes[..header_end - 4])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "headers are not valid UTF-8"))?;
    let mut lines = headers.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let (method, path) = parse_request_line(request_line)?;

    let mut header_fields = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid header line",
            ));
        };
        if !is_http_token(name)
            || value
                .bytes()
                .any(|byte| byte == 0x7f || (byte < 0x20 && byte != b'\t'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid header line",
            ));
        }
        let value = value.trim_matches(|character| character == ' ' || character == '\t');
        header_fields.push((name, value));
    }

    let mut host: Option<&str> = None;
    for (name, value) in &header_fields {
        if name.eq_ignore_ascii_case("host") && host.replace(*value).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exactly one Host header is required",
            ));
        }
    }
    let Some(host) = host else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exactly one Host header is required",
        ));
    };
    if !valid_loopback_host(host) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Host must be a loopback address",
        ));
    }

    let mut content_length = None;
    for (name, value) in &header_fields {
        if name.eq_ignore_ascii_case("origin") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Origin headers are not supported",
            ));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Transfer-Encoding is not supported",
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate Content-Length header",
                ));
            }
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid Content-Length header",
                ));
            }
            content_length = Some(value.parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length header")
            })?);
        }
    }
    let content_length = content_length.unwrap_or(0);

    let authorized_session = if let Some((session_id, _)) = session_path(&path) {
        if !state.active_session.matches(session_id) {
            return Ok(HttpRequest {
                method,
                path,
                body: Value::Null,
            });
        }
        true
    } else {
        false
    };

    let body_end = header_end
        .checked_add(content_length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request size overflow"))?;
    let max_request_bytes = if authorized_session {
        MAX_REQUEST_BYTES
    } else {
        MAX_SESSION_REQUEST_BYTES
    };
    if body_end > max_request_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request is too large",
        ));
    }
    if bytes.len() > body_end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request contains bytes after body",
        ));
    }
    while bytes.len() < body_end {
        let chunk_limit = scratch.len();
        read_more(
            stream,
            &mut bytes,
            &mut scratch,
            body_end,
            chunk_limit,
            "connection closed before body",
        )
        .await?;
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..body_end]).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "request body is not valid JSON")
        })?
    };
    Ok(HttpRequest { method, path, body })
}

async fn read_headers(
    stream: &mut TcpStream,
    bytes: &mut bytes::BytesMut,
    scratch: &mut [u8; 4096],
) -> io::Result<usize> {
    loop {
        if bytes.ends_with(b"\r\n\r\n") {
            let header_end = bytes.len();
            if header_end > MAX_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request headers are too large",
                ));
            }
            return Ok(header_end);
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers are too large",
            ));
        }
        read_more(
            stream,
            bytes,
            scratch,
            MAX_HEADER_BYTES,
            1,
            "connection closed before headers",
        )
        .await?;
    }
}

async fn read_more(
    stream: &mut TcpStream,
    bytes: &mut bytes::BytesMut,
    scratch: &mut [u8; 4096],
    stage_limit: usize,
    chunk_limit: usize,
    eof_message: &'static str,
) -> io::Result<usize> {
    let remaining = stage_limit
        .checked_sub(bytes.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request is too large"))?;
    if remaining == 0 || chunk_limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request is too large",
        ));
    }
    let allowed = remaining.min(chunk_limit).min(scratch.len());
    let count = std::future::poll_fn(|cx| stream.poll_read(cx, &mut scratch[..allowed]))
        .await
        .map_err(reactor_error)?;
    if count == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, eof_message));
    }
    bytes.extend_from_slice(&scratch[..count]);
    Ok(count)
}

fn parse_request_line(line: &str) -> io::Result<(String, String)> {
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || !is_http_token(method)
        || !valid_origin_target(path)
        || (version != "HTTP/1.0" && version != "HTTP/1.1")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }
    Ok((method.to_string(), path.to_string()))
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_origin_target(target: &str) -> bool {
    if !target.starts_with('/') || !target.is_ascii() {
        return false;
    }
    let bytes = target.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
            continue;
        }
        if !(byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
                    | b'/'
                    | b'?'
            ))
        {
            return false;
        }
        index += 1;
    }
    true
}

fn valid_loopback_host(value: &str) -> bool {
    if value.is_empty()
        || !value.is_ascii()
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || matches!(byte, b',' | b'/' | b'\\' | b'@' | b'?' | b'#')
        })
    {
        return false;
    }
    if let Some(bracketed) = value.strip_prefix('[') {
        let Some((address, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        if address.contains('[') || suffix.contains(']') {
            return false;
        }
        let valid_port = if suffix.is_empty() {
            true
        } else {
            suffix.strip_prefix(':').is_some_and(valid_decimal_port)
        };
        return valid_port
            && address
                .parse::<std::net::Ipv6Addr>()
                .is_ok_and(|address| address.is_loopback());
    }
    if value.contains('[') || value.contains(']') {
        return false;
    }
    let (host, port) = match value.split_once(':') {
        Some((host, port)) => {
            if host.contains(':') || port.contains(':') {
                return false;
            }
            (host, Some(port))
        }
        None => (value, None),
    };
    if port.is_some_and(|port| !valid_decimal_port(port)) {
        return false;
    }
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback())
}

fn valid_decimal_port(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u16>().is_ok()
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

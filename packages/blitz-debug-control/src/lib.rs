//! Debug-only, loopback WebDriver transport for Blitz renderers.
//!
//! The server owns networking and session authentication. Renderer commands
//! cross a serialized channel and must be executed by the UI/runtime thread.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

const PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

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

/// A command forwarded from the HTTP server to the renderer thread.
#[derive(Debug)]
pub struct ControlRequest {
    pub method: String,
    pub path: String,
    pub body: Value,
    reply: SyncSender<ControlResponse>,
}

impl ControlRequest {
    /// Complete this request. Failure means the client already disconnected.
    pub fn respond(self, response: ControlResponse) -> Result<(), ControlResponse> {
        self.reply.send(response).map_err(|error| error.0)
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
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DebugServer {
    /// Bind a loopback port, write the descriptor, and start the server thread.
    pub fn start(config: ServerConfig) -> io::Result<(Self, Receiver<ControlRequest>)> {
        if !config.bind_address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "debug control must bind to a loopback address",
            ));
        }
        let listener = TcpListener::bind(config.bind_address)?;
        let address = listener.local_addr()?;
        let token = random_hex(32)?;
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_shutdown = Arc::clone(&shutdown);
        let thread_token = token.clone();
        let thread = thread::Builder::new()
            .name("blitz-debug-control".into())
            .spawn(move || server_loop(listener, &thread_token, command_tx, thread_shutdown))?;

        if let Err(error) = write_descriptor(&config, address, &token) {
            shutdown.store(true, Ordering::Release);
            let _ = TcpStream::connect(address);
            let _ = thread.join();
            return Err(error);
        }

        Ok((
            Self {
                address,
                token,
                descriptor_path: config.descriptor_path,
                shutdown,
                thread: Some(thread),
            },
            command_rx,
        ))
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
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.descriptor_path);
    }
}

impl Drop for DebugServer {
    fn drop(&mut self) {
        self.stop();
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

fn server_loop(
    listener: TcpListener,
    token: &str,
    command_tx: SyncSender<ControlRequest>,
    shutdown: Arc<AtomicBool>,
) {
    let mut active_session: Option<String> = None;
    for connection in listener.incoming() {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        match connection {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(COMMAND_TIMEOUT));
                let _ = stream.set_write_timeout(Some(COMMAND_TIMEOUT));
                let response = match read_request(&mut stream) {
                    Ok(request) => route(request, token, &mut active_session, &command_tx),
                    Err(error) => webdriver_error("invalid argument", error.to_string()),
                };
                let _ = write_response(&mut stream, response);
            }
            Err(_) if shutdown.load(Ordering::Acquire) => break,
            Err(_) => continue,
        }
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if bytes.len() >= MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request is too large",
            ));
        }
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
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
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .unwrap_or(0);
    if header_end + content_length > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request is too large",
        ));
    }
    while bytes.len() < header_end + content_length {
        let remaining = header_end + content_length - bytes.len();
        let mut chunk = vec![0; remaining.min(4096)];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before body",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    Ok(HttpRequest { method, path, body })
}

fn route(
    request: HttpRequest,
    token: &str,
    active_session: &mut Option<String>,
    command_tx: &SyncSender<ControlRequest>,
) -> Value {
    if request.method == "GET" && request.path == "/status" {
        return json!({"value": {
            "ready": true,
            "message": "Blitz debug control is ready",
            "protocolVersion": PROTOCOL_VERSION,
        }});
    }

    if request.method == "POST" && request.path == "/session" {
        if active_session.is_some() {
            return webdriver_error("session not created", "only one session is supported");
        }
        let supplied_token = request
            .body
            .pointer("/capabilities/alwaysMatch/blitz:token")
            .and_then(Value::as_str);
        if supplied_token != Some(token) {
            return webdriver_error("invalid argument", "invalid blitz:token capability");
        }
        let session_id = match random_hex(16) {
            Ok(value) => value,
            Err(error) => return webdriver_error("unknown error", error.to_string()),
        };
        *active_session = Some(session_id.clone());
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
    if active_session.as_deref() != Some(session_id) {
        return webdriver_error("invalid session id", "session is not active");
    }
    if request.method == "DELETE" && command_path.is_empty() {
        *active_session = None;
        return json!({"value": null});
    }

    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let control_request = ControlRequest {
        method: request.method,
        path: command_path.to_string(),
        body: request.body,
        reply: reply_tx,
    };
    match command_tx.try_send(control_request) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            return webdriver_error("timeout", "renderer command queue is full");
        }
        Err(TrySendError::Disconnected(_)) => {
            return webdriver_error("unknown error", "renderer command channel is closed");
        }
    }
    match reply_rx.recv_timeout(COMMAND_TIMEOUT) {
        Ok(ControlResponse::Success(value)) => json!({"value": value}),
        Ok(ControlResponse::Error {
            error,
            message,
            stacktrace,
        }) => json!({"value": {
            "error": error,
            "message": message,
            "stacktrace": stacktrace,
        }}),
        Err(RecvTimeoutError::Timeout) => webdriver_error("timeout", "renderer command timed out"),
        Err(RecvTimeoutError::Disconnected) => {
            webdriver_error("unknown error", "renderer response channel is closed")
        }
    }
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

fn write_response(stream: &mut TcpStream, body: Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(&body).map_err(io::Error::other)?;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn descriptor_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("blitz-debug-{nonce}.json"))
    }

    fn request(address: SocketAddr, method: &str, path: &str, body: Value) -> Value {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        };
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        serde_json::from_slice(&response[body_start..]).unwrap()
    }

    fn create_session(address: SocketAddr, token: &str) -> String {
        request(
            address,
            "POST",
            "/session",
            json!({"capabilities": {"alwaysMatch": {"blitz:token": token}}}),
        )["value"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn status_auth_session_command_and_reconnect() {
        let descriptor = descriptor_path();
        let (server, commands) = DebugServer::start(ServerConfig {
            bind_address: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            descriptor_path: descriptor.clone(),
            renderer_revision: "test-revision".into(),
        })
        .unwrap();

        let status = request(server.address(), "GET", "/status", Value::Null);
        assert_eq!(status["value"]["ready"], true);
        assert!(descriptor.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&descriptor).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let rejected = request(
            server.address(),
            "POST",
            "/session",
            json!({"capabilities": {"alwaysMatch": {"blitz:token": "wrong"}}}),
        );
        assert_eq!(rejected["value"]["error"], "invalid argument");

        let session = create_session(server.address(), server.token());
        let address = server.address();
        let command_path = format!("/session/{session}/blitz/getDomSnapshot");
        let client = thread::spawn(move || request(address, "GET", &command_path, Value::Null));
        let command = commands.recv_timeout(COMMAND_TIMEOUT).unwrap();
        assert_eq!(command.method, "GET");
        assert_eq!(command.path, "blitz/getDomSnapshot");
        command
            .respond(ControlResponse::Success(json!({"documentRevision": 7})))
            .unwrap();
        let response = client.join().unwrap();
        assert_eq!(response["value"]["documentRevision"], 7);

        let deleted = request(
            server.address(),
            "DELETE",
            &format!("/session/{session}"),
            Value::Null,
        );
        assert!(deleted["value"].is_null());
        let second_session = create_session(server.address(), server.token());
        assert_ne!(second_session, session);

        server.shutdown();
        assert!(!descriptor.exists());
    }

    #[test]
    fn rejects_non_loopback_bind_address() {
        let result = DebugServer::start(ServerConfig {
            bind_address: ([0, 0, 0, 0], 0).into(),
            descriptor_path: descriptor_path(),
            renderer_revision: "test-revision".into(),
        });
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    }

    #[test]
    fn a_full_renderer_queue_fails_without_blocking_the_server() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        command_tx
            .send(ControlRequest {
                method: "GET".into(),
                path: "occupied".into(),
                body: Value::Null,
                reply: reply_tx,
            })
            .unwrap();
        let mut active_session = Some("test-session".to_string());
        let response = route(
            HttpRequest {
                method: "GET".into(),
                path: "/session/test-session/blitz/getDomSnapshot".into(),
                body: Value::Null,
            },
            "token",
            &mut active_session,
            &command_tx,
        );
        assert_eq!(response["value"]["error"], "timeout");
        drop(command_rx);
    }
}

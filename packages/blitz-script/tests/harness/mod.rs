//! Driving a real document in a separate process, through the debug-control
//! protocol.
//!
//! The point of going through a process and a socket rather than calling into
//! `ScriptDocument` directly is that nothing here can reach past the interface a
//! driver has. A test that constructs the document and inspects its internals
//! agrees with the engine by construction; this one only knows what the running
//! document reports, which is what a person clicking the thing would find out.

#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
pub const DEADLINE: Duration = Duration::from_secs(20);
/// Between polls. A spin on `yield_now` across seven driven processes is what
/// made the connections fail in the first place.
pub const POLL: Duration = Duration::from_millis(5);

pub struct ChildGuard {
    pub child: Child,
    pub descriptor_path: PathBuf,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_file(&self.descriptor_path);
    }
}

/// Distinct per call, not merely per instant.
///
/// A wall-clock nonce is not unique here: the tests run in parallel and start
/// within microseconds of each other, so two of them drew the same path, read
/// each other's descriptor, and opened a session with the wrong token. That
/// arrives as a missing `sessionId` on whichever test lost, which reads as the
/// control protocol being broken rather than as two tests colliding, and only
/// sometimes.
static NEXT_DESCRIPTOR: AtomicUsize = AtomicUsize::new(0);

pub fn temporary_descriptor() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = NEXT_DESCRIPTOR.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("blitz-debug-{pid}-{seq}-{nonce}.json"))
}

pub fn wait_for_descriptor(path: &Path, child: &mut Child) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Ok(bytes) = fs::read(path) {
            return serde_json::from_slice(&bytes).unwrap();
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "debug harness exited early"
        );
        assert!(Instant::now() < deadline, "descriptor was not published");
        std::thread::sleep(POLL);
    }
}

/// One request, or `None` if the connection did not produce a whole response.
///
/// Separated from [`request`] because "the server refused this connection" and
/// "the server answered" are different facts, and collapsing them into an
/// `unwrap` reported a truncated response as a missing field several frames
/// away from the cause.
fn try_request(address: SocketAddr, method: &str, path: &str, body: &[u8]) -> Option<Value> {
    let mut stream = TcpStream::connect(address).ok()?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(body).ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    let body_start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    serde_json::from_slice(&response[body_start..]).ok()
}

pub fn request(address: SocketAddr, method: &str, path: &str, body: Value) -> Value {
    let body = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap()
    };

    // Retried, because these tests run in parallel and each drives its own
    // renderer process over its own socket. A connection refused under that
    // load is the machine being busy, not the engine being wrong, and it
    // arrived as an empty response several frames from anything meaningful.
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(value) = try_request(address, method, path, &body) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "no response to {method} {path} within {DEADLINE:?}"
        );
        std::thread::sleep(POLL);
    }
}

pub fn create_session(address: SocketAddr, token: &str) -> String {
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

pub fn session_request(
    address: SocketAddr,
    session: &str,
    method: &str,
    command: &str,
    body: Value,
) -> Value {
    request(
        address,
        method,
        &format!("/session/{session}/{command}"),
        body,
    )
}

/// One driven document: the harness process, a session, and the plumbing to
/// address elements in it.
pub struct Driven {
    _child: ChildGuard,
    address: SocketAddr,
    session: String,
}

impl Driven {
    /// Launch the harness on a fixture page under `tests/fixtures`.
    pub fn open(fixture: &str) -> Self {
        let page = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        assert!(page.is_file(), "no fixture at {}", page.display());

        let descriptor_path = temporary_descriptor();
        let mut child = ChildGuard {
            child: Command::new(env!("CARGO_BIN_EXE_solid-debug-harness"))
                .env("TAURI_BLITZ_DRIVER", "127.0.0.1:0")
                .env("TAURI_BLITZ_DRIVER_DESCRIPTOR", &descriptor_path)
                .env("BLITZ_DEBUG_DOCUMENT", &page)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
            descriptor_path: descriptor_path.clone(),
        };

        let descriptor = wait_for_descriptor(&descriptor_path, &mut child.child);
        let address: SocketAddr = descriptor["address"].as_str().unwrap().parse().unwrap();

        // The descriptor is published before the server will answer. Opening a
        // session against it first fails as a missing `sessionId`, which reads
        // as the control protocol being broken rather than as arriving early --
        // and only for whichever tests lost the race, so it looks flaky too.
        let deadline = Instant::now() + DEADLINE;
        loop {
            if request(address, "GET", "/status", Value::Null)["value"]["ready"]
                == Value::Bool(true)
            {
                break;
            }
            assert!(
                child.child.try_wait().unwrap().is_none(),
                "debug harness exited early"
            );
            assert!(Instant::now() < deadline, "harness never became ready");
            std::thread::sleep(POLL);
        }

        let session = create_session(address, descriptor["token"].as_str().unwrap());

        Self {
            _child: child,
            address,
            session,
        }
    }

    fn element(&self, selector: &str) -> String {
        let found = session_request(
            self.address,
            &self.session,
            "POST",
            "element",
            json!({"using": "css selector", "value": selector}),
        );
        found["value"][ELEMENT_KEY]
            .as_str()
            .unwrap_or_else(|| panic!("no element matching {selector}: {found}"))
            .to_string()
    }

    /// Activate an element the way a pointer would.
    pub fn click(&self, selector: &str) {
        let id = self.element(selector);
        session_request(
            self.address,
            &self.session,
            "POST",
            &format!("element/{id}/click"),
            json!({}),
        );
    }

    /// A live DOM property, which is where a control's real state lives.
    pub fn property(&self, selector: &str, name: &str) -> Value {
        let id = self.element(selector);
        session_request(
            self.address,
            &self.session,
            "GET",
            &format!("element/{id}/property/{name}"),
            Value::Null,
        )["value"]
            .clone()
    }

    pub fn checked(&self, selector: &str) -> bool {
        self.property(selector, "checked") == Value::Bool(true)
    }

    /// The text the document is showing, which is how a fixture reports what
    /// its listeners saw.
    pub fn text(&self, selector: &str) -> String {
        let id = self.element(selector);
        session_request(
            self.address,
            &self.session,
            "GET",
            &format!("element/{id}/text"),
            Value::Null,
        )["value"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

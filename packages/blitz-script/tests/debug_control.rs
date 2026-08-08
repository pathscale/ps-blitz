#![cfg(feature = "debug-control")]

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{Value, json};

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const DEADLINE: Duration = Duration::from_secs(10);

struct ChildGuard {
    child: Child,
    descriptor_path: PathBuf,
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

fn temporary_descriptor() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("blitz-solid-debug-{nonce}.json"))
}

fn wait_for_descriptor(path: &Path, child: &mut Child) -> Value {
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
        std::thread::yield_now();
    }
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

fn session_request(
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

#[test]
fn separate_process_controls_solid_without_fixed_sleeps() {
    let descriptor_path = temporary_descriptor();
    let mut child = ChildGuard {
        child: Command::new(env!("CARGO_BIN_EXE_solid-debug-harness"))
            .env("TAURI_BLITZ_DRIVER", "127.0.0.1:0")
            .env("TAURI_BLITZ_DRIVER_DESCRIPTOR", &descriptor_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        descriptor_path: descriptor_path.clone(),
    };

    let descriptor = wait_for_descriptor(&descriptor_path, &mut child.child);
    assert_eq!(descriptor["pid"].as_u64(), Some(child.child.id() as u64));
    let address: SocketAddr = descriptor["address"].as_str().unwrap().parse().unwrap();
    let token = descriptor["token"].as_str().unwrap();

    assert_eq!(
        request(address, "GET", "/status", Value::Null)["value"]["ready"],
        true
    );
    let rejected = request(
        address,
        "POST",
        "/session",
        json!({"capabilities": {"alwaysMatch": {"blitz:token": "wrong"}}}),
    );
    assert_eq!(rejected["value"]["error"], "invalid argument");

    let session = create_session(address, token);
    let button = session_request(
        address,
        &session,
        "POST",
        "element",
        json!({"using": "css selector", "value": "#increment"}),
    );
    let button = button["value"][ELEMENT_KEY].as_str().unwrap();
    let text = session_request(
        address,
        &session,
        "GET",
        &format!("element/{button}/text"),
        Value::Null,
    );
    assert_eq!(text["value"], "increment");
    let rect = session_request(
        address,
        &session,
        "GET",
        &format!("element/{button}/rect"),
        Value::Null,
    );
    assert!(rect["value"]["width"].as_f64().unwrap() > 0.0);
    assert!(rect["value"]["height"].as_f64().unwrap() > 0.0);
    session_request(
        address,
        &session,
        "POST",
        &format!("element/{button}/click"),
        json!({}),
    );
    let trace = session_request(
        address,
        &session,
        "POST",
        "blitz/traceEvent",
        json!({"after": 0}),
    );
    assert!(
        trace["value"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "click"
                && entry["includedDocument"] == true
                && entry["inputPath"] == "pointer-hit-test")
    );
    let idle = session_request(address, &session, "POST", "blitz/waitForIdle", json!({}));
    assert!(idle["value"]["layoutRevision"].as_u64().unwrap() > 0);

    let count = session_request(
        address,
        &session,
        "POST",
        "element",
        json!({"using": "css selector", "value": "#count"}),
    );
    let count = count["value"][ELEMENT_KEY].as_str().unwrap();
    let text = session_request(
        address,
        &session,
        "GET",
        &format!("element/{count}/text"),
        Value::Null,
    );
    assert_eq!(text["value"], "1");
    let screenshot = session_request(address, &session, "GET", "screenshot", Value::Null);
    let png = base64::engine::general_purpose::STANDARD
        .decode(screenshot["value"].as_str().unwrap())
        .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let snapshot = session_request(
        address,
        &session,
        "GET",
        "blitz/getDomSnapshot",
        Value::Null,
    );
    assert!(
        snapshot["value"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| { node["attributes"]["id"] == "count" && node["text"] == "1" })
    );
    assert!(snapshot["value"]["paintRevision"].as_u64().unwrap() > 0);

    let executed = session_request(
        address,
        &session,
        "POST",
        "execute/sync",
        json!({
            "script": "console.warn('remote-note'); return document.getElementById('count').textContent;",
            "args": [],
        }),
    );
    assert_eq!(executed["value"], "1");
    let asynchronous = session_request(
        address,
        &session,
        "POST",
        "execute/async",
        json!({
            "script": "const done = arguments[arguments.length - 1]; setTimeout(() => done(arguments[0] * 2), 1);",
            "args": [21],
        }),
    );
    assert_eq!(asynchronous["value"], 42);
    session_request(
        address,
        &session,
        "POST",
        "execute/sync",
        json!({
            "script": "const input = document.createElement('input'); input.id = 'remote-input'; document.getElementById('app').appendChild(input);",
            "args": [],
        }),
    );
    let input = session_request(
        address,
        &session,
        "POST",
        "element",
        json!({"using": "css selector", "value": "#remote-input"}),
    );
    let input = input["value"][ELEMENT_KEY].as_str().unwrap();
    session_request(
        address,
        &session,
        "POST",
        &format!("element/{input}/value"),
        json!({"text": "typed remotely"}),
    );
    let input_value = session_request(
        address,
        &session,
        "POST",
        "execute/sync",
        json!({
            "script": "return document.getElementById('remote-input').value",
            "args": [],
        }),
    );
    assert_eq!(input_value["value"], "typed remotely");
    let console = session_request(
        address,
        &session,
        "POST",
        "blitz/getConsoleEntries",
        json!({"after": 0}),
    );
    assert!(
        console["value"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["level"] == "warn" && entry["message"] == "remote-note")
    );

    let thrown = session_request(
        address,
        &session,
        "POST",
        "execute/sync",
        json!({"script": "throw new Error('deliberate-probe-error')", "args": []}),
    );
    assert_eq!(thrown["value"]["error"], "javascript error");
    let errors = session_request(
        address,
        &session,
        "POST",
        "blitz/getRuntimeErrors",
        json!({"after": 0}),
    );
    let deliberate = errors["value"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["message"]
                .as_str()
                .unwrap()
                .contains("deliberate-probe-error")
        })
        .expect("deliberate error should be retained");
    assert!(!deliberate["stack"].as_str().unwrap().is_empty());

    request(
        address,
        "DELETE",
        &format!("/session/{session}"),
        Value::Null,
    );
    let second_session = create_session(address, token);
    assert_ne!(second_session, session);
    let source = session_request(address, &second_session, "GET", "source", Value::Null);
    assert!(source["value"].as_str().unwrap().contains("id=\"count\""));
    session_request(
        address,
        &second_session,
        "POST",
        "blitz/shutdown",
        json!({}),
    );

    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "debug harness did not exit");
        std::thread::yield_now();
    }
    assert!(!descriptor_path.exists());
}

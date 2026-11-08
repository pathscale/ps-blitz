#![cfg(not(target_arch = "wasm32"))]

use blitz_net::Provider;
use blitz_platform_api::{FetchState, MemoryStorage, PlatformHost, ResourcePolicy};
use blitz_traits::platform::{Bytes, FetchError, FetchRequest, Method, OriginKey, StatusCode, Url};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::TcpListener,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);
const BODY: &[u8] = b"guest payload";

fn reply_to_post(listener: &TcpListener, path: &str, response: &str) {
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("no request for {path}: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, format!("POST {path} HTTP/1.1\r\n"));
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "incomplete headers");
        if line == "\r\n" {
            break;
        }
    }
    let mut body = [0; BODY.len()];
    reader.read_exact(&mut body).unwrap();
    assert_eq!(&body[..], BODY);
    reader.get_mut().write_all(response.as_bytes()).unwrap();
}

fn redirect(status: u16, location: &str) -> String {
    format!(
        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

#[test]
fn guest_posts_follow_same_origin_redirects_but_never_contact_another_origin() {
    let provider = Arc::new(Provider::new(None));
    let policy = Arc::new(ResourcePolicy::default());
    for status in [307, 308] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        origin.set_nonblocking(true).unwrap();
        let other = TcpListener::bind("127.0.0.1:0").unwrap();
        other.set_nonblocking(true).unwrap();
        let base = Url::parse(&format!("http://{}/", origin.local_addr().unwrap())).unwrap();
        let cross_origin = format!("http://{}/sink", other.local_addr().unwrap());
        let document = policy.document(base.clone(), &policy.browser(base.clone()));
        let (wake, ready) = mpsc::channel();
        let host = PlatformHost::new(
            OriginKey::for_document(&base),
            provider.for_scope(document.clone()),
            Arc::new(MemoryStorage::new()),
        )
        .with_initiator(document)
        .with_waker(Arc::new(move || {
            let _ = wake.send(());
        }));
        let server = thread::spawn(move || {
            reply_to_post(&origin, "/start", &redirect(status, "/same"));
            reply_to_post(
                &origin,
                "/same",
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            reply_to_post(&origin, "/cross", &redirect(status, "/next"));
            reply_to_post(&origin, "/next", &redirect(status, &cross_origin));
        });
        let post = |path| {
            FetchRequest::get(base.join(path).unwrap())
                .method(Method::POST)
                .body(Bytes::from_static(BODY))
        };

        let allowed = host.start_fetch(post("start"));
        ready.recv_timeout(TIMEOUT).expect("same-origin completion");
        assert_eq!(
            host.with_response(allowed, |response| (response.url.clone(), response.status)),
            Some((base.join("same").unwrap(), StatusCode::OK)),
        );

        let blocked = host.start_fetch(post("cross"));
        let completed = ready.recv_timeout(TIMEOUT);
        assert!(
            matches!(other.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
            "a guest POST followed the cross-origin {status} redirect",
        );
        completed.expect("cross-origin refusal");
        assert_eq!(
            host.state(blocked),
            FetchState::Failed(FetchError::Blocked("guest-fetch-origin".into())),
        );
        server.join().unwrap();
    }
}

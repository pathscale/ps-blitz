//! `<script type="module">`: parsing in module goal, resolving imports over the
//! document's fetcher, and `import.meta`.
//!
//! Before this existed, a module was handed to the classic-script evaluator and
//! died on its own first line with
//! `SyntaxError: expected token '.', got '{' in import.meta`. Every assertion
//! here is about a page that could not start at all.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use blitz_dom::DocumentConfig;
use blitz_script::{FetchError, ScriptDocument, ScriptFetcher};
use url::Url;

/// How long a fixture server waits for the engine to connect.
///
/// A server blocking in `accept()` forever turns "the loader stopped calling
/// the fetcher" into a hung test suite with no output, which is strictly worse
/// than a failure. Every accept loop here has a deadline for that reason.
const SERVE_TIMEOUT: Duration = Duration::from_secs(10);

fn config_with_base(base_url: &str) -> DocumentConfig {
    DocumentConfig {
        base_url: Some(base_url.to_owned()),
        ..Default::default()
    }
}

fn eval_string(doc: &mut ScriptDocument, expression: &str) -> String {
    match doc.eval_json(expression) {
        Ok(serde_json::Value::String(value)) => value,
        other => panic!("expected {expression} to be a string, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A loopback HTTP server and a fetcher that speaks to it.
//
// A real socket rather than an in-memory map, because the property under test
// is that the loader reaches the document's fetcher for a URL it resolved
// itself. A map keyed by the string the test already wrote down would pass
// without the resolution ever being right.
// ---------------------------------------------------------------------------

/// Serve `routes` (path -> JavaScript body) until the deadline expires, then
/// stop. Returns the origin to import from and a handle recording which paths
/// were actually requested.
fn serve_modules(
    routes: Vec<(&'static str, String)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback is available");
    let port = listener
        .local_addr()
        .expect("the socket has an address")
        .port();
    listener
        .set_nonblocking(true)
        .expect("the listener can be polled");

    let handle = std::thread::spawn(move || {
        let mut requested = Vec::new();
        let deadline = Instant::now() + SERVE_TIMEOUT;

        while Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection yet. The test either has not reached the
                    // fetch or never will; the deadline decides which.
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("the stream can time out");

            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            // Only the request head: reading to EOF would block, since the
            // client holds the connection open waiting for our response.
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(byte[0]),
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/")
                .to_owned();

            let body = routes
                .iter()
                .find(|(route, _)| *route == path)
                .map(|(_, body)| body.clone());
            requested.push(path);

            let response = match body {
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned(),
            };
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }

        requested
    });

    (format!("http://127.0.0.1:{port}"), handle)
}

/// The minimum HTTP/1.1 client needed to prove the loader calls out.
///
/// `ScriptFetcher` is synchronous, so this is a blocking round trip on the
/// document thread — the same shape a real embedder's fetcher has.
struct LoopbackFetcher;

impl ScriptFetcher for LoopbackFetcher {
    fn fetch(&self, url: &Url) -> Result<String, FetchError> {
        if url.scheme() != "http" {
            return Err(FetchError::UnsupportedScheme(url.scheme().to_owned()));
        }
        let host = url.host_str().unwrap_or("127.0.0.1");
        let port = url.port().unwrap_or(80);
        let path = url.path();

        let mut stream = TcpStream::connect((host, port)).map_err(FetchError::Io)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(FetchError::Io)?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .map_err(FetchError::Io)?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(FetchError::Io)?;

        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or_else(|| FetchError::InvalidData("no header terminator".to_owned()))?;
        if !head.starts_with("HTTP/1.1 200") {
            return Err(FetchError::InvalidData(format!(
                "unexpected status for {url}: {}",
                head.lines().next().unwrap_or_default()
            )));
        }
        Ok(body.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An inline module runs at all.
///
/// The narrowest statement of the original bug: this markup produced a
/// `SyntaxError` on `import.meta` and set nothing, because the classic-script
/// goal has no top-level `await`, no `export`, and no module `import`.
#[test]
fn an_inline_module_is_parsed_in_module_goal() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">
             const value = await Promise.resolve("module ran");
             globalThis.result = value;
           </script>"#,
        DocumentConfig::default(),
    );
    doc.execute_scripts();

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "module ran");
}

/// A classic script is unchanged.
///
/// The regression that matters: every page's scripts go through the same
/// routing decision, so a module path that captured classic scripts would break
/// far more than it fixed.
#[test]
fn a_classic_script_still_runs_unchanged() {
    let mut doc = ScriptDocument::from_html(
        r#"<div id="root"></div>
           <script>
             const el = document.createElement("span");
             el.textContent = "classic";
             document.getElementById("root").appendChild(el);
             globalThis.classicRan = "yes";
           </script>"#,
        DocumentConfig::default(),
    );
    doc.execute_scripts();

    assert_eq!(eval_string(&mut doc, "globalThis.classicRan"), "yes");
    assert_eq!(
        eval_string(&mut doc, "document.getElementById('root').textContent"),
        "classic"
    );
}

/// `import.meta.url` names the module's own URL.
///
/// Asset helpers are built on it — `new URL("./icon.svg", import.meta.url)` —
/// and an undefined value there throws inside module initialisation, taking the
/// whole module with it.
#[test]
fn import_meta_url_is_the_module_url() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">globalThis.metaUrl = import.meta.url;</script>"#,
        config_with_base("https://example.invalid/page/index.html"),
    );
    doc.execute_scripts();

    assert_eq!(
        eval_string(&mut doc, "globalThis.metaUrl"),
        "https://example.invalid/page/index.html"
    );
}

/// A module imports another module, fetched over a real socket.
///
/// This is the case that decides whether the fix is worth anything: every site
/// in the corpus that failed this way uses `import ... from`, not bare
/// `import.meta`.
#[test]
fn a_module_imports_a_relative_module_over_the_fetcher() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import { greeting } from "./dep.js";
               globalThis.result = greeting + " from the loader";"#
                .to_owned(),
        ),
        ("/dep.js", r#"export const greeting = "hello";"#.to_owned()),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.execute_scripts();

    assert_eq!(
        eval_string(&mut doc, "globalThis.result"),
        "hello from the loader"
    );

    let requested = server.join().expect("the server thread finishes");
    assert!(
        requested.iter().any(|path| path == "/dep.js"),
        "the imported module should have been fetched, saw: {requested:?}"
    );
}

/// The same module imported twice is instantiated once.
///
/// The spec requires it, and the observable consequence of getting it wrong is
/// subtle: two copies of a module's top-level state, so a store, a router, or a
/// framework registry silently splits in half.
#[test]
fn a_module_imported_twice_is_one_instance() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import { bump, count } from "./counter.js";
               import "./sibling.js";
               bump();
               globalThis.result = String(count());"#
                .to_owned(),
        ),
        (
            "/sibling.js",
            r#"import { bump } from "./counter.js";
               bump();"#
                .to_owned(),
        ),
        (
            "/counter.js",
            r#"let n = 0;
               export function bump() { n += 1; }
               export function count() { return n; }"#
                .to_owned(),
        ),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.execute_scripts();

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "2");

    let requested = server.join().expect("the server thread finishes");
    let counter_fetches = requested
        .iter()
        .filter(|path| *path == "/counter.js")
        .count();
    assert_eq!(
        counter_fetches, 1,
        "the shared module should be fetched once, saw: {requested:?}"
    );
}

/// A classic fallback marked `nomodule` does not also run.
///
/// Pages that ship both a module bundle and an ES5 one rely on the engine
/// skipping exactly one of them. Now that modules run, running the fallback too
/// would mount the application twice.
#[test]
fn a_nomodule_fallback_is_skipped() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">globalThis.ran = "module";</script>
           <script nomodule>globalThis.ran = "fallback";</script>"#,
        DocumentConfig::default(),
    );
    doc.execute_scripts();

    assert_eq!(eval_string(&mut doc, "globalThis.ran"), "module");
}

//! The crate's defining constraint, as a test.
//!
//! `blitz-platform-api` exists so that the platform APIs are written once,
//! above the transport and below the runtime. Two things in its dependency
//! graph would mean it had failed at that:
//!
//! - **An HTTP client.** `blitz-net` already ships `reqwest` with HTTP/2,
//!   cookies, compression and a cacache disk cache. A client reachable from
//!   here would mean this crate had started doing HTTP itself, which is a
//!   second and worse implementation of something that already works, and it
//!   would put a TLS stack in the graph of a crate that is supposed to be
//!   plain Rust over a trait.
//! - **A scripting engine.** Boa or wasmi appearing here would mean the layer
//!   had collapsed back into the binding it was extracted from, and
//!   `blitz-script` could no longer bind the same host.
//!
//! Either would arrive through a feature flag enabled three crates away rather
//! than through an edit anybody reviewed, which is why this resolves the real
//! graph instead of reading the manifest. Same technique, and the same reasons,
//! as `blitz-dom-api`'s `no_boa.rs`.

use std::process::Command;

/// Package names that mean an HTTP client is in the graph.
///
/// **`http` is deliberately absent, and this is the trap in this test.** The
/// `http` crate is types only, it is already in the graph via `blitz-traits`,
/// and this crate's public API is built from its `HeaderMap`, `Method` and
/// `StatusCode`. A substring or prefix match on "http" would fail this test on
/// its first run for exactly the wrong reason, in the way a substring search
/// for "boa" matches `keyboard-types` in the sibling test.
const HTTP_CLIENTS: &[&str] = &[
    "reqwest",
    "hyper",
    "isahc",
    "ureq",
    "curl",
    "surf",
    "attohttpc",
    "awc",
    "http-cache",
    "http-cache-reqwest",
    "reqwest-middleware",
];

/// Package names that mean a language runtime is in the graph.
///
/// `wasm-bindgen` is not here and must not be: it is glue for a wasm *target*,
/// not an engine that runs a guest, and it is a legitimate dependency for a
/// crate compiled to wasm32.
const SCRIPT_ENGINES: &[&str] = &[
    "boa",
    "wasmi",
    "wasmtime",
    "wasmer",
    "rquickjs",
    "quick-js",
    "v8",
    "rusty_v8",
    "deno_core",
    "rhai",
    "mlua",
    "rustpython",
];

/// Whether `name` is `banned`, or a crate from the same family.
///
/// Matches the name exactly, or the name followed by `_` or `-`, so that
/// `boa_engine` and `hyper-util` are caught while `bytes` is not caught by
/// `b`. Not a substring search, for the reason given on [`HTTP_CLIENTS`].
fn is_family(name: &str, banned: &str) -> bool {
    name == banned
        || (name.starts_with(banned)
            && matches!(name.as_bytes().get(banned.len()), Some(b'_' | b'-')))
}

/// The package name a `cargo tree` line begins with, lowercased.
fn package_name(line: &str) -> Option<String> {
    Some(line.split_whitespace().next()?.to_ascii_lowercase())
}

fn names_a_banned_crate(line: &str, banned: &[&str]) -> bool {
    let Some(name) = package_name(line) else {
        return false;
    };
    banned.iter().any(|entry| is_family(&name, entry))
}

/// Every package in the graph, one per line.
fn dependency_tree(edges: &str) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

    let output = Command::new(&cargo)
        .args([
            "tree",
            "--locked",
            "--manifest-path",
            manifest,
            "--package",
            // `ps-blitz-platform-api`, the name the fork publishes under. Same
            // miss as `no_boa.rs`: `cargo tree -p` resolves against the
            // workspace, so the unprefixed name failed both tests outright with
            // "package ID specification `blitz-platform-api` did not match any
            // packages" before either could inspect an edge.
            "ps-blitz-platform-api",
            "--edges",
            edges,
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .unwrap_or_else(|err| panic!("could not run `{cargo} tree`: {err}"));

    assert!(
        output.status.success(),
        "`cargo tree --edges {edges}` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout).into_owned();
    // A silent empty result would pass every assertion below, which is the
    // failure mode this whole test exists to avoid.
    assert!(
        tree.lines()
            .any(|line| line.starts_with("ps-blitz-traits ")),
        "`cargo tree --edges {edges}` produced no recognisable graph:\n{tree}"
    );
    tree
}

fn assert_clean(edges: &str) {
    let tree = dependency_tree(edges);
    for line in tree.lines() {
        assert!(
            !names_a_banned_crate(line, HTTP_CLIENTS),
            "an HTTP client reached the `{edges}` graph: {line}\n\
             Route through `blitz-net`'s existing client instead.\nfull tree:\n{tree}"
        );
        assert!(
            !names_a_banned_crate(line, SCRIPT_ENGINES),
            "a scripting engine reached the `{edges}` graph: {line}\n\
             This crate sits below every runtime binding.\nfull tree:\n{tree}"
        );
    }
}

#[test]
fn no_client_or_engine_in_the_shipped_dependency_graph() {
    assert_clean("normal,build");
}

/// Dev-dependencies too. One here would not ship, but it would mean the tests
/// had quietly started proving something about a graph that is not the one this
/// crate has.
#[test]
fn no_client_or_engine_in_the_full_dependency_graph() {
    assert_clean("normal,build,dev");
}

/// The manifest check, cheap, and it still catches the direct case if
/// `cargo tree` cannot run at all.
#[test]
fn the_manifest_names_neither() {
    for line in include_str!("../Cargo.toml").lines() {
        assert!(
            !names_a_banned_crate(line, HTTP_CLIENTS),
            "Cargo.toml names an HTTP client directly: {line}"
        );
        assert!(
            !names_a_banned_crate(line, SCRIPT_ENGINES),
            "Cargo.toml names a scripting engine directly: {line}"
        );
    }
}

/// The detector itself, because a check that cannot fail is not a check.
///
/// The negative cases are the ones that matter: every one of them is a crate
/// that really is in this graph, or really would be legitimate in it.
#[test]
fn the_detector_catches_the_real_names_and_leaves_the_graph_alone() {
    assert!(names_a_banned_crate("reqwest v0.12.0", HTTP_CLIENTS));
    assert!(names_a_banned_crate("hyper v1.4.1", HTTP_CLIENTS));
    assert!(names_a_banned_crate("hyper-util v0.1.7 (*)", HTTP_CLIENTS));
    assert!(names_a_banned_crate(
        "reqwest-middleware v0.4.0",
        HTTP_CLIENTS
    ));
    assert!(names_a_banned_crate("boa_engine v0.20.0", SCRIPT_ENGINES));
    assert!(names_a_banned_crate("wasmi v1.1.0", SCRIPT_ENGINES));
    assert!(names_a_banned_crate("WASMI v1.1.0", SCRIPT_ENGINES));

    // In this graph today, and every one must stay allowed.
    assert!(!names_a_banned_crate("http v1.1.0", HTTP_CLIENTS));
    assert!(!names_a_banned_crate("http v1.1.0", SCRIPT_ENGINES));
    assert!(!names_a_banned_crate("url v2.5.0", HTTP_CLIENTS));
    assert!(!names_a_banned_crate("bytes v1.7.1", HTTP_CLIENTS));
    assert!(!names_a_banned_crate(
        "keyboard-types v0.7.0",
        SCRIPT_ENGINES
    ));
    assert!(!names_a_banned_crate(
        "ps-blitz-traits v0.3.0-beta.3 (/path)",
        HTTP_CLIENTS
    ));

    // Legitimate for a wasm32 build, and not an engine.
    assert!(!names_a_banned_crate(
        "wasm-bindgen v0.2.93",
        SCRIPT_ENGINES
    ));
    assert!(!names_a_banned_crate(
        "wasm-bindgen-futures v0.4.43",
        SCRIPT_ENGINES
    ));

    assert!(!names_a_banned_crate("", HTTP_CLIENTS));
}

/// `is_family` must not match a crate that merely starts with the same letters.
#[test]
fn the_family_matcher_needs_a_separator() {
    assert!(is_family("boa", "boa"));
    assert!(is_family("boa_engine", "boa"));
    assert!(is_family("boa-engine", "boa"));
    assert!(!is_family("board-game", "boa"));
    assert!(!is_family("v8ify", "v8"));
    assert!(!is_family("curling", "curl"));
}

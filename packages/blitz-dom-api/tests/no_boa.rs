//! The crate's defining constraint, as a test.
//!
//! `blitz-dom-api` exists so that a runtime other than Boa can drive the DOM.
//! A Boa dependency anywhere in its graph, direct or transitive, would mean it
//! had failed at the one thing it is for, and a feature flag added three crates
//! away is how that would happen without anybody noticing.
//!
//! This resolves the real graph rather than reading the manifest, because the
//! manifest cannot see transitive edges.

use std::process::Command;

/// Whether a `cargo tree` line names a Boa crate.
///
/// A substring search for "boa" is not good enough in either direction: it
/// matches `keyboard-types` (which is in this graph, via `blitz-traits`) and
/// would have failed this test on the first run for no reason. Match the
/// package name, which is the first token on the line.
fn names_a_boa_crate(line: &str) -> bool {
    let Some(name) = line.split_whitespace().next() else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name == "boa" || name.starts_with("boa_") || name.starts_with("boa-")
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
            // `ps-blitz-dom-api`, the name the fork publishes under. `cargo
            // tree -p` resolves against the workspace, so the unprefixed name
            // failed the whole test with "package ID specification
            // `blitz-dom-api` did not match any packages" before it could look
            // at a single edge. The assertion below already spelled
            // `ps-blitz-dom`, so the rename was applied to one half of this
            // file and not the other.
            "ps-blitz-dom-api",
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
        tree.lines().any(|line| line.starts_with("ps-blitz-dom ")),
        "`cargo tree --edges {edges}` produced no recognisable graph:\n{tree}"
    );
    tree
}

fn assert_boa_free(edges: &str) {
    let tree = dependency_tree(edges);
    for line in tree.lines() {
        assert!(
            !names_a_boa_crate(line),
            "a Boa crate reached the `{edges}` graph: {line}\nfull tree:\n{tree}"
        );
    }
}

#[test]
fn no_boa_in_the_shipped_dependency_graph() {
    assert_boa_free("normal,build");
}

/// Dev-dependencies too. A Boa crate in the test graph would not ship, but it
/// would mean the tests had quietly started proving something about Boa's DOM
/// rather than about this one.
#[test]
fn no_boa_in_the_full_dependency_graph() {
    assert_boa_free("normal,build,dev");
}

/// The manifest check, which is cheap and catches the direct case even if
/// `cargo tree` cannot run.
#[test]
fn the_manifest_names_no_boa_crate() {
    for line in include_str!("../Cargo.toml").lines() {
        assert!(
            !names_a_boa_crate(line),
            "Cargo.toml names a Boa crate directly: {line}"
        );
    }
}

/// The detector itself, because a check that cannot fail is not a check.
/// `keyboard-types` is the real line that a substring search got wrong.
#[test]
fn the_detector_finds_boa_and_leaves_keyboard_types_alone() {
    assert!(names_a_boa_crate("boa_engine v0.20.0"));
    assert!(names_a_boa_crate("boa_runtime v0.20.0 (*)"));
    assert!(names_a_boa_crate("boa_gc v0.20.0"));
    assert!(names_a_boa_crate("boa-engine v0.20.0"));
    assert!(names_a_boa_crate("BOA_ENGINE v0.20.0"));
    assert!(!names_a_boa_crate("keyboard-types v0.7.0"));
    assert!(!names_a_boa_crate("ps-blitz-dom v0.3.0-beta.3 (/path)"));
    assert!(!names_a_boa_crate(""));
}

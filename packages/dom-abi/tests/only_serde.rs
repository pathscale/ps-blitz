//! The crate's defining constraint, as a test.
//!
//! `dom-abi` sits at the bottom of a diamond whose other corners drag in a
//! JavaScript parser (`oxc`, via `solid-layouts-oxc`), a browser engine (via
//! `ui-templates`) and a wasm runtime (via `blitz-wasm`). A dependency arriving
//! here puts it in *all* of them, which is the precise outcome splitting this
//! crate out was for. So: one dependency, `serde`, and a test that resolves the
//! real graph rather than reading the manifest, because a manifest cannot see
//! transitive edges.
//!
//! # Matching, and the mistake it is avoiding
//!
//! Package names are matched whole, against the first token on a `cargo tree`
//! line. The equivalent test in `blitz-dom-api` looks for Boa crates and
//! **failed on its first run** because a substring search for "boa" matches
//! `keyboard-types`. The same trap is set here with a sharper edge: a substring
//! search for "serde" accepts `serde_json`, `serde_yaml` and every other crate
//! in that family, so the naive version of this test would wave through exactly
//! the kind of dependency it exists to catch.
//!
//! # Why `ron` is allowed, and only where
//!
//! The round-trip test needs *an* encoding to round-trip through, and RON is
//! the one these types are written in today. It is a dev-dependency, and the
//! dev graph is asserted separately and just as tightly: RON must never reach
//! the shipped graph, because a consumer that wants CBOR or postcard should get
//! it by choosing a different serde format, not by taking RON along.

use std::collections::BTreeSet;
use std::process::Command;

/// The package name a `cargo tree --format {p}` line names.
///
/// The first whitespace-separated token. Everything after it is the version,
/// the path, and possibly a ` (*)` de-duplication marker.
fn package_name(line: &str) -> Option<&str> {
    let name = line.split_whitespace().next()?;
    (!name.is_empty()).then_some(name)
}

/// Every package name in the graph rooted at `spec`.
fn packages_in_tree(spec: &str, edges: &str, depth: Option<&str>) -> BTreeSet<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

    let mut args = vec![
        "tree",
        "--locked",
        "--manifest-path",
        manifest,
        "--package",
        spec,
        "--edges",
        edges,
        "--prefix",
        "none",
        "--format",
        "{p}",
    ];
    if let Some(depth) = depth {
        args.push("--depth");
        args.push(depth);
    }

    let output = Command::new(&cargo)
        .args(&args)
        .output()
        .unwrap_or_else(|err| panic!("could not run `{cargo} tree`: {err}"));

    assert!(
        output.status.success(),
        "`cargo tree --package {spec} --edges {edges}` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout).into_owned();
    let names: BTreeSet<String> = tree
        .lines()
        .filter_map(package_name)
        .map(str::to_owned)
        .collect();

    // A silent empty result would pass every assertion below, which is the
    // failure mode this whole test exists to avoid.
    assert!(
        !names.is_empty(),
        "`cargo tree --package {spec} --edges {edges}` produced no recognisable graph:\n{tree}"
    );
    names
}

/// The direct dependencies: the depth-1 graph, minus this crate itself.
fn direct_dependencies(edges: &str) -> BTreeSet<String> {
    let mut names = packages_in_tree("dom-abi", edges, Some("1"));
    assert!(
        names.remove("dom-abi"),
        "the depth-1 tree did not contain this crate; the output shape changed"
    );
    names
}

#[test]
fn the_shipped_graph_has_exactly_one_direct_dependency() {
    let direct = direct_dependencies("normal,build");
    assert_eq!(
        direct,
        BTreeSet::from(["serde".to_owned()]),
        "this crate takes one dependency and it is serde"
    );
}

#[test]
fn nothing_outside_serdes_own_graph_reaches_the_shipped_graph() {
    let ours = packages_in_tree("dom-abi", "normal,build", None);
    let mut allowed = packages_in_tree("serde", "normal,build", None);
    allowed.insert("dom-abi".to_owned());

    let extra: Vec<&String> = ours.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "packages reached the shipped graph from outside serde's: {extra:?}"
    );
}

#[test]
fn the_dev_graph_adds_ron_and_nothing_else() {
    let direct = direct_dependencies("normal,build,dev");
    assert_eq!(
        direct,
        BTreeSet::from(["ron".to_owned(), "serde".to_owned()]),
        "the round-trip test's encoder is the only dev-dependency"
    );

    let ours = packages_in_tree("dom-abi", "normal,build,dev", None);
    let mut allowed = packages_in_tree("serde", "normal,build,dev", None);
    allowed.extend(packages_in_tree("ron", "normal,build,dev", None));
    allowed.insert("dom-abi".to_owned());

    let extra: Vec<&String> = ours.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "packages reached the dev graph from outside serde's and ron's: {extra:?}"
    );
}

#[test]
fn ron_does_not_reach_the_shipped_graph() {
    let ours = packages_in_tree("dom-abi", "normal,build", None);
    assert!(
        !ours.contains("ron"),
        "RON is the encoding the tests use, not one this crate ships:\n{ours:?}"
    );
}

/// The manifest check: cheap, direct-only, and it still works when `cargo tree`
/// cannot run at all.
#[test]
fn the_manifest_declares_one_dependency() {
    let manifest = include_str!("../Cargo.toml");

    let mut in_dependencies = false;
    let mut declared: Vec<String> = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        declared.push(name.trim().trim_matches('"').to_owned());
    }

    assert_eq!(declared, vec!["serde".to_owned()]);
}

/// The detector itself, because a check that cannot fail is not a check.
///
/// `serde_json` is the line a substring search gets wrong here, the way
/// `keyboard-types` was the line it got wrong in `blitz-dom-api`.
#[test]
fn the_detector_reads_names_and_not_substrings() {
    assert_eq!(package_name("serde v1.0.219"), Some("serde"));
    assert_eq!(
        package_name("dom-abi v0.3.0-beta.3 (/Users/x/ps-blitz/packages/dom-abi)"),
        Some("dom-abi")
    );
    assert_eq!(package_name("serde v1.0.219 (*)"), Some("serde"));
    assert_eq!(package_name(""), None);

    // The whole point: these are different packages, and a substring match
    // would have accepted all three as "serde".
    for impostor in [
        "serde_json v1.0.140",
        "serde_yaml v0.9.34",
        "serde-wasm-bindgen v0.6.5",
    ] {
        let name = package_name(impostor).expect("a name");
        assert_ne!(name, "serde");
        assert!(
            impostor.contains("serde"),
            "this case is only interesting because a substring search accepts it"
        );
    }
}

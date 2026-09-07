//! `URL` and `TextEncoder`/`TextDecoder` are WHATWG globals every browser has.
//!
//! They were absent here for a reason that is easy to miss: `boa_runtime`
//! implements them, but nothing registered them. Only `Console` was, so a page
//! got a JavaScript engine with a console and no web platform around it.
//!
//! The failure that found this is worth recording, because it does not look
//! like a missing-global failure. `@solidjs/router` constructs a `URL` while
//! its module is evaluating, so a router-based application does not break on
//! navigation: it throws before the first component renders, and the page is
//! blank with one line in the log.
//!
//! Each assertion below reads a *parsed* field rather than checking that a
//! constructor exists, because a constructor that exists and returns a bare
//! object would satisfy the weaker test.

use blitz_script::ScriptDocument;

fn eval_string(doc: &mut ScriptDocument, code: &str) -> String {
    doc.eval(&format!("globalThis.__out = String({code});"));
    doc.eval_json("globalThis.__out")
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn page() -> ScriptDocument {
    ScriptDocument::from_html(
        "<html><body></body></html>",
        blitz_dom::DocumentConfig::default(),
    )
}

/// The constructor is callable, not merely present.
#[test]
fn url_parses_its_components() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        eval_string(
            &mut doc,
            r#"(() => { const u = new URL("https://user@host.example:8443/a/b?x=1#frag");
                return [u.protocol, u.hostname, u.port, u.pathname, u.search, u.hash].join(" "); })()"#
        ),
        "https: host.example 8443 /a/b ?x=1 #frag"
    );
}

/// Relative resolution against a base is the half a naive stub gets wrong.
#[test]
fn url_resolves_against_a_base() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        eval_string(
            &mut doc,
            r#"new URL("../c", "https://host.example/a/b/d").href"#
        ),
        "https://host.example/a/c"
    );
}

/// An invalid URL throws rather than yielding a half-parsed object.
///
/// The error's name is part of the assertion. `new URL("/x")` throws whether
/// the constructor is missing or the string is bad, so a test that only
/// checked "it threw" would pass against no `URL` at all.
#[test]
fn url_rejects_a_relative_string_with_no_base() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        eval_string(
            &mut doc,
            r#"(() => { try { new URL("/no-scheme"); return "no throw"; }
                catch (e) { return e.name; } })()"#
        ),
        "TypeError"
    );
}

/// `TextEncoder` round-trips through `TextDecoder`, including a non-ASCII
/// character, which is where a byte-per-char stub diverges.
#[test]
fn text_encoding_round_trips_non_ascii() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        eval_string(
            &mut doc,
            r#"(() => { const bytes = new TextEncoder().encode("é");
                return bytes.length + ":" + new TextDecoder().decode(bytes); })()"#
        ),
        "2:é"
    );
}

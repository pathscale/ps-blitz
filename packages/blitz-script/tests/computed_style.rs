//! `getComputedStyle` answers with the engine's values, not with a stub.
//!
//! Measured over a hundred-site corpus, three sites stopped on
//! `getComputedStyle is not defined`. The tempting fix is an object that
//! returns the empty string for everything, and it is worse than the error: a
//! page that reads `display` and gets `""` concludes nothing is hidden and lays
//! itself out wrongly, with nothing in the log to say why. The error at least
//! stopped loudly.
//!
//! So these tests assert real values. A stub would pass a test that only
//! checked the call did not throw, which is exactly the test not to write.

use blitz_dom::Document as _;
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
        r#"<html><head><style>
             #shown { display: flex; width: 320px; height: 40px; color: rgb(1, 2, 3); }
             #gone  { display: none; }
           </style></head>
           <body style="margin:0">
             <div id="shown">visible</div>
             <div id="gone">hidden</div>
           </body></html>"#,
        blitz_dom::DocumentConfig::default(),
    )
}

/// The value distinguishes a hidden element from a shown one.
///
/// This is the assertion a stub cannot pass, and the one the failing sites
/// actually depended on.
#[test]
fn a_hidden_element_reports_display_none() {
    let mut doc = page();
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let shown = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).display",
    );
    let gone = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('gone')).display",
    );

    assert_eq!(shown, "flex", "a shown element should report its display");
    assert_eq!(gone, "none", "a hidden element should report `none`");
    assert_ne!(
        shown, gone,
        "a stub returning one value for both is the bug"
    );
}

/// `getPropertyValue` reads the same values as property access.
#[test]
fn get_property_value_agrees_with_property_access() {
    let mut doc = page();
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let direct = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).display",
    );
    let via_getter = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).getPropertyValue('display')",
    );

    assert_eq!(direct, "flex");
    assert_eq!(via_getter, direct);
}

/// Hyphenated names are also reachable in their camelCase spelling, because
/// scripts read `style.fontSize` as often as they ask for `font-size`.
#[test]
fn a_hyphenated_property_is_reachable_both_ways() {
    let mut doc = page();
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let hyphen = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).getPropertyValue('font-size')",
    );
    let camel = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).fontSize",
    );

    assert!(
        hyphen.ends_with("px"),
        "font-size should be a length, got {hyphen:?}"
    );
    assert_eq!(camel, hyphen);
}

/// `width` reports the used value, not the specified one.
///
/// A page asking `getComputedStyle(el).width` wants to know how wide the box
/// ended up. `auto` is not an answer it can use.
#[test]
fn width_reports_the_used_value() {
    let mut doc = page();
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let width = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).width",
    );

    assert_eq!(width, "320px", "width should be the laid-out width");
}

/// A property outside the supported set reads as unset rather than throwing.
#[test]
fn an_unsupported_property_reads_as_empty() {
    let mut doc = page();
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let value = eval_string(
        &mut doc,
        "getComputedStyle(document.getElementById('shown')).getPropertyValue('clip-path')",
    );

    assert_eq!(value, "", "an unsupported property should read as unset");
}

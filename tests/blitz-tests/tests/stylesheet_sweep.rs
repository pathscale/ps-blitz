//! Resolve every class in the application's shipped stylesheet, one at a time.
//!
//! 0.6.1 aborted about two seconds after `boot: ready`, inside stylo:
//!
//! ```text
//! stylo-0.20.0/values/computed/length_percentage.rs:658:13:
//! internal error: entered unreachable code: resolve_map should turn
//! percentages to lengths, and parsing should ensure that we don't end up with
//! a number
//! ```
//!
//! The panic names a line in a registry crate and no frame of ours, and a
//! release build's backtrace is 78 frames of `__mh_execute_header`, so nothing
//! in the log says which declaration on which element reached it. This finds it
//! from the other end: pull every class selector out of the stylesheet the app
//! actually embeds, apply them one per document, and report the ones that
//! abort. A class that panics here is the declaration.
//!
//! Regenerate the fixture from a built frontend with:
//!   cp agencyzero/apps/gui/dist/static/css/index.*.css \
//!      ps-blitz/tests/blitz-tests/fixtures/app.css

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const CSS: &str = include_str!("../fixtures/app.css");

/// Every class name the stylesheet defines, unescaped back to what an author
/// would write in `class=`.
fn classes() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes: Vec<char> = CSS.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '.' {
            i += 1;
            continue;
        }
        let mut name = String::new();
        let mut j = i + 1;
        while j < bytes.len() {
            let c = bytes[j];
            if c == '\\' && j + 1 < bytes.len() {
                // Tailwind escapes `[`, `%`, `.`, `/` and friends in selectors.
                name.push(bytes[j + 1]);
                j += 2;
                continue;
            }
            if c.is_alphanumeric() || "-_[]".contains(c) {
                name.push(c);
                j += 1;
                continue;
            }
            break;
        }
        // A leading digit means this was a decimal (`.5rem`), not a selector.
        if name.len() > 1
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && !found.contains(&name)
        {
            found.push(name);
        }
        i = j.max(i + 1);
    }
    found
}

fn resolve_with_class(class: &str) {
    let html = format!(
        r#"<html><head><style>{CSS}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div class="{class}"><span class="{class}">text</span></div>
           </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
}

#[test]
fn the_fixture_is_the_shipped_stylesheet() {
    assert!(CSS.len() > 100_000, "stylesheet is {} bytes", CSS.len());
    assert!(
        classes().len() > 500,
        "only found {} classes",
        classes().len()
    );
}

#[test]
fn no_class_in_the_shipped_stylesheet_aborts_layout() {
    // The panic unwinds before it aborts, so every class can be tried in one
    // run rather than one process per class. The default hook would print 78
    // frames of nothing per failure, so it is silenced for the sweep.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut killers: Vec<String> = Vec::new();
    for class in classes() {
        let name = class.clone();
        if std::panic::catch_unwind(move || resolve_with_class(&name)).is_err() {
            killers.push(class);
        }
    }
    std::panic::set_hook(previous);
    killers.sort();

    assert_eq!(
        killers,
        Vec::<String>::new(),
        "the set of classes that abort layout changed: {killers:?}"
    );
}

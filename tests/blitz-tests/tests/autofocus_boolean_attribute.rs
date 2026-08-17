//! `autofocus` is a boolean attribute, so its presence is what counts.
//!
//! blitz-dom asked for the literal string "true", which is the one spelling a
//! real page almost never uses. HTML writes `<input autofocus>`, the parser
//! stores that as the empty string, and every framework that sets it does the
//! same: Solid's `setBoolAttribute` is `node.setAttribute(name, "")`.
//!
//! So a field marked `autofocus` in markup never took focus. In AgencyZero
//! that was the rename box behind the pencil next to a project name: clicking
//! the pencil swapped in a text field that was never focused, so typing went
//! nowhere and the name could not be edited.
//!
//! blitz-script worked around it by writing "true" from its property setter,
//! which fixed the JS path and left the parsed path broken.

use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{node_id::NodeId, shell::{ColorScheme, Viewport}};
use std::sync::Arc;

/// Whether `#field` is the focused node, in one document rather than two:
/// node ids are per document, so comparing across two builds of the same
/// markup compares nothing.
fn field_is_focused(html: &str) -> bool {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let inner = doc.inner();
    let field: NodeId = inner
        .query_selector("#field")
        .unwrap()
        .expect("no node matching #field");
    inner.get_focussed_node_id() == Some(field)
}

/// The spelling every framework and every hand-written page uses.
#[test]
fn a_bare_autofocus_attribute_takes_focus() {
    const HTML: &str = r#"<html><body><input id="field" autofocus></body></html>"#;
    assert!(
        field_is_focused(HTML),
        "`<input autofocus>` should focus the field"
    );
}

/// The explicit empty string, which is what `setAttribute(name, "")` produces.
#[test]
fn an_empty_autofocus_attribute_takes_focus() {
    const HTML: &str = r#"<html><body><input id="field" autofocus=""></body></html>"#;
    assert!(field_is_focused(HTML));
}

/// And the spelling blitz-dom used to require, which must keep working.
#[test]
fn autofocus_true_still_takes_focus() {
    const HTML: &str = r#"<html><body><input id="field" autofocus="true"></body></html>"#;
    assert!(field_is_focused(HTML));
}

/// A boolean attribute is false only by being absent, so even "false" is true.
/// That is the spec, and it is why the string test was the wrong test.
#[test]
fn autofocus_false_is_still_present_and_still_focuses() {
    const HTML: &str = r#"<html><body><input id="field" autofocus="false"></body></html>"#;
    assert!(
        field_is_focused(HTML),
        "a boolean attribute is false only when it is absent"
    );
}

/// Without the attribute nothing is focused, so the fix cannot be "focus the
/// first field you see".
#[test]
fn no_autofocus_attribute_focuses_nothing() {
    const HTML: &str = r#"<html><body><input id="field"></body></html>"#;
    assert!(!field_is_focused(HTML));
}

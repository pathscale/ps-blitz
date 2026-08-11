//! `get_cursor` returning `None` means `cursor: none` — hide the pointer.
//!
//! The shell acts on that literally: `None` calls `set_cursor_visible(false)`.
//! So every path meaning "I have nothing to say about the cursor here" has to
//! answer `Default`, not `None`, or the pointer disappears wherever that path
//! is taken. It used to be taken by three separate `?`s — no hover node, no
//! primary styles, and an un-hovered sub-document — and the last of those is
//! reached by simply moving the mouse into page content in any browser that
//! embeds its pages as sub-documents.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use cursor_icon::CursorIcon;
use std::sync::Arc;

fn make_doc(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

#[test]
fn a_document_with_no_hover_node_shows_a_cursor() {
    let doc = make_doc("<html><body><div>hello</div></body></html>");

    // Nothing has been hovered, so there is no hit node and no hover node.
    // That is not `cursor: none`; it is no information, and the pointer must
    // stay visible.
    assert_eq!(
        doc.get_cursor(),
        Some(CursorIcon::Default),
        "an un-hovered document hid the cursor"
    );
}

#[test]
fn hovering_plain_content_shows_a_cursor() {
    let mut doc = make_doc(
        "<html><body style='margin:0'>\
         <div style='width:400px;height:300px;background:red'></div>\
         </body></html>",
    );
    doc.set_hover_to(200.0, 150.0);

    let cursor = doc.get_cursor();
    assert!(
        cursor.is_some(),
        "hovering ordinary content hid the cursor: {cursor:?}"
    );
}

#[test]
fn cursor_none_still_hides_the_pointer() {
    let mut doc = make_doc(
        "<html><body style='margin:0'>\
         <div style='width:400px;height:300px;cursor:none'></div>\
         </body></html>",
    );
    doc.set_hover_to(200.0, 150.0);

    // The one case that legitimately answers `None`. If this starts returning
    // `Default`, the fix for the disappearing pointer went too far and
    // `cursor: none` no longer works.
    assert_eq!(
        doc.get_cursor(),
        None,
        "cursor: none no longer hides the pointer"
    );
}

#[test]
fn a_pointer_cursor_survives_over_a_link() {
    let mut doc = make_doc(
        "<html><body style='margin:0'>\
         <a href='https://example.com' style='display:block;width:400px;height:300px'>x</a>\
         </body></html>",
    );
    doc.set_hover_to(200.0, 150.0);

    assert_eq!(doc.get_cursor(), Some(CursorIcon::Pointer));
}

#[test]
fn entering_an_unhovered_subdocument_does_not_hide_the_cursor() {
    // The shape a browser has: the page lives in a sub-document, and the
    // pointer crosses into it from chrome that is in the outer document. The
    // outer hit lands on the frame, which delegates to the inner document —
    // and that inner document has no hover state yet, because nothing has
    // hovered it. Delegating its "no hover node" answer straight through
    // reads as `cursor: none` and the pointer vanishes on entry.
    let mut doc = make_doc(
        "<html><body style='margin:0'>\
         <iframe srcdoc='<html><body>inner</body></html>' \
                 style='display:block;width:400px;height:300px;border:0'></iframe>\
         </body></html>",
    );
    doc.resolve(0.0);
    doc.set_hover_to(200.0, 150.0);

    let cursor = doc.get_cursor();
    assert!(
        cursor.is_some(),
        "moving into a sub-document hid the cursor: {cursor:?}"
    );
}

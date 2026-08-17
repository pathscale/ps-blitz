//! Double click selects a word, triple click selects a line, in ordinary
//! document text.
//!
//! The click count was computed on every pointer down but only ever read in
//! the text-input branch of `handle_pointerdown`: an `<input>` got
//! `select_word_at_point`, and everything else collapsed the selection to a
//! caret no matter how many times it was clicked. So double clicking a word in
//! a transcript, a label or a paragraph selected nothing, and because Cmd+C
//! copies `get_selected_text()`, nothing could be copied that way either.

use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    events::{
        BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, Point,
        PointerCoords, PointerDetails, UiEvent,
    },
    shell::{ColorScheme, Viewport},
};
use std::sync::Arc;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
  body { margin: 0; font-size: 16px; }
  p { margin: 0; }
</style></head>
<body><p id="para">alpha beta gamma</p><p id="second">delta epsilon</p></body></html>
"#;

fn make_doc() -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn pointer_event(x: f32, y: f32, buttons: MouseEventButtons) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Point::default(),
        active_pointers: Default::default(),
    }
}

/// One press-and-release at a point, without moving.
fn click_at(doc: &mut HtmlDocument, x: f32, y: f32) {
    let held = MouseEventButtons::from(MouseEventButton::Main);
    doc.handle_ui_event(UiEvent::PointerDown(pointer_event(x, y, held)));
    doc.handle_ui_event(UiEvent::PointerUp(pointer_event(
        x,
        y,
        MouseEventButtons::None,
    )));
}

/// A point inside the word "beta" of the first paragraph.
///
/// Resolved from the layout rather than guessed, so the test does not depend
/// on the metrics of whatever font the machine happens to supply: walk the
/// paragraph's box and take the first x whose byte offset lands inside
/// "beta".
fn point_in_beta(doc: &HtmlDocument) -> Option<(f32, f32)> {
    let para = doc.query_selector("#para").ok()??;
    let rect = doc.get_client_bounding_rect(para)?;
    let y = (rect.y + rect.height / 2.0) as f32;
    let text = "alpha beta gamma";
    let beta = text.find("beta")?;
    let beta_end = beta + "beta".len();

    let mut x = rect.x as f32;
    let right = (rect.x + rect.width) as f32;
    while x < right {
        if let Some((_, offset)) = doc.find_text_position(x, y) {
            // Stay off the boundaries: a click exactly on the edge is
            // ambiguous about which word it belongs to.
            if offset > beta && offset < beta_end {
                return Some((x, y));
            }
        }
        x += 1.0;
    }
    None
}

#[test]
fn double_click_selects_the_word_under_the_pointer() {
    let mut doc = make_doc();
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    click_at(&mut doc, x, y);
    click_at(&mut doc, x, y);

    assert!(
        doc.has_text_selection(),
        "expected a selection after a double click"
    );
    assert_eq!(
        doc.get_selected_text().as_deref(),
        Some("beta"),
        "a double click should select exactly the word under the pointer"
    );
}

#[test]
fn triple_click_selects_the_whole_line() {
    let mut doc = make_doc();
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    click_at(&mut doc, x, y);
    click_at(&mut doc, x, y);
    click_at(&mut doc, x, y);

    assert!(
        doc.has_text_selection(),
        "expected a selection after a triple click"
    );
    assert_eq!(
        doc.get_selected_text().as_deref(),
        Some("alpha beta gamma"),
        "a triple click should select the whole line, and stop at its end"
    );
}

#[test]
fn a_single_click_still_collapses_the_selection() {
    let mut doc = make_doc();
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    click_at(&mut doc, x, y);

    assert_eq!(
        doc.get_selected_text(),
        None,
        "a single click places a caret, it does not select text"
    );
}

/// Two clicks far enough apart in space are two single clicks, not a double
/// click, even when they arrive within the double-click interval.
#[test]
fn two_distant_clicks_do_not_select_a_word() {
    let mut doc = make_doc();
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    click_at(&mut doc, x, y);
    click_at(&mut doc, x + 40.0, y);

    assert_eq!(
        doc.get_selected_text(),
        None,
        "a second click elsewhere starts a new caret, it does not select a word"
    );
}

//! Inline content wider than its box must start at the box, not be centred on it.
//!
//! Taken from a live application document rather than invented: a project-list
//! row whose label is a shell command with no break opportunity in it. Measured
//! through the debug driver on 2026-08-12:
//!
//! ```text
//! SPAN   (no class)                                w=873  x=702
//! BUTTON min-w-0 flex-1 ... text-left truncate     w=230  x=1023
//! ```
//!
//! Span centre 1138.5, button centre 1138. The span is centred on a box it is
//! nearly four times wider than, so it hangs 321px off the *left* as well as
//! the right, and `overflow: hidden` clips neither side to the box. Forty
//! elements in one session did it, which is the "text spills past its
//! container" bug recorded in the application's TODO as not reproducible
//! synthetically.
//!
//! The button carries `text-align: left` explicitly, so whatever centres this
//! is not reading it.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

/// The label of the row, with no space or hyphen anywhere parley could break.
const UNBREAKABLE: &str = concat!(
    "cd/Users/revenge/code/blitz-rust&&git-diff;echo===age===;",
    "cd/Users/revenge/code/tauri-runtime-blitz&&git-status;echo===done===;",
    "cd/Users/revenge/code/agencyzero&&cargo-build---release;echo===built===",
);

fn document(button_style: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0">
             <div id="row" style="display:flex; align-items:baseline; gap:8px; width:230px">
               <button id="label" style="min-width:0; flex:1 1 0%; {button_style}">
                 <span id="text">{UNBREAKABLE}</span>
               </button>
             </div>
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
    doc
}

/// `(x, width)` of the element with this id, in the document's own coordinates.
fn box_of(doc: &HtmlDocument, id: &str) -> (f32, f32) {
    let node_id = doc
        .query_selector(&format!("#{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("no #{id}"));
    let layout = doc.get_node(node_id).unwrap().final_layout();
    (layout.location.x, layout.size.width)
}

/// The reproduction, as measured. `truncate` is Tailwind's
/// `overflow: hidden; text-overflow: ellipsis; white-space: nowrap`.
#[test]
fn overflowing_inline_content_starts_at_its_box_rather_than_centring_on_it() {
    let doc = document(
        "text-align: left; overflow: hidden; text-overflow: ellipsis; white-space: nowrap",
    );

    let (button_x, button_width) = box_of(&doc, "label");
    let (text_x, text_width) = box_of(&doc, "text");

    // Clipped, so it must sit inside the box on both edges. Before the fix the
    // label was a flex item under the user-agent sheet's
    // `justify-content: center`, and it hung 570px off the left of a 230px box.
    assert!(
        text_x >= button_x - 1.0,
        "clipped text starts {}px to the left of its box (text {text_x}, box {button_x})",
        button_x - text_x
    );
    assert!(
        text_x + text_width <= button_x + button_width + 1.0,
        "clipped text runs {}px past the right of its box",
        (text_x + text_width) - (button_x + button_width)
    );
}

/// The same box without the clip, to tell "overflow is mishandled" apart from
/// "inline content is misplaced whenever it overflows".
#[test]
fn the_same_overflow_without_a_clip_also_starts_at_its_box() {
    let doc = document("text-align: left; white-space: nowrap");

    let (button_x, _) = box_of(&doc, "label");
    let (text_x, _) = box_of(&doc, "text");

    assert!(
        text_x >= button_x - 1.0,
        "overflowing text starts {}px to the left of its box (text {text_x}, box {button_x})",
        button_x - text_x
    );
}

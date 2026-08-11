//! An inline-level element must move when its line does.
//!
//! Taken from a live instance, not invented. With a transcript 910px wide the
//! diagnostics reported six boxes inside it sitting past its right edge, all of
//! them 28px tall inline elements in blocks that had themselves resolved to
//! 713px and wrapped to three lines:
//!
//!   1009.9px right  button   child[1620,2165 166x28]  parent[63,2167 713x85]
//!    987.8px right  generic  child[1655,2844 109x28]  parent[63,2844 713x85]
//!    872.8px right  generic  child[1540, 938 109x28]  parent[63, 938 713x85]
//!
//! The block's own box is right and its line count is right, so the text
//! rewrapped. What did not move is the box of the inline *element* on that
//! line: a `<button class="inline">` for an item reference, and inline `<code>`
//! spans. They keep the x they were given when the pane was wider, which is
//! what the owner sees as text escaping the message.
//!
//! The width changes for real reasons: showing and hiding the project sidebar,
//! and resizing the window.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDE: u32 = 1400;
const NARROW: u32 = 760;

/// Prose long enough to wrap several times at both widths, with inline elements
/// spread through it rather than only at the start: an inline box on the first
/// line can be right by accident.
/// The bubble is shrink-to-fit: `align-self: flex-start` with a percentage cap,
/// which is what the application's agent bubble is. That matters, because a
/// shrink-to-fit box is measured at max-content first and only then laid out at
/// the width it actually got, so the inline content is laid out twice at two
/// different widths. A fixed-width pane never does that, which is why the first
/// version of this fixture passed.
const HTML: &str = r#"<html><body style="margin:0">
    <div id="pane" style="display:flex; flex-direction:column; width:100%;
                          padding:0 24px; font-size:14px; line-height:28px;">
      <div id="bubble" style="display:flex; flex-direction:column;
                              align-self:flex-start; max-width:88%; padding:12px 16px;">
      <p id="prose">
        I am checking the running bundle identity and the active profile path to
        determine whether data was deleted, and the reference
        <button id="chip" style="display:inline; font-weight:600;">item-198a4811</button>
        covers it. The data still exists: the new process is writing to
        <code id="code">com.pathscale.agencyzero</code> even though the bundle
        that was opened is named Experimental, and the older store and its
        snapshots are both intact and readable on disk right now.
      </p>
      </div>
    </div>
  </body></html>"#;

fn document(width: u32) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(width, 900, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// The right edge of a node, and the right edge of the block containing it.
fn edges(doc: &HtmlDocument, selector: &str) -> (f32, f32) {
    let id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no {selector}"));
    let node = doc.get_node(id).unwrap().final_layout();
    let prose = doc.query_selector("#prose").unwrap().unwrap();
    let block = doc.get_node(prose).unwrap().final_layout();
    (
        node.location.x + node.size.width,
        block.location.x + block.size.width,
    )
}

fn assert_inside(doc: &HtmlDocument, selector: &str, when: &str) {
    let (right, block_right) = edges(doc, selector);
    assert!(
        right <= block_right + 0.5,
        "{selector} ends at {right}, past the block's {block_right}, {when}"
    );
}

/// Laid out narrow from the start. The control: if this fails, inline boxes are
/// wrong everywhere and the resize is not the subject.
#[test]
fn inline_elements_sit_inside_the_block_when_laid_out_narrow() {
    let doc = document(NARROW);
    assert_inside(&doc, "#chip", "laid out narrow from the start");
    assert_inside(&doc, "#code", "laid out narrow from the start");
}

/// The reported case. Lay out wide, then narrow the viewport the way hiding the
/// project sidebar narrows the transcript, and resolve again.
#[test]
fn inline_elements_follow_their_line_when_the_pane_narrows() {
    let mut doc = document(WIDE);
    let (wide_right, _) = edges(&doc, "#chip");

    doc.set_viewport(Viewport::new(NARROW, 900, 1.0, ColorScheme::Light));
    doc.resolve(0.0);

    let (narrow_right, block_right) = edges(&doc, "#chip");
    assert!(
        narrow_right != wide_right,
        "the chip did not move at all when the pane went from {WIDE} to {NARROW}"
    );
    assert!(
        narrow_right <= block_right + 0.5,
        "chip ends at {narrow_right}, past the block's {block_right}, after narrowing"
    );
    assert_inside(&doc, "#code", "after narrowing");
}

/// And back out again, because a stale box that happens to fit is still stale.
#[test]
fn inline_elements_follow_their_line_when_the_pane_widens() {
    let mut doc = document(NARROW);
    doc.set_viewport(Viewport::new(WIDE, 900, 1.0, ColorScheme::Light));
    doc.resolve(0.0);
    assert_inside(&doc, "#chip", "after widening");
    assert_inside(&doc, "#code", "after widening");
}

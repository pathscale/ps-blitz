//! A scroll offset may not outlive the content it was valid for.
//!
//! Scroll offsets are clamped when a scroll *happens*, against the layout as
//! it stood at that moment. Nothing re-checks them when a later relayout makes
//! the content shorter, so an offset that was legal keeps its value and the
//! viewport ends up parked past the end of the document, looking at nothing.
//!
//! Measured on AgencyZero's settings pane: `scroll=8170` against `content=7751`
//! and `client=708`, so the maximum legal offset was 7043 and the viewport was
//! showing y 8170..8878 of a document that ended at 7751. **1,127px of pure
//! emptiness.** The pane rendered as a blank rectangle inside correctly painted
//! chrome, at 89fps and 3.4ms a frame, because there was genuinely nothing
//! there to draw.
//!
//! It reads as "the app went blank" and it recovers on any scroll, because a
//! scroll re-runs the clamp. That is also why it looked like a missing repaint
//! for so long: every renderer metric is healthy while it happens.
//!
//! `clamp_text_input_scroll` already does exactly this job for text inputs.
//! Generic scrollers had no equivalent.

use blitz_dom::{Document, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    node_id::NodeId,
    shell::{ColorScheme, Viewport},
};
use std::sync::Arc;

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;

/// A scroller whose content height is driven by a class on the body.
fn doc_with_tail(tail_height: u32) -> HtmlDocument {
    let html = format!(
        r#"<html><head><style>
          body {{ margin: 0; }}
          #scroller {{ height: 200px; overflow-y: scroll; }}
          #tail {{ height: {tail_height}px; }}
        </style></head>
        <body>
          <div id="scroller">
            <div id="head">head</div>
            <div id="tail">tail</div>
          </div>
        </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn scroller_id(doc: &HtmlDocument) -> NodeId {
    doc.inner()
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller")
}

/// `(offset, max legal offset)` for the scroller.
fn offsets(doc: &HtmlDocument, id: NodeId) -> (f64, f64) {
    let inner = doc.inner();
    let node = inner.get_node(id).unwrap();
    let layout = node.final_layout();
    let max = (layout.scroll_height() as f64).max(0.0);
    (node.scroll_offset().y, max)
}

/// Scroll to the very bottom of a tall document, then make it short.
///
/// The offset has to come back inside the new bounds. Left alone it strands
/// the viewport past the end and the pane paints nothing at all.
#[test]
fn an_offset_is_clamped_when_a_relayout_shrinks_the_content() {
    let mut doc = doc_with_tail(4000);
    let id = scroller_id(&doc);

    // All the way down, which is legal at this height.
    doc.inner_mut().scroll_node_by(id, 0.0, -100_000.0, |_| {});
    let (parked, max_before) = offsets(&doc, id);
    assert!(
        parked > 0.0 && parked <= max_before + 1.0,
        "fixture did not scroll to the bottom: {parked} of {max_before}"
    );

    // The content shrinks under it, as a settings pane does when a panel
    // collapses or a layout change makes the column shorter.
    {
        let tail = doc.inner().query_selector("#tail").unwrap().expect("#tail");
        let mut mutator = doc.mutate();
        mutator.set_attribute(
            tail,
            QualName {
                prefix: None,
                ns: ns!(),
                local: "style".into(),
            },
            "height: 100px",
        );
    }
    doc.inner_mut().resolve(0.0);

    let (offset, max_after) = offsets(&doc, id);
    assert!(
        max_after < parked,
        "fixture did not actually shrink: max {max_after} vs old offset {parked}"
    );
    assert!(
        offset <= max_after + 1.0,
        "the scroller is parked {:.0}px past the end of its own content \
         (offset {offset}, max {max_after}); the viewport shows empty space \
         and the pane paints nothing",
        offset - max_after
    );
}

/// The offset that was already legal is left exactly where it was. A clamp
/// that also *moves* a valid offset would scroll the page under the reader
/// every time anything relaid out.
#[test]
fn a_valid_offset_is_left_alone_by_a_relayout() {
    let mut doc = doc_with_tail(4000);
    let id = scroller_id(&doc);

    doc.inner_mut().scroll_node_by(id, 0.0, -300.0, |_| {});
    let (before, _) = offsets(&doc, id);
    assert!(before > 0.0, "fixture did not scroll at all");

    doc.inner_mut().resolve(0.0);

    let (after, _) = offsets(&doc, id);
    assert_eq!(after, before, "a relayout moved an offset that was fine");
}

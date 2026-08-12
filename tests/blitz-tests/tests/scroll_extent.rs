//! A scroller's reported extent must reach the bottom of its content.
//!
//! Reported as transcripts not pinning to the bottom. The application scrolls
//! with the usual `el.scrollTop = el.scrollHeight` idiom, so an extent that
//! under-reports leaves the view short of the end by exactly the shortfall:
//! measured live as content running 1,062px past the visible bottom with
//! `scrollTop` already at the reported maximum.
//!
//!   cargo test --release -p blitz-tests --test scroll_extent -- --nocapture

use blitz_dom::{Document as _, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;
const VIEW: f32 = 400.0;

fn document(inner: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0">
             <div id="scroller" style="height:{VIEW}px; width:{WIDTH}px; overflow-y:auto;
                                       display:flex; flex-direction:column; gap:16px;
                                       padding-top:56px; padding-bottom:8px;">
               {inner}
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

/// (reported max scroll, real content height, view height)
fn extents(doc: &HtmlDocument) -> (f32, f32, f32) {
    let id = doc.query_selector("#scroller").unwrap().unwrap();
    let node = doc.get_node(id).unwrap();
    let layout = node.final_layout();
    let reported = layout.scroll_height();

    // What is actually in there: the bottom of the last child, plus the
    // scroller's bottom padding.
    let last = node
        .children
        .iter()
        .filter_map(|child| doc.get_node(*child))
        .filter(|child| child.is_element())
        .map(|child| {
            let l = child.final_layout();
            l.location.y + l.size.height
        })
        .fold(0.0f32, f32::max);
    let content = last + layout.padding.bottom;

    (reported, content, layout.size.height)
}

/// Blocks of prose, which is what a transcript is.
#[test]
fn a_scroller_reports_an_extent_that_reaches_its_content() {
    let paragraphs: String = (0..12)
        .map(|i| {
            format!(
                r#"<div style="margin-bottom:16px;"><p>Message {i}. Perfect, proceed with the
                   backup here and restore there, and I will keep monitoring the active
                   descriptor so that nothing is lost between one replacement and the next
                   one that comes back after it.</p></div>"#
            )
        })
        .collect();
    let doc = document(&paragraphs);

    let (reported, content, view) = extents(&doc);
    let expected = (content - view).max(0.0);
    println!(
        "\n  reported max scroll {reported:.1}, content {content:.1}, view {view:.1}, \
         expected {expected:.1}\n"
    );

    assert!(
        (reported - expected).abs() < 1.0,
        "the scroller reports {reported:.1} of scrollable height where its content \
         needs {expected:.1}: scrolling to the reported maximum leaves the last \
         {:.1}px unreachable",
        expected - reported
    );
}

//! Replacing an element's text must replace what is painted.
//!
//! Two reports of text drawn on top of other text, both on elements whose
//! content changes in place: a live cost readout, and a truncated branch name.
//! The suspicion is a stale inline layout — the shaped glyphs of the old text
//! still being painted under the new ones.
//!
//! The second case is the one this change made possible: text that is replaced
//! while its pane is hidden. A hidden pane keeps its boxes now, so nothing
//! forces the reshape that hiding used to guarantee.
//!
//!   cargo test --release -p blitz-tests --test text_update_repaint -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 300;
const HEIGHT: u32 = 100;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// How much ink is on the page. Text painted over text is strictly more ink
/// than either alone, which is the whole signal here: it needs no knowledge of
/// where the glyphs land.
fn ink(doc: &mut HtmlDocument) -> usize {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    buffer
        .chunks_exact(4)
        .filter(|px| px[0] < 200 && px[1] < 200 && px[2] < 200)
        .count()
}

fn set_text(doc: &mut HtmlDocument, selector: &str, text: &str) {
    let inner = &mut *doc.inner_mut();
    let node = inner.query_selector(selector).unwrap().unwrap();
    let child = inner.get_node(node).unwrap().children[0];
    let mut mutator = inner.mutate();
    mutator.set_node_text(child, text);
    drop(mutator);
    inner.resolve(0.0);
}

fn set_class(doc: &mut HtmlDocument, selector: &str, class: &str) {
    let inner = &mut *doc.inner_mut();
    let node = inner.query_selector(selector).unwrap().unwrap();
    let mut mutator = inner.mutate();
    mutator.set_attribute(node, attr_name("class"), class);
    drop(mutator);
    inner.resolve(0.0);
}

const PAGE: &str = r#"<html><head><style>
      .hidden { display: none }
      .shown { display: block }
    </style></head>
    <body style="margin:0; background:#ffffff; font-size:16px; color:#000000">
      <div id="pane" class="shown"><span id="readout">1.77s calc $1.56</span></div>
    </body></html>"#;

/// The control: replacing text in a visible pane.
#[test]
fn replacing_text_in_a_visible_pane_repaints_it() {
    let mut doc = document(PAGE);
    let before = ink(&mut doc);
    assert!(before > 0, "fixture: the readout paints");

    set_text(&mut doc, "#readout", "0");
    let after = ink(&mut doc);

    assert!(
        after < before,
        "a shorter string painted at least as much ink as the long one it \
         replaced ({after} vs {before}): the old glyphs are still being painted"
    );
}

/// The case a retained tab makes possible: the text changes while the pane is
/// hidden, so nothing about the reveal forces a reshape.
#[test]
fn replacing_text_while_hidden_repaints_on_reveal() {
    let mut doc = document(PAGE);
    let long = ink(&mut doc);

    set_class(&mut doc, "#pane", "hidden");
    assert_eq!(ink(&mut doc), 0, "a hidden pane paints nothing");

    set_text(&mut doc, "#readout", "0");
    set_class(&mut doc, "#pane", "shown");

    let after = ink(&mut doc);
    assert!(after > 0, "the revealed pane paints something");
    assert!(
        after < long,
        "text replaced while the pane was hidden came back painted over its own \
         old glyphs ({after} vs {long} for the longer original)"
    );
}

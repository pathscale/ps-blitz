//! A `<select>` has to get a layout box.
//!
//! There was no `select` rule in the user-agent stylesheet, so a select
//! computed `display: inline`, and `option { display: none }` left it with no
//! in-flow content. Height on a non-replaced inline is ignored, so even
//! `<select style="height:34px">` reported 0x0 and nothing on the page could
//! find it, let alone press it.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn doc(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn size_of(doc: &HtmlDocument, selector: &str) -> (f32, f32) {
    let node_id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    let layout = doc.get_node(node_id).unwrap().final_layout();
    (layout.size.width, layout.size.height)
}

#[test]
fn an_authored_select_size_is_honoured() {
    let doc = doc(r#"<html><body style="margin:0">
            <select id="country" style="box-sizing:border-box; width:120px; height:34px">
                <option value="us">United States</option>
                <option value="fr" selected>France</option>
            </select>
        </body></html>"#);

    assert_eq!(
        size_of(&doc, "#country"),
        (120.0, 34.0),
        "a select must take an authored width and height, which needs a block-level box"
    );
}

#[test]
fn a_bare_select_still_gets_a_box() {
    let doc = doc(r#"<html><body style="margin:0; line-height:normal">
            <select id="country">
                <option value="us">United States</option>
                <option value="fr">France</option>
            </select>
        </body></html>"#);

    let (width, height) = size_of(&doc, "#country");
    assert!(
        width > 0.0 && height > 0.0,
        "a select with no authored size must still be hittable, got {width}x{height}"
    );
}

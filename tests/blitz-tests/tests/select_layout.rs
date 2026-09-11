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

#[test]
fn a_labelled_option_is_measured_by_its_label() {
    // The `label` attribute is what the control shows; the element's text is
    // not displayed at all. Measuring the text sized the box to a string the
    // reader never sees.
    let doc = doc(r#"<html><body style="margin:0; line-height:normal">
            <select id="short"><option label="X">A considerably longer hidden string</option></select>
            <select id="long"><option>A considerably longer hidden string</option></select>
        </body></html>"#);

    let (short_width, _) = size_of(&doc, "#short");
    let (long_width, _) = size_of(&doc, "#long");
    assert!(
        short_width < long_width / 4.0,
        "the labelled select must be sized from its one-character label, got {short_width} against {long_width}"
    );
}

#[test]
fn an_option_written_across_several_lines_is_not_measured_wider_for_it() {
    // The whitespace inside the label is collapsed away when it is rendered,
    // so it must not be counted. `trim()` only strips the ends: a label whose
    // own words are split across lines kept the newline and the run of leading
    // spaces before the next word, and the control came out that much wider.
    let indented = doc(r#"<html><body style="margin:0; line-height:normal">
            <select id="country">
                <option value="us">United
                    States</option>
            </select>
        </body></html>"#);
    let inline = doc(r#"<html><body style="margin:0; line-height:normal">
            <select id="country"><option value="us">United States</option></select>
        </body></html>"#);

    assert_eq!(
        size_of(&indented, "#country").0,
        size_of(&inline, "#country").0,
        "how the markup is formatted must not change how wide the control is"
    );
}

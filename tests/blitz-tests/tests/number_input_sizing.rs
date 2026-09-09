//! `input[type=number]` gets a text editor like every other text-like input, so
//! it has to get the same intrinsic content box. It did not: the type list in
//! `layout::construct` included `number` and the one in `layout::mod` did not,
//! so a bare number input measured its padding and border and nothing else.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn size_of(doc: &HtmlDocument, selector: &str) -> (f32, f32) {
    let node_id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    let layout = doc.get_node(node_id).unwrap().final_layout();
    (layout.size.width, layout.size.height)
}

#[test]
fn a_number_input_is_sized_like_the_other_text_inputs() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0; line-height:normal">
            <div><input id="text" type="text"></div>
            <div><input id="number" type="number"></div>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let text = size_of(&doc, "#text");
    let number = size_of(&doc, "#number");

    assert!(
        number.0 > 100.0 && number.1 > 10.0,
        "a number input collapsed to its padding and border: {number:?}"
    );
    assert_eq!(
        number, text,
        "a number input must measure the same as a text input"
    );
}

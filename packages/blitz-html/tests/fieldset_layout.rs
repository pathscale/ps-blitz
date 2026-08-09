//! HTML fieldsets use block layout by default, so authored dimensions apply.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

#[test]
fn fieldset_honors_authored_width_and_height_without_an_explicit_display() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0">
            <fieldset id="wheel"
                      style="position:relative; margin:0; width:190px; height:190px;
                             padding:0; border:0">
                <label style="position:absolute; width:28px; height:28px"></label>
            </fieldset>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let fieldset = doc
        .query_selector("#wheel")
        .unwrap()
        .expect("fieldset not found");
    let layout = doc.get_node(fieldset).unwrap().final_layout;
    assert_eq!(
        (layout.size.width, layout.size.height),
        (190.0, 190.0),
        "the user-agent display must let fieldset dimensions participate in layout"
    );
}

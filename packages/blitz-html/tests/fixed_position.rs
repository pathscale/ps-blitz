//! Fixed-position boxes use the viewport as their containing block, even when
//! they are mounted beneath ordinary flow content.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

#[test]
fn fixed_descendant_of_flow_content_is_viewport_relative() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0">
            <div style="height:900px"></div>
            <div id="mount">
                <div id="overlay" style="position:fixed;top:0;left:0;width:100vw;height:100vh"></div>
            </div>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let overlay = doc
        .query_selector("#overlay")
        .unwrap()
        .expect("#overlay not found");
    let rect = doc
        .get_client_bounding_rect(overlay)
        .expect("#overlay has no client rect");

    assert_eq!((rect.x, rect.y), (0.0, 0.0));
    assert_eq!((rect.width, rect.height), (1344.0, 900.0));
}

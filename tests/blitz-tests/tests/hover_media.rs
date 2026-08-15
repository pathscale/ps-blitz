use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn pixel(doc: &mut HtmlDocument) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, 200, 100, 0, 0),
        200,
        100,
    );
    [
        buffer[40 * 200 * 4 + 40 * 4],
        buffer[40 * 200 * 4 + 40 * 4 + 1],
        buffer[40 * 200 * 4 + 40 * 4 + 2],
    ]
}

#[test]
fn desktop_hover_media_rule_repaints_a_nested_button() {
    let html = r#"<html><head><style>
      body { margin: 0; background: white }
      button { width: 80px; height: 80px; background: #0000ff }
      @media (hover: hover) { button:hover { background: #ff0000 } }
    </style></head><body><button><span>close</span></button></body></html>"#;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    assert_eq!(pixel(&mut doc), [0, 0, 255]);
    doc.set_hover_to(40.0, 40.0);
    doc.resolve(0.0);
    assert_eq!(pixel(&mut doc), [255, 0, 0]);
}

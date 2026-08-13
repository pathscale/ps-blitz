use std::sync::Arc;

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};

#[test]
fn inset_fifty_percent_hides_a_native_checkbox() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0;background:#ff0000">
          <input type="checkbox" checked style="position:absolute;width:40px;height:40px;clip-path:inset(50%)">
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(80, 80, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, 80, 80, 0, 0),
        80,
        80,
    );
    for pixel in buffer.chunks_exact(4) {
        assert_eq!(&pixel[..3], &[255, 0, 0]);
    }
}

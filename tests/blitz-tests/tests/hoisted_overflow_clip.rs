//! Positioned descendants must retain every ancestor overflow clip after they
//! are hoisted into a stacking context for paint ordering.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 240;
const HEIGHT: u32 = 180;

fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

#[test]
fn z_index_child_does_not_escape_an_overflow_hidden_panel() {
    let html = r#"<html><body style="margin:0;background:#fff">
      <div style="position:relative;width:120px;height:60px;overflow:hidden;background:#008000">
        <button style="position:absolute;z-index:10;left:20px;top:90px;width:40px;height:30px;background:#ff0000"></button>
      </div>
    </body></html>"#;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    assert_eq!(pixel_at(&mut doc, 30, 30), [0, 128, 0], "fixture panel");
    assert_eq!(
        pixel_at(&mut doc, 30, 100),
        [255, 255, 255],
        "a z-index child painted outside its overflow-hidden panel",
    );
}

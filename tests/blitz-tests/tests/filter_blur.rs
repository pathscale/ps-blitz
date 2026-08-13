//! `filter: blur()` on an element's own content.
//!
//! Distinct from `backdrop-filter`, and the distinction is the whole point:
//! this one is implemented all the way down. `anyrender_vello_cpu` maps
//! `anyrender::Filter` onto `vello_common::filter_effects::FilterPrimitive`
//! (`filters.rs`), and `vello_cpu` applies it in `push_layer`. It was invisible
//! only because the `filters` cargo feature was never enabled by any consumer.
//!
//! `backdrop-filter` is the one still missing, and it is missing lower down:
//! its input is `FilterSource::BackgroundImage`, which appears exactly once in
//! all of vello — as an enum variant, referenced by nothing.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 160;

fn render(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

fn pixel_at(buffer: &[u8], x: u32, y: u32) -> [u8; 3] {
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

fn fixture(style: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0;background:#fff">
          <div style="position:absolute;left:40px;top:40px;width:80px;height:80px;
                      background:#000;{style}"></div>
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

/// Unfiltered, the square's edge is a step: black inside, white outside.
#[test]
fn an_unfiltered_square_has_a_hard_edge() {
    let buffer = render(&mut fixture(""));
    assert_eq!(pixel_at(&buffer, 60, 38), [255, 255, 255], "just outside");
    assert_eq!(pixel_at(&buffer, 60, 42), [0, 0, 0], "just inside");
}

/// Blurred, that step becomes a ramp: both samples land between the two.
#[test]
fn filter_blur_softens_the_edge() {
    let buffer = render(&mut fixture("filter:blur(6px)"));

    let outside = pixel_at(&buffer, 60, 38);
    let inside = pixel_at(&buffer, 60, 42);
    assert!(
        outside[0] < 255,
        "just outside the edge should darken under blur, got {outside:?}"
    );
    assert!(
        inside[0] > 0,
        "just inside the edge should lighten under blur, got {inside:?}"
    );
}

//! `appearance: none` must suppress the native control painting.
//!
//! Component libraries hide the real `<input type="checkbox">` and draw their
//! own track and thumb. The standard way to say so is `appearance: none`, and
//! the input is then shrunk to about a pixel and parked beside the control it
//! backs.
//!
//! `blitz-paint` never reads `appearance`, so `draw_checkbox` fills the border
//! box with the accent colour whenever the box is checked. Driving AgencyZero's
//! Settings, that painted a ~1px accent square one pixel to the left of a
//! toggle's rounded track: the input sat at x=997 with the track spanning
//! 998..1044, and only the checked toggles showed it, because the unchecked
//! branch fills nothing.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 120;
const HEIGHT: u32 = 80;

fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
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

/// The control the library asked not to be drawn must not be drawn.
#[test]
fn appearance_none_suppresses_the_native_checkbox() {
    // `color` is what `draw_checkbox` uses as its accent, so red here means any
    // native painting is unmistakable against the white page.
    let mut doc = document(
        r#"<html><body style="margin:0;background:#fff">
        <input type="checkbox" checked style="appearance:none;position:absolute;
               left:20px;top:20px;width:20px;height:20px;margin:0;color:#ff0000">
        </body></html>"#,
    );

    assert_eq!(
        pixel_at(&mut doc, 5, 5),
        [255, 255, 255],
        "fixture background"
    );
    assert_eq!(
        pixel_at(&mut doc, 30, 30),
        [255, 255, 255],
        "a checkbox with appearance:none painted its native control anyway",
    );
}

/// The counterpart, so a fix cannot simply stop drawing checkboxes: without
/// `appearance: none` the native control is still expected to paint.
#[test]
fn a_default_checkbox_still_paints() {
    let mut doc = document(
        r#"<html><body style="margin:0;background:#fff">
        <input type="checkbox" checked style="position:absolute;
               left:20px;top:20px;width:20px;height:20px;margin:0;color:#ff0000">
        </body></html>"#,
    );

    assert_ne!(
        pixel_at(&mut doc, 30, 30),
        [255, 255, 255],
        "a checkbox with no appearance override should still draw",
    );
}

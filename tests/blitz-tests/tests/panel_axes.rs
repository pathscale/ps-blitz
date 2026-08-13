//! The two glass axes that computed style cannot answer for.
//!
//! AgencyZero drives a panel's edge and depth from custom properties:
//!
//! ```css
//! outline: 1px solid rgb(from var(--color-primary) r g b / var(--az-glass-border, 16%));
//! box-shadow: 0 8px 32px rgb(0 0 0 / var(--az-glass-shadow, 0));
//! ```
//!
//! The debug driver's `getComputedStyle` returns only a handful of properties,
//! and neither of these is among them, so the only honest check is pixels.
//! Each axis is asserted twice: at its default it must paint nothing, and when
//! moved it must paint — otherwise a slider that writes a token but changes no
//! frame reads as working while doing nothing.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 200;

fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

/// A panel on a white page, with the two axes set from custom properties.
fn panel(border_alpha: &str, shadow_alpha: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0;background:#fff">
          <div style="--az-glass-border:{border_alpha};--az-glass-shadow:{shadow_alpha};
                      position:absolute;left:60px;top:60px;width:80px;height:80px;
                      background:#101010;
                      outline:2px solid rgb(255 0 0 / var(--az-glass-border));
                      outline-offset:-2px;
                      box-shadow:0 8px 32px rgb(0 0 0 / var(--az-glass-shadow));"></div>
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

/// Sampled *inside* the panel's own edge, because the fixture asks for
/// `outline-offset: -2px`.
///
/// This used to sample outside, at (59, 59), and pass: `outline-offset` was
/// read by nobody, so a `-2px` probe painted in exactly the same place as one
/// with no offset at all and every inset ring landed outside its box. That is
/// fixed (see `outline_offset.rs`), so the ring is now where the stylesheet
/// asked for it and the sample follows it in. AgencyZero's `.rounded-panel`
/// asks for the same inset pixel.
///
/// At rest the ring is transparent and the panel's own background shows through
/// at that pixel, which is why the off case is the panel colour and not the
/// page.
#[test]
fn the_border_axis_paints_only_when_raised() {
    const PANEL_FILL: [u8; 3] = [0x10, 0x10, 0x10];

    // At 0% the outline is fully transparent, so the panel shows through.
    let mut off = panel("0%", "0");
    let at_edge = pixel_at(&mut off, 61, 100);
    assert_eq!(
        at_edge, PANEL_FILL,
        "a 0% edge should paint nothing over the panel"
    );

    // Raised, the outline colour has to appear in the same place.
    let mut on = panel("100%", "0");
    let raised = pixel_at(&mut on, 61, 100);
    assert!(
        raised[0] > 150 && raised[1] < 90,
        "a raised edge should paint the outline colour, got {raised:?}"
    );

    // And nothing should escape the box, which is what the negative offset buys.
    let outside = pixel_at(&mut on, 59, 100);
    assert_eq!(
        outside,
        [255, 255, 255],
        "an inset edge must not paint outside the panel"
    );
}

/// The shadow falls below the panel: `0 8px 32px` puts it under the bottom edge.
#[test]
fn the_depth_axis_paints_only_when_raised() {
    // Default is 0 alpha — the page stays white below the panel.
    let mut off = panel("16%", "0");
    let below = pixel_at(&mut off, 100, 152);
    assert_eq!(
        below,
        [255, 255, 255],
        "a 0 depth should leave the page untouched below the panel"
    );

    let mut on = panel("16%", "0.6");
    let shadowed = pixel_at(&mut on, 100, 152);
    assert!(
        shadowed[0] < 250,
        "a raised depth should darken below the panel, got {shadowed:?}"
    );
}

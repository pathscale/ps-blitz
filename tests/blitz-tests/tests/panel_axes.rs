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

/// Sampled just *outside* the panel, not inside it.
///
/// `outline-offset` is ignored by this renderer: a probe with `-2px` painted in
/// exactly the same place as one with no offset at all. So the outline always
/// lands outside the border box, and AgencyZero's `.rounded-panel`, which asks
/// for `outline-offset: -1px`, gets an outline sitting a pixel outside rather
/// than inset. Worth fixing in the engine; irrelevant to whether the axis works.
#[test]
fn the_border_axis_paints_only_when_raised() {
    // At 0% the outline is fully transparent, so the page shows through.
    let mut off = panel("0%", "0");
    let at_edge = pixel_at(&mut off, 59, 59);
    assert_eq!(
        at_edge,
        [255, 255, 255],
        "a 0% edge should paint nothing outside the panel"
    );

    // Raised, the outline colour has to appear in the same place.
    let mut on = panel("100%", "0");
    let raised = pixel_at(&mut on, 59, 59);
    assert!(
        raised[0] > 150 && raised[1] < 90,
        "a raised edge should paint the outline colour, got {raised:?}"
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

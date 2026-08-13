//! Does `backdrop-filter` actually blur what is behind it?
//!
//! `blitz-paint` threads a `backdrop_filter` through its layer stack
//! (`layers.rs`, `render.rs`), which says the property is parsed and carried.
//! It does not say pixels change. Glass support needs the answer before anyone
//! plans around it, and the difference is a wiring job versus an implementation
//! job — so this asks in pixels.
//!
//! The fixture is a hard black/white edge with a panel over the boundary. Blur
//! samples both sides, so a working implementation cannot leave either pixel
//! pure: over the white half it must darken, over the black half it must
//! lighten. A no-op leaves both exactly as they were.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 120;

fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

/// Half black, half white, with a transparent blurring panel across the seam.
fn fixture(panel_style: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0;background:#fff">
          <div style="position:absolute;left:0;top:0;width:100px;height:120px;background:#000"></div>
          <div style="position:absolute;left:40px;top:30px;width:120px;height:60px;{panel_style}"></div>
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

/// The control: with no panel at all, the seam is exactly black and white.
#[test]
fn the_fixture_has_a_hard_edge_without_a_panel() {
    let mut doc = fixture("");
    assert_eq!(pixel_at(&mut doc, 60, 60), [0, 0, 0], "left of the seam");
    assert_eq!(
        pixel_at(&mut doc, 140, 60),
        [255, 255, 255],
        "right of the seam"
    );
}

#[test]
fn backdrop_filter_blur_mixes_both_sides_of_the_edge() {
    let mut doc = fixture("backdrop-filter:blur(12px)");

    // Close to the seam on purpose. blur(12px) reaches roughly three standard
    // deviations, so a sample 40px away is legitimately still pure black and
    // says nothing — an earlier version of this test read that as a failure.
    let dark_side = pixel_at(&mut doc, 95, 60);
    let light_side = pixel_at(&mut doc, 105, 60);

    assert!(
        dark_side[0] > 0,
        "over the black half the blur should pull in white, got {dark_side:?}"
    );
    assert!(
        light_side[0] < 255,
        "over the white half the blur should pull in black, got {light_side:?}"
    );
}

/// The blur must be a gradient across the seam, not a single perturbed pixel.
///
/// A one-pixel assertion passes for anything that merely disturbs the output.
/// Sampling a run across the boundary asks the stronger question: does it fall
/// monotonically from the light side to the dark one, the way a gaussian over a
/// step edge has to?
#[test]
fn backdrop_blur_ramps_monotonically_across_the_seam() {
    let mut doc = fixture("backdrop-filter:blur(12px)");

    // Left to right across the seam at x=100, all inside the panel.
    let samples: Vec<u8> = (88..=112)
        .step_by(4)
        .map(|x| pixel_at(&mut doc, x, 60)[0])
        .collect();

    assert!(
        samples.windows(2).all(|pair| pair[1] >= pair[0]),
        "expected a monotonic ramp from dark to light across the seam, got {samples:?}"
    );
    assert!(
        samples.first() < samples.last(),
        "the ramp should actually rise, got {samples:?}"
    );
    assert!(
        samples.iter().any(|v| *v > 0 && *v < 255),
        "a blur must produce intermediate values, got {samples:?}"
    );
}

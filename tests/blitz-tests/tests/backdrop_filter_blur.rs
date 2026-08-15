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

/// Whether this build can apply filters at all.
///
/// `anyrender_vello_cpu` skips every filter when its `multithreading` feature
/// is compiled in, because `RenderContext::new` takes default settings, those
/// select the multi-threaded dispatcher, and that dispatcher answers a filter
/// with `unimplemented!("Filter effects are not yet supported in
/// multi-threaded rendering")`. Skipping is the right call: the alternative is
/// a panic mid-frame.
///
/// It matters here because cargo unifies features across a workspace build.
/// `cargo test -p blitz-tests` gets `filters` only, and blurs.
/// `cargo test --workspace` also picks up `multithreading` from the root
/// manifest, and does not. Same test, same machine, two answers, and CI runs
/// the second one. This test asserted the first for three days without anyone
/// seeing it, because the branch had not been pushed.
///
/// Probed rather than read off a `cfg!`, because a `cfg!(feature = ...)` here
/// would name *this* crate's features, not `anyrender_vello_cpu`'s.
///
/// The probe is the plain `filter` path, not `backdrop-filter`, so it does not
/// assume the answer to the question the tests below ask. One guard governs
/// both, so either both work or neither does.
fn filters_are_active() -> bool {
    let mut probe = fixture("filter:blur(12px);background:#f00");
    // A solid red box blurred at its own edge: if filters run at all, the
    // panel's edge is soft and this pixel is not the untouched backdrop.
    pixel_at(&mut probe, 41, 60) != pixel_at(&mut probe, 60, 60)
}

/// What this build actually does, stated out loud so a green run still says
/// which of the two configurations it was.
fn require_filters(test: &str) -> bool {
    if filters_are_active() {
        return true;
    }
    eprintln!(
        "{test}: filters are compiled out of this build \
         (anyrender_vello_cpu + multithreading), so backdrop-filter is a \
         documented no-op here. Asserting the no-op instead."
    );
    false
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

    if !require_filters("backdrop_filter_blur_mixes_both_sides_of_the_edge") {
        // The no-op is asserted exactly, not skipped. A build that half-applies
        // the filter, or that starts applying it, changes these and says so.
        assert_eq!(dark_side, [0, 0, 0], "the no-op must leave black alone");
        assert_eq!(
            light_side,
            [255, 255, 255],
            "the no-op must leave white alone"
        );
        return;
    }

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

    if !require_filters("backdrop_blur_ramps_monotonically_across_the_seam") {
        // Still a step edge, with no intermediate value anywhere: the exact
        // shape a no-op leaves behind.
        assert!(
            samples.iter().all(|v| *v == 0 || *v == 255),
            "the no-op must leave a hard step, got {samples:?}"
        );
        return;
    }

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

//! A caret belongs to its input, and its input can be clipped.
//!
//! The caret and selection fills passed `None` for their clip, so they painted
//! unbounded: an input taller than a clipping ancestor drew its caret outside
//! that ancestor, over whatever happened to be there.
//!
//! Found in AgencyZero as "a little blinking thing" with nothing logical near
//! it. The prompt viewport is `overflow: hidden` with a JS-measured height,
//! and its textarea keeps its own `rows` height, so on a fresh launch the
//! textbox measured 836x44 inside an 836x26 parent at the same origin. The
//! 18px that did not fit were painted anyway, blinking on the caret's own
//! 500ms clock, and they appeared in both the stable and experimental builds.
//!
//! The app-side sizing disagreement is a separate bug. This one is the
//! renderer's: whatever an input's box says, a caret must not escape a clip
//! its input is subject to.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene_at_time;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const SIZE: u32 = 120;

/// A 40px input inside a 16px clipping wrapper, both at the same origin.
///
/// The caret is black on white and the wrapper clips at y=16, so anything
/// painted below that row is the caret escaping. `caret-color` is forced so
/// the test does not depend on the default text colour.
const HTML: &str = r#"<html><body style="margin:0; background:#ffffff">
    <div style="width:120px; height:16px; overflow:hidden">
      <textarea autofocus rows="8" style="width:120px; height:100px; border:0;
                padding:0; margin:0; background:#ffffff; caret-color:#000000;
                color:#000000; font-size:12px"></textarea>
    </div>
  </body></html>"#;

/// Render at a time when the caret is in its visible half.
///
/// `paint_scene_at_time` takes the animation clock the blink is derived from:
/// `(animation_time % 1.0) < 0.5` is the on phase, so 0.1 is solidly inside it.
fn render_with_caret_shown() -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(SIZE, SIZE, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_at_time(scene, &mut doc.inner_mut(), 1.0, SIZE, SIZE, 0, 0, 0.1),
        SIZE,
        SIZE,
    )
}

/// Darkest pixel in a row, as a rough "is anything drawn here" probe.
fn darkest_in_row(buffer: &[u8], y: u32) -> u8 {
    (0..SIZE)
        .map(|x| {
            let i = ((y * SIZE + x) * 4) as usize;
            buffer[i].min(buffer[i + 1]).min(buffer[i + 2])
        })
        .min()
        .unwrap_or(255)
}

#[test]
fn a_caret_does_not_paint_below_a_clipping_ancestor() {
    let buffer = render_with_caret_shown();

    // Well below the 16px clip, inside the input's own 40px box. Nothing the
    // wrapper clips may appear here.
    for y in [20u32, 40, 60, 80] {
        let darkest = darkest_in_row(&buffer, y);
        assert!(
            darkest > 200,
            "row {y} is below the 16px clip but something dark was painted there \
             (darkest channel {darkest}); the caret escaped its ancestor's overflow"
        );
    }
}

/// The clip must not swallow the caret entirely either: inside the wrapper it
/// still has to draw, or this test would pass against a renderer that simply
/// stopped painting carets.
#[test]
fn a_caret_still_paints_inside_the_clip() {
    let buffer = render_with_caret_shown();

    let darkest = (0..16u32)
        .map(|y| darkest_in_row(&buffer, y))
        .min()
        .unwrap_or(255);
    assert!(
        darkest < 200,
        "no caret was painted inside the 16px wrapper at all (darkest channel \
         {darkest}); the fixture or the blink phase is wrong, not the clip"
    );
}

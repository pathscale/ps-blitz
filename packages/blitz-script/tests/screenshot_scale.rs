//! A screenshot is painted at the scale the document was laid out for.
//!
//! It used to be painted at a hardcoded 1.0, so a HiDPI window came back as a
//! CSS-pixel image of a document laid out for twice that: legible, but half the
//! resolution the window actually renders at, and not the picture anyone means
//! when they ask for a screenshot of it.
//!
//! These also pin down what the scale does *not* do, which is the more useful
//! half. Glyph size does not follow it: a document laid out at 2.0 and painted
//! at 1.0 keeps its text inside its boxes. That was worth establishing, because
//! text overflowing its boxes in a screenshot of a HiDPI window looks exactly
//! like a scale mismatch and is not one.

#![cfg(feature = "debug-control")]

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document as _, DocumentConfig};
use blitz_paint::paint_scene;
use blitz_script::ScriptDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

/// A line of text in a box exactly as tall as the line, on white.
///
/// `overflow: visible`, so glyphs drawn too large spill below the box instead
/// of being clipped into looking correct.
const HTML: &str = r#"<!doctype html><html><body style="margin:0;background:#fff">
<div style="width:200px;height:24px;font:16px/24px monospace;color:#000">Hgjy</div>
<div style="width:400px;height:376px;background:#fff"></div>
</body></html>"#;

const CSS_WIDTH: u32 = 400;
const CSS_HEIGHT: u32 = 400;
/// The first line's box. Ink below this row is a glyph that outgrew it.
const LINE_HEIGHT: u32 = 24;

/// Paint the document, laid out at `layout_scale`, using `paint_scale`.
fn ink_below_the_line(layout_scale: f32, paint_scale: f64) -> usize {
    let mut document = ScriptDocument::from_html(HTML, DocumentConfig::default());
    let (width, height) = (
        (f64::from(CSS_WIDTH) * paint_scale).round() as u32,
        (f64::from(CSS_HEIGHT) * paint_scale).round() as u32,
    );
    {
        let mut inner = document.inner_mut();
        inner.set_viewport(Viewport::new(
            (f64::from(CSS_WIDTH) * f64::from(layout_scale)).round() as u32,
            (f64::from(CSS_HEIGHT) * f64::from(layout_scale)).round() as u32,
            layout_scale,
            ColorScheme::Light,
        ));
        inner.resolve(0.0);
    }
    let mut inner = document.inner_mut();
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut inner, paint_scale, width, height, 0, 0),
        width,
        height,
    );

    // A generous margin below the line's own box, so antialiasing on the
    // baseline does not count as overflow.
    let first_clear_row = ((f64::from(LINE_HEIGHT) * paint_scale).round() as u32) + 2;
    let mut ink = 0;
    for y in first_clear_row..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            if buffer[offset] < 128 {
                ink += 1;
            }
        }
    }
    ink
}

/// The control: at 1.0 everywhere, the line stays in its box.
#[test]
fn text_stays_inside_its_box_at_scale_one() {
    assert_eq!(
        ink_below_the_line(1.0, 1.0),
        0,
        "the fixture itself overflows, so the other cases prove nothing"
    );
}

/// The case a HiDPI window produces, and the one that was broken.
#[test]
fn text_stays_inside_its_box_when_the_document_is_laid_out_at_two() {
    assert_eq!(
        ink_below_the_line(2.0, 2.0),
        0,
        "painting at the document's own scale must keep glyphs in their boxes"
    );
}

/// Glyph size does not follow the paint scale, so a mismatch costs resolution
/// rather than correctness.
///
/// Written to settle a wrong diagnosis rather than to guard a fix. Text
/// overflowing its boxes in a screenshot of a HiDPI window reads exactly like
/// geometry and glyphs disagreeing about scale; this says it is not that, so
/// the next person looking at it starts somewhere else.
#[test]
fn a_mismatched_paint_scale_does_not_make_text_overflow() {
    assert_eq!(ink_below_the_line(2.0, 1.0), 0);
}

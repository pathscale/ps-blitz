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
//!
//! It was a viewport mismatch, and the last test here is the one that finds it.
//! `commit_cpu_frame` painted into a buffer sized from the requested screenshot
//! while leaving the viewport at the window's own size, so a screenshot taken at
//! a size the window does not have laid the document out for one geometry and
//! painted it into another. Every test above varies the scale and holds the
//! dimensions fixed, which is exactly why none of them caught it.

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

/// Text laid out for one viewport width and painted into a narrower buffer
/// runs past the boxes drawn around it.
///
/// This is the fault the three tests above were written to look for and did not
/// find, because all of them vary the *scale* and none of them vary the
/// viewport's dimensions. `commit_cpu_frame` painted into a buffer sized from
/// the requested screenshot size while leaving the viewport at whatever the
/// window happened to be, so a screenshot taken at a size the window does not
/// have laid the document out for one geometry and painted it into another.
///
/// A wide paragraph is used rather than the single-line fixture: the symptom is
/// text reaching horizontally past where its box was drawn, which needs a line
/// long enough to wrap differently at the two widths.
#[test]
fn text_laid_out_for_a_wider_viewport_paints_outside_the_narrow_one() {
    const WIDE: u32 = 800;
    const NARROW: u32 = 300;
    // The width is a percentage of the viewport, which is what makes this
    // sensitive to viewport width at all: a fixed `280px` box wraps identically
    // whatever the viewport is and would prove nothing. Real pages are
    // overwhelmingly built this way, which is why the fault shows on them.
    const PARAGRAPH: &str = r#"<!doctype html><html><body style="margin:0;background:#fff">
<div style="width:95%;font:16px/24px monospace;color:#000">
Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy Hgjy
</div></body></html>"#;

    // Ink to the right of where a box laid out for `NARROW` could reach.
    let ink_right_of_the_box = |viewport_width: u32| {
        let mut document = ScriptDocument::from_html(PARAGRAPH, DocumentConfig::default());
        {
            let mut inner = document.inner_mut();
            inner.set_viewport(Viewport::new(viewport_width, 400, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
        }
        let mut inner = document.inner_mut();
        let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut inner, 1.0, NARROW, 400, 0, 0),
            NARROW,
            400,
        );
        let mut ink = 0;
        for y in 0..400u32 {
            for x in 290..NARROW {
                let offset = ((y * NARROW + x) * 4) as usize;
                if buffer[offset] < 128 {
                    ink += 1;
                }
            }
        }
        ink
    };

    // Laid out for the buffer it is painted into: the paragraph wraps at 280px
    // and nothing reaches the right edge.
    assert_eq!(
        ink_right_of_the_box(NARROW),
        0,
        "a document laid out for the size being painted must stay inside it"
    );

    // Laid out for a viewport far wider than the buffer. This is what
    // `commit_cpu_frame` used to do, and it is the overflow.
    assert!(
        ink_right_of_the_box(WIDE) > 0,
        "laying out at {WIDE} and painting at {NARROW} should demonstrate the \
         mismatch this test exists to describe; if it no longer does, the \
         fixture stopped being sensitive to viewport width"
    );
}

//! A sub-document's page background has to land where the sub-document is.
//!
//! `BlitzDomPainter::initial_x/initial_y` were being fed in two different
//! units. The top level passes safe-area insets in *logical* pixels, while
//! `draw_sub_document` passes `translation + content_box.origin()`, which are
//! *device* pixels. Inside `paint_scene` the two uses then disagree with each
//! other as well: the root background rect multiplies by `scale`, and both
//! `render_element` and the viewport cull use the value raw.
//!
//! The result is a sub-document whose contents paint in the right place while
//! its page background paints at twice the offset — off the bottom-right of
//! wherever it should be. It hides in two ways: the top level normally has zero
//! insets, so `0 * scale` is still `0`, and a headless capture renders a
//! standalone document at origin. It only appears in a real window, at a
//! device scale above 1, for a page embedded as a sub-document — which is
//! every page in the browser, on every HiDPI display.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::{Arc, Mutex};

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;

static PAINT_LOCK: Mutex<()> = Mutex::new(());

/// Render at device scale 2, which is what a retina window uses and what the
/// double-scaled offset needs in order to be visible at all.
const SCALE: f64 = 2.0;
const CSS_W: u32 = 200;
const CSS_H: u32 = 150;

fn pixel(html: &str, css_x: usize, css_y: usize) -> [u8; 3] {
    let guard = PAINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            // Viewport::new takes *device* dimensions and divides by scale to
            // get CSS pixels, so these are the surface size, not the CSS size.
            viewport: Some(Viewport::new(
                (CSS_W as f64 * SCALE) as u32,
                (CSS_H as f64 * SCALE) as u32,
                SCALE as f32,
                ColorScheme::Light,
            )),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc.resolve(0.0);

    let px_w = (CSS_W as f64 * SCALE) as u32;
    let px_h = (CSS_H as f64 * SCALE) as u32;
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), SCALE, px_w, px_h, 0, 0),
        px_w,
        px_h,
    );
    drop(guard);

    let x = (css_x as f64 * SCALE) as usize;
    let y = (css_y as f64 * SCALE) as usize;
    let idx = (y * px_w as usize + x) * 4;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// An iframe pushed away from the origin, whose document paints a red page
/// background. Every point inside the frame must be red.
#[test]
fn a_sub_documents_page_background_covers_the_sub_document() {
    let html = "<html><body style='margin:0;background:#ffffff'>\
                <div style='height:40px'></div>\
                <iframe srcdoc=\"<html><body style='margin:0;background:#ff0000'></body></html>\" \
                        style='display:block;margin-left:40px;width:120px;height:80px;border:0'>\
                </iframe></body></html>";

    // Just inside the frame's top-left corner. With the offset applied twice
    // the background starts far below and right of here, leaving the page
    // white where the sub-document should be.
    assert_eq!(
        pixel(html, 45, 45),
        [255, 0, 0],
        "the sub-document's page background did not reach its top-left corner"
    );

    // The middle of the frame, which the doubled offset also misses.
    assert_eq!(
        pixel(html, 100, 80),
        [255, 0, 0],
        "the sub-document's page background did not cover its centre"
    );
}

/// The background must not spill outside the frame either.
#[test]
fn a_sub_documents_page_background_stays_inside_it() {
    let html = "<html><body style='margin:0;background:#ffffff'>\
                <div style='height:40px'></div>\
                <iframe srcdoc=\"<html><body style='margin:0;background:#ff0000'></body></html>\" \
                        style='display:block;margin-left:40px;width:120px;height:80px;border:0'>\
                </iframe></body></html>";

    assert_eq!(
        pixel(html, 10, 10),
        [255, 255, 255],
        "the sub-document's background painted above and left of the frame"
    );
}

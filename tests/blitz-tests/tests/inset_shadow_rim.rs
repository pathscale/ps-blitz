//! An inset shadow is a soft rim inside the edge, drawn with one layer.
//!
//! The effect is the padding box filled with the shadow colour and a blurred
//! copy of the border box punched back out of it, so the colour survives only
//! near the edge. That needs a compositing group for the `DestOut` to compose
//! against, and it needs clipping to the padding box, and it used to use one
//! nested layer for each of those two jobs.
//!
//! They are the same clip, so one layer does both. The second was not free:
//! every layer is a GPU group that vello gives scratch buffers, and vello pools
//! those by size class and never releases them (`ResourcePool` in
//! `wgpu_engine.rs` has no eviction path, on 0.9 or on master). A frame of
//! AgencyZero peaked at 116 layers, 74 of them inset shadows, and the residue
//! was 52 pooled 8 MB blocks: 416 MB held for the life of the process.
//!
//! Halving the layer count is only worth having if the pixels are unchanged,
//! which is what this asserts: colour at the rim, background in the middle, and
//! nothing painted outside the element.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::{Arc, Mutex};

/// `latest_scene_layers` reads process-global counters published by *every*
/// scene painted in this binary, so any test that paints clobbers them for a
/// test that is about to read them. Every test here paints, so every test takes
/// this lock, not just the two that read the counters. Rendering is well under
/// a second, so serialising them costs less than the flake did.
static LAYER_COUNTS: Mutex<()> = Mutex::new(());

const WIDTH: u32 = 120;
const HEIGHT: u32 = 120;

/// The element covers 20..100 on both axes, on a white page.
const HTML: &str = r#"
<!doctype html>
<html><body style="margin:0;background:#ffffff">
  <div style="position:absolute;left:20px;top:20px;width:80px;height:80px;
              background:#ffffff;
              box-shadow: inset 0 0 12px 0 rgb(0, 0, 255);"></div>
</body></html>
"#;

fn render(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

fn document() -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// `(r, g, b)` at a pixel.
fn pixel(buffer: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * WIDTH + x) * 4) as usize;
    (buffer[i], buffer[i + 1], buffer[i + 2])
}

/// How blue a pixel is, over the white page it sits on.
fn blueness(buffer: &[u8], x: u32, y: u32) -> i32 {
    let (r, _g, b) = pixel(buffer, x, y);
    i32::from(b) - i32::from(r)
}

#[test]
fn the_shadow_is_strongest_at_the_edge_and_absent_in_the_middle() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = document();
    let buffer = render(&mut doc);

    // Just inside the top edge, and the centre.
    let rim = blueness(&buffer, 60, 23);
    let middle = blueness(&buffer, 60, 60);

    assert!(
        rim > 20,
        "the rim should carry the shadow colour, got blueness {rim}",
    );
    assert!(
        middle < 8,
        "the middle should be clear of the shadow, got blueness {middle}",
    );
    assert!(
        rim > middle + 20,
        "the shadow should fall off toward the middle: rim {rim}, middle {middle}",
    );
}

/// The clip is the whole reason the remaining layer exists.
///
/// A `DestOut` group that is not clipped to the padding box would compose
/// against, and punch a hole in, whatever is behind the element.
#[test]
fn nothing_is_painted_outside_the_element() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = document();
    let buffer = render(&mut doc);

    for (x, y, corner) in [
        (5, 5, "top left"),
        (114, 5, "top right"),
        (5, 114, "bottom left"),
        (114, 114, "bottom right"),
    ] {
        assert_eq!(
            pixel(&buffer, x, y),
            (255, 255, 255),
            "the page is white outside the element, but the {corner} corner is not",
        );
    }
}

/// Two inset shadows compose, so the loop cannot leave a layer unbalanced.
///
/// An unmatched `push_layer` does not fail loudly: the scene simply carries an
/// open group to the rasteriser and everything after it is clipped to the last
/// shape pushed. Rendering a second element after the shadowed one is what
/// catches that.
#[test]
fn a_later_element_is_not_clipped_by_the_shadow() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    const TWO: &str = r#"
    <!doctype html>
    <html><body style="margin:0;background:#ffffff">
      <div style="position:absolute;left:10px;top:10px;width:40px;height:40px;
                  background:#ffffff;
                  box-shadow: inset 0 0 8px 0 rgb(0,0,255), inset 0 0 3px 0 rgb(0,255,0);"></div>
      <div style="position:absolute;left:70px;top:70px;width:30px;height:30px;
                  background:rgb(255,0,0)"></div>
    </body></html>
    "#;

    let mut doc = HtmlDocument::from_html(
        TWO,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render(&mut doc);

    let (r, g, b) = pixel(&buffer, 85, 85);
    assert!(
        r > 200 && g < 80 && b < 80,
        "the red square after the shadowed element should paint in full, got ({r}, {g}, {b})",
    );
}

/// A shadow at zero alpha must cost nothing, not two layers.
///
/// The guard used to be `shadow_color == Color::TRANSPARENT`, an exact
/// comparison against a float that has been through colour parsing, a custom
/// property and an sRGB conversion. `draw_outset_box_shadow` was fixed for
/// exactly that and this half was not.
///
/// It matters because of what a layer costs. Each inset shadow pushes two
/// compositing groups every frame, and vello pools the scratch buffers it gives
/// them by size class and never releases them. An application driving shadow
/// alpha from a custom property has the declaration on every panel with the
/// value at zero unless someone moved a slider, so this is the default state.
#[test]
fn an_invisible_inset_shadow_paints_nothing() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    const INVISIBLE: &str = r#"
    <!doctype html>
    <html><body style="margin:0;background:#ffffff">
      <div style="position:absolute;left:20px;top:20px;width:80px;height:80px;
                  background:#ffffff;
                  box-shadow: inset 0 0 12px 0 rgba(0, 0, 255, 0);"></div>
    </body></html>
    "#;

    let mut doc = HtmlDocument::from_html(
        INVISIBLE,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render(&mut doc);

    for (x, y) in [(60, 23), (23, 60), (60, 60)] {
        assert_eq!(
            pixel(&buffer, x, y),
            (255, 255, 255),
            "a fully transparent shadow must not tint ({x}, {y})",
        );
    }
}

/// A transparent shadow must not suppress a visible one behind it.
///
/// The old guard used `return` from inside the loop rather than `continue`, so
/// the first invisible shadow abandoned every shadow after it.
#[test]
fn a_transparent_shadow_does_not_cancel_the_one_after_it() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    const MIXED: &str = r#"
    <!doctype html>
    <html><body style="margin:0;background:#ffffff">
      <div style="position:absolute;left:20px;top:20px;width:80px;height:80px;
                  background:#ffffff;
                  box-shadow: inset 0 0 12px 0 rgba(255, 0, 0, 0),
                              inset 0 0 12px 0 rgb(0, 0, 255);"></div>
    </body></html>
    "#;

    let mut doc = HtmlDocument::from_html(
        MIXED,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render(&mut doc);

    let rim = blueness(&buffer, 60, 23);
    assert!(
        rim > 20,
        "the visible shadow after a transparent one must still paint, got blueness {rim}",
    );
}

/// The layer saving, asserted rather than assumed.
///
/// Pixels alone cannot show this: `rgba(..., 0)` parses to exactly zero, so the
/// old exact-comparison guard also produced no visible shadow. What it did not
/// do is skip the work, because the guard sat inside the loop after the padding
/// box path was built and after both layers were pushed.
///
/// Two compositing groups per panel, every frame, for a shadow nobody can see.
/// That is what feeds vello's buffer pool, and vello never releases what it
/// pools.
#[test]
fn an_invisible_inset_shadow_costs_no_layers() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    const INVISIBLE: &str = r#"
    <!doctype html>
    <html><body style="margin:0;background:#ffffff">
      <div style="position:absolute;left:20px;top:20px;width:80px;height:80px;
                  background:#ffffff;
                  box-shadow: inset 0 0 12px 0 rgba(0, 0, 255, 0);"></div>
    </body></html>
    "#;

    let mut doc = HtmlDocument::from_html(
        INVISIBLE,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let _ = render(&mut doc);

    let counts = blitz_paint::latest_scene_layers();
    let inset = counts.by_site[blitz_paint::LayerSite::InsetShadow as usize];
    assert_eq!(
        inset, 0,
        "an invisible inset shadow must push no compositing layers, got {inset}",
    );
}

/// And a visible one still does, so the guard cannot be "skip everything".
#[test]
fn a_visible_inset_shadow_still_costs_its_layers() {
    let _serial = LAYER_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = document();
    let _ = render(&mut doc);

    let counts = blitz_paint::latest_scene_layers();
    let inset = counts.by_site[blitz_paint::LayerSite::InsetShadow as usize];
    assert!(
        inset > 0,
        "a visible inset shadow still needs its compositing layers",
    );
}

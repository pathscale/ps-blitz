//! `clip-path` must clip the outline and the outset box shadow too.
//!
//! This is the shape every component library uses to hide the real control
//! behind a custom one: a 1px absolutely positioned input with
//! `clip-path: inset(50%)`, which is a zero-area clip. The visible switch is a
//! sibling.
//!
//! `draw_outline` and `draw_outset_box_shadow` run *before* the clip-path layer
//! is pushed (`render.rs`), so neither is clipped. With the UA sheet's
//! `input:focus { outline: 2px solid #4D90FE }`, focusing such an input paints a
//! blue mark next to the control it was supposed to be invisible behind.
//! Reported against AgencyZero's Settings toggles, where the focused one grew a
//! blue speck off its left edge and the unfocused one did not.
//!
//! CSS says `clip-path` clips the element it is applied to, and that includes
//! the outline and the shadow.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 100;

fn render(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

/// Coloured pixels.
///
/// The fixture is deliberately achromatic — a white page and a `#111` track —
/// so anything with colour in it came from the blue focus outline and nothing
/// else. Testing "not white and not black" instead catches the track's own
/// antialiased edge, which is a correct grey and not the subject.
fn coloured_pixels(buffer: &[u8]) -> Vec<(u32, u32, [u8; 3])> {
    let mut coloured = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel = [buffer[offset], buffer[offset + 1], buffer[offset + 2]];
            let spread = pixel.iter().max().unwrap() - pixel.iter().min().unwrap();
            if spread > 24 {
                coloured.push((x, y, pixel));
            }
        }
    }
    coloured
}

fn document(focus: bool) -> HtmlDocument {
    // The sr-only pattern, verbatim from a shipping toggle: 1px box, absolutely
    // positioned, zero-area clip. The track beside it is the part meant to show.
    let html = r#"<html><body style="margin:0;background:#fff">
      <span style="position:relative;display:inline-block;margin:40px">
        <input type="checkbox" checked
               style="position:absolute;width:1px;height:1px;margin:-1px;padding:0;
                      border:0;overflow:hidden;clip-path:inset(50%)">
        <span style="display:block;width:40px;height:20px;border-radius:9999px;
                     background:#111"></span>
      </span>
    </body></html>"#;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    if focus {
        let input = doc
            .query_selector("input")
            .expect("valid selector")
            .expect("the fixture has an input");
        doc.set_focus_to(input);
        doc.resolve(0.0);
    }
    doc
}

/// The baseline: unfocused, the hidden input contributes nothing.
#[test]
fn an_unfocused_sr_only_input_paints_nothing() {
    let mut doc = document(false);
    let buffer = render(&mut doc);
    let stray = coloured_pixels(&buffer);
    assert!(
        stray.is_empty(),
        "unfocused fixture should have no colour in it, found {:?}",
        &stray[..stray.len().min(4)]
    );
}

/// Focused, the UA outline must still be clipped away by `clip-path`.
#[test]
fn a_focused_sr_only_input_keeps_its_outline_inside_the_clip() {
    let mut doc = document(true);
    let buffer = render(&mut doc);
    let stray = coloured_pixels(&buffer);
    assert!(
        stray.is_empty(),
        "a focus outline escaped a zero-area clip-path, found {:?}",
        &stray[..stray.len().min(4)]
    );
}

//! Text must not paint outside the box that clips it.
//!
//! Two reports of overlapping text, both on a long string next to something
//! else: a branch name `fix/pr-relaunch-corruption` running into the chip
//! beside it, and a transcript line running under a status readout. Both are
//! styled the way the application truncates — `overflow: hidden` with
//! `text-overflow: ellipsis` and `white-space: nowrap` — so the question is
//! whether the clip is applied to the glyphs at all.
//!
//!   cargo test --release -p blitz-tests --test text_clipping -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 400;
const HEIGHT: u32 = 60;
/// The clipped box is 150px wide at x=0, so nothing may paint past here.
const CLIP_EDGE: u32 = 150;

fn render(html: &str) -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

/// The x of the rightmost dark pixel, i.e. how far the text actually reached.
fn rightmost_ink(buffer: &[u8]) -> Option<u32> {
    let mut rightmost = None;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let idx = ((y * WIDTH + x) * 4) as usize;
            if buffer[idx] < 200 && buffer[idx + 1] < 200 && buffer[idx + 2] < 200 {
                rightmost = Some(rightmost.map_or(x, |current: u32| current.max(x)));
            }
        }
    }
    rightmost
}

const LONG: &str = "fix/pr-relaunch-corruption-and-more-branch-name-here";

#[test]
fn a_nowrap_overflow_hidden_box_clips_its_text() {
    let buffer = render(&format!(
        r#"<html><body style="margin:0; background:#ffffff; font-size:16px; color:#000000">
             <div style="width:150px; overflow:hidden; white-space:nowrap;
                         text-overflow:ellipsis">{LONG}</div>
           </body></html>"#
    ));

    let reached = rightmost_ink(&buffer).expect("the text paints");
    assert!(
        reached <= CLIP_EDGE,
        "text painted out to x={reached} from a box that ends at {CLIP_EDGE}: \
         `overflow: hidden` is not clipping the glyphs, so a long string runs \
         over whatever sits beside it"
    );
}

/// The same box as a flex item, which is how the application lays these out:
/// a truncating label beside a chip, in a row.
#[test]
fn a_truncating_flex_item_clips_its_text() {
    let buffer = render(&format!(
        r#"<html><body style="margin:0; background:#ffffff; font-size:16px; color:#000000">
             <div style="display:flex; width:400px">
               <div style="width:150px; min-width:0; overflow:hidden; white-space:nowrap;
                           text-overflow:ellipsis">{LONG}</div>
             </div>
           </body></html>"#
    ));

    let reached = rightmost_ink(&buffer).expect("the text paints");
    assert!(
        reached <= CLIP_EDGE,
        "text painted out to x={reached} from a flex item that ends at {CLIP_EDGE}"
    );
}

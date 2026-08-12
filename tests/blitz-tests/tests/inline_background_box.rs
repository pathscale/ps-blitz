//! An inline box's background must honour `border-radius` and paint its border.
//!
//! The application styles inline code as a chip: `rounded-[5px] border
//! bg-base-300 px-[5px]`. It renders as a hard-edged rectangle with no border,
//! which reads as a stray square sitting behind the text.
//!
//!   cargo test --release -p blitz-tests --test inline_background_box -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 100;
const PAGE: [u8; 3] = [255, 255, 255];
const CHIP: [u8; 3] = [0, 0, 255];

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

fn pixel(buffer: &[u8], x: u32, y: u32) -> [u8; 3] {
    let idx = ((y * WIDTH + x) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// A block with the same styling, as the control: whatever the answer is for
/// an inline box, a block box has to already get it right for the comparison
/// to mean anything.
#[test]
fn a_block_background_is_rounded() {
    let buffer = render(
        r#"<html><body style="margin:0; background:#ffffff">
             <div style="position:absolute; left:20px; top:20px; width:100px; height:40px;
                         border-radius:12px; background:#0000ff"></div>
           </body></html>"#,
    );
    assert_eq!(
        pixel(&buffer, 60, 40),
        CHIP,
        "control: the middle of the block is painted"
    );
    assert_eq!(
        pixel(&buffer, 20, 20),
        PAGE,
        "control: a 12px radius should leave the block's corner unpainted"
    );
}

/// The same corner test on an inline box, which is the application's chip.
#[test]
fn an_inline_background_is_rounded() {
    let buffer = render(
        r#"<html><body style="margin:0; background:#ffffff; font-size:20px; line-height:40px">
             <span style="border-radius:12px; background:#0000ff; padding:0 10px">xxxxx</span>
           </body></html>"#,
    );

    // Find the chip's painted extent, so the corner probe is its corner rather
    // than a guess at where the text engine put it.
    let mut left = WIDTH;
    let mut top = HEIGHT;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if pixel(&buffer, x, y) == CHIP {
                left = left.min(x);
                top = top.min(y);
            }
        }
    }
    assert!(left < WIDTH && top < HEIGHT, "the chip painted somewhere");

    assert_ne!(
        pixel(&buffer, left, top),
        CHIP,
        "an inline box painted its background into the corner a 12px radius should \
         have rounded away: the chip renders as a hard-edged square"
    );
}

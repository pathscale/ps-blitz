//! `outline-offset` moves the outline ring, in both directions.
//!
//! It was read by nobody: `create_css_rect` resolved `outline-width` and built
//! the ring between the border box and that width, so every offset computed to
//! the same picture. A probe at `-2px` painted in exactly the same place as one
//! with no offset at all, which is how the bug was first noticed.
//!
//! That matters to more than spec compliance. AgencyZero's panel hairline asks
//! for `outline-offset: -1px` so the ring sits just inside the rounded corner;
//! what it got was a ring one pixel outside the box, which reads as the border
//! refusing to pick up the panel styling.
//!
//! Each case samples the same row through the left edge, so the three assert
//! against each other: the ring has to be in a different place each time.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 200;

/// The box occupies 60..140 on both axes, so its left edge is at x = 60.
const BOX_LEFT: u32 = 60;

fn row(offset: &str) -> Vec<[u8; 3]> {
    let html = format!(
        r#"<html><body style="margin:0;background:#fff">
          <div style="position:absolute;left:60px;top:60px;width:80px;height:80px;
                      background:#000;
                      outline:2px solid #ff0000;
                      outline-offset:{offset};"></div>
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

    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );

    // One row through the middle of the box, well clear of its corners.
    (0..WIDTH)
        .map(|x| {
            let offset = ((100 * WIDTH + x) * 4) as usize;
            [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
        })
        .collect()
}

fn is_red(pixel: [u8; 3]) -> bool {
    pixel[0] > 150 && pixel[1] < 90 && pixel[2] < 90
}

/// The x range covered by the ring's left edge, found by looking rather than
/// assumed.
///
/// Bounded to the left half of the row: the row crosses the ring twice, so
/// scanning the whole width reports one span running from the left edge to the
/// right one.
fn ring_span(row: &[[u8; 3]]) -> (usize, usize) {
    let left = &row[..(WIDTH / 2) as usize];
    let first = left
        .iter()
        .position(|p| is_red(*p))
        .expect("no outline drawn");
    let last = left
        .iter()
        .enumerate()
        .filter(|(_, p)| is_red(**p))
        .map(|(x, _)| x)
        .next_back()
        .unwrap();
    (first, last)
}

#[test]
fn no_offset_puts_the_ring_immediately_outside_the_border_box() {
    let row = row("0");
    let (first, last) = ring_span(&row);

    assert_eq!(
        (first, last),
        ((BOX_LEFT - 2) as usize, (BOX_LEFT - 1) as usize),
        "a 2px outline at offset 0 occupies the two pixels outside the box"
    );
}

#[test]
fn a_positive_offset_pushes_the_ring_away_and_leaves_a_gap() {
    let row = row("6px");
    let (first, last) = ring_span(&row);

    assert_eq!(
        (first, last),
        ((BOX_LEFT - 8) as usize, (BOX_LEFT - 7) as usize),
        "offset 6 moves the ring 6px further out"
    );
    // The page must show through between the ring and the box, which is what
    // distinguishes a moved outline from a thicker one.
    assert_eq!(
        row[(BOX_LEFT - 4) as usize],
        [255, 255, 255],
        "the gap the offset opens should be page, not outline"
    );
}

#[test]
fn a_negative_offset_pulls_the_ring_inside_the_border_box() {
    let row = row("-2px");
    let (first, last) = ring_span(&row);

    assert_eq!(
        (first, last),
        (BOX_LEFT as usize, (BOX_LEFT + 1) as usize),
        "offset -2 pulls a 2px ring fully inside the box"
    );
    assert_eq!(
        row[(BOX_LEFT - 1) as usize],
        [255, 255, 255],
        "nothing should paint outside the box any more"
    );
}

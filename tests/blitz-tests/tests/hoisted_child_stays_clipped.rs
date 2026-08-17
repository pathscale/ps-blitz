//! A hoisted child stays inside the clips it was hoisted past.
//!
//! A positioned child with a `z-index` is lifted into its containing stacking
//! context and painted from there, so it never passes the display or overflow
//! checks of the ancestors it skipped. `HoistedChild` records those ancestors
//! and `resolve_hoisted_clips` turns them into real clips.
//!
//! `6eb7f0ac` fixed one escape from this: a hidden pane's subtree was walked,
//! publishing its raised children into the visible tab's paint list, and one
//! ghost chevron appeared per retained tab. That fix stops the walk at
//! `display: none`.
//!
//! This covers the other side of the same shape: an ancestor that is visible
//! but *clips*. AgencyZero's panel-edge chevron is `absolute left-full z-20`
//! inside a horizontally scrolling tab strip, and the strip's box is wider
//! than its client area, so anything escaping its clip lands on the window
//! chrome next to it.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const W: u32 = 200;
const H: u32 = 60;

fn render(html: &str) -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(W, H, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, W, H, 0, 0),
        W,
        H,
    )
}

/// Is anything non-white painted in the column band `x0..x1`?
fn painted_between(buffer: &[u8], x0: u32, x1: u32) -> bool {
    (0..H).any(|y| {
        (x0..x1).any(|x| {
            let i = ((y * W + x) * 4) as usize;
            buffer[i] < 200 || buffer[i + 1] < 200 || buffer[i + 2] < 200
        })
    })
}

/// A `z-index` child positioned past the right edge of a clipping parent.
///
/// The parent clips at x=100. The child is pushed to `left:100%`, so it starts
/// exactly at the clip boundary and must not appear at all.
#[test]
fn a_hoisted_child_does_not_escape_its_clipping_ancestor() {
    let buffer = render(
        r#"<html><body style="margin:0;background:#ffffff">
             <div style="position:relative; width:100px; height:60px; overflow:hidden">
               <div style="position:absolute; left:100%; top:0; z-index:20;
                           width:80px; height:60px; background:#000000"></div>
             </div>
           </body></html>"#,
    );

    assert!(
        !painted_between(&buffer, 105, W),
        "a z-index child positioned past its clipping ancestor was painted \
         outside that ancestor's overflow"
    );
}

/// The control: without `overflow:hidden` the same child *is* visible, so the
/// assertion above is testing the clip and not some unrelated culling.
#[test]
fn the_same_child_paints_when_nothing_clips_it() {
    let buffer = render(
        r#"<html><body style="margin:0;background:#ffffff">
             <div style="position:relative; width:100px; height:60px">
               <div style="position:absolute; left:100%; top:0; z-index:20;
                           width:80px; height:60px; background:#000000"></div>
             </div>
           </body></html>"#,
    );

    assert!(
        painted_between(&buffer, 105, W),
        "the fixture's child never painted at all, so the clip test proves nothing"
    );
}

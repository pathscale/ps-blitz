//! Positioned descendants must retain every ancestor overflow clip after they
//! are hoisted into a stacking context for paint ordering.
//!
//! "Every" is not "any": an ancestor does not clip a positioned box whose
//! containing block is outside that ancestor (CSS 2.1 11.1.1). Getting that
//! backwards clips things that are meant to hang outside their parent, which
//! is a whole idiom — the application's own panel-edge chevron is
//! `absolute left-full`, drawn deliberately beyond its parent's right edge.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 240;
const HEIGHT: u32 = 180;

const WHITE: [u8; 3] = [255, 255, 255];
const GREEN: [u8; 3] = [0, 128, 0];
const RED: [u8; 3] = [255, 0, 0];

fn document(body: &str) -> HtmlDocument {
    let html = format!(r#"<html><body style="margin:0;background:#fff">{body}</body></html>"#);
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let offset = ((y * WIDTH + x) * 4) as usize;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

/// A 120x60 panel that is both the clipper and the containing block, with a
/// raised child placed 30px below its bottom edge.
const PANEL: &str = r#"<div style="position:relative;width:120px;height:60px;overflow:hidden;background:#008000">
        <div style="position:absolute;z-index:10;left:20px;top:90px;width:40px;height:30px;background:#ff0000"></div>
      </div>"#;

#[test]
fn z_index_child_does_not_escape_an_overflow_hidden_panel() {
    let mut doc = document(PANEL);

    assert_eq!(pixel_at(&mut doc, 30, 30), GREEN, "fixture panel");
    assert_eq!(
        pixel_at(&mut doc, 30, 100),
        WHITE,
        "a z-index child painted outside its overflow-hidden panel",
    );
}

/// The same clip, one level further out: the hoist crosses two boxes, and the
/// clip has to survive being carried up through the second.
#[test]
fn the_clip_survives_a_hoist_through_an_intermediate_box() {
    let mut doc = document(&format!(
        r#"<div style="position:relative;z-index:1"><div style="padding:10px">{PANEL}</div></div>"#
    ));

    assert_eq!(pixel_at(&mut doc, 40, 40), GREEN, "fixture panel");
    assert_eq!(
        pixel_at(&mut doc, 40, 110),
        WHITE,
        "the clip was dropped on the way up to the stacking context",
    );
}

/*
 * The exception, and the reason this cannot simply clip everything it passes:
 * the child is positioned against the outer box, so the `overflow: hidden`
 * wrapper between them has no say over it and it must still paint below.
 */
#[test]
fn a_clipper_that_is_not_the_containing_block_does_not_clip() {
    let mut doc = document(
        r#"<div style="position:relative">
             <div style="width:120px;height:60px;overflow:hidden;background:#008000">
               <div style="position:absolute;z-index:10;left:20px;top:90px;width:40px;height:30px;background:#ff0000"></div>
             </div>
           </div>"#,
    );

    assert_eq!(pixel_at(&mut doc, 30, 30), GREEN, "fixture panel");
    assert_eq!(
        pixel_at(&mut doc, 30, 100),
        RED,
        "a box positioned against an ancestor of the clipper was clipped anyway",
    );
}

/// `position: fixed` is positioned against the viewport, so no ancestor's
/// overflow reaches it. A clipped overlay is a disappeared overlay.
#[test]
fn a_fixed_child_is_not_clipped_by_an_ancestor() {
    let mut doc = document(
        r#"<div style="position:relative;width:120px;height:60px;overflow:hidden;background:#008000">
             <div style="position:fixed;z-index:10;left:20px;top:90px;width:40px;height:30px;background:#ff0000"></div>
           </div>"#,
    );

    assert_eq!(pixel_at(&mut doc, 30, 30), GREEN, "fixture panel");
    assert_eq!(pixel_at(&mut doc, 30, 100), RED, "a fixed child was clipped");
}

/// Negative z-index paints below its parent's background rather than above it,
/// through a separate loop, and is clipped by the same ancestors.
#[test]
fn a_negative_z_index_child_is_clipped_too() {
    let mut doc = document(
        r#"<div style="position:relative;z-index:0;width:120px;height:60px;overflow:hidden">
             <div style="position:absolute;z-index:-1;left:20px;top:90px;width:40px;height:30px;background:#ff0000"></div>
           </div>"#,
    );

    assert_eq!(
        pixel_at(&mut doc, 30, 100),
        WHITE,
        "a negative z-index child painted outside its overflow-hidden panel",
    );
}

/// The clip is the padding box, so a border narrows it. One pixel inside the
/// border edge is still outside the clip.
#[test]
fn the_clip_is_the_padding_box_not_the_border_box() {
    let mut doc = document(
        r#"<div style="position:relative;width:120px;height:60px;overflow:hidden;border:8px solid #008000;background:#fff">
             <div style="position:absolute;z-index:10;left:0;top:0;width:200px;height:200px;background:#ff0000"></div>
           </div>"#,
    );

    assert_eq!(pixel_at(&mut doc, 4, 30), GREEN, "the border itself");
    assert_eq!(
        pixel_at(&mut doc, 20, 30),
        RED,
        "content inside the padding box was clipped away",
    );
    assert_eq!(
        pixel_at(&mut doc, 30, 90),
        WHITE,
        "content below the panel was not clipped",
    );
}

//! A caret in a hidden input must not paint.
//!
//! `display:none` removes a box entirely and `visibility:hidden` keeps the box
//! but draws nothing in it. Either way a focused input inside must not put a
//! blinking caret on screen.
//!
//! Measured in AgencyZero: the composer textbox reported HIDDEN in the
//! inspector while the window still showed a small rectangle flashing on the
//! caret's own 500ms clock, in a place with nothing logical near it. Focus was
//! on the document root at the time, so the caret did not even belong to a
//! visible input.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene_at_time;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const W: u32 = 200;
const H: u32 = 80;

/// Render the document twice, in the caret's on and off phases.
///
/// If the two frames differ, something blinked. That is the whole assertion:
/// a hidden input has nothing that may change between blink phases.
fn blinks(html: &str) -> bool {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(W, H, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let field = doc.query_selector("#field").unwrap().expect("#field");
    doc.set_focus_to(field);

    let on = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_at_time(scene, &mut doc.inner_mut(), 1.0, W, H, 0, 0, 0.25),
        W,
        H,
    );
    let off = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_at_time(scene, &mut doc.inner_mut(), 1.0, W, H, 0, 0, 0.75),
        W,
        H,
    );
    on != off
}

#[test]
fn a_display_none_input_does_not_blink() {
    assert!(
        !blinks(
            r#"<html><body style="margin:0;background:white">
                 <input id="field" value="hi" style="display:none">
               </body></html>"#
        ),
        "a focused `display:none` input still painted a blinking caret"
    );
}

#[test]
fn a_visibility_hidden_input_does_not_blink() {
    assert!(
        !blinks(
            r#"<html><body style="margin:0;background:white">
                 <input id="field" value="hi" style="visibility:hidden">
               </body></html>"#
        ),
        "a focused `visibility:hidden` input still painted a blinking caret"
    );
}

/// An input inside a hidden *ancestor*, which is how a pane that is switched
/// away from hides its composer.
#[test]
fn an_input_in_a_hidden_ancestor_does_not_blink() {
    assert!(
        !blinks(
            r#"<html><body style="margin:0;background:white">
                 <div style="display:none"><input id="field" value="hi"></div>
               </body></html>"#
        ),
        "a focused input inside a `display:none` ancestor still painted a \
         blinking caret"
    );
}

/// The control: a visible focused input must still blink, or the assertions
/// above would pass against a renderer that simply stopped drawing carets.
#[test]
fn a_visible_input_still_blinks() {
    assert!(
        blinks(
            r#"<html><body style="margin:0;background:white">
                 <input id="field" value="hi"
                        style="margin:10px;width:160px;height:40px;color:black;background:white">
               </body></html>"#
        ),
        "a visible focused input did not blink at all; the fixture is wrong"
    );
}

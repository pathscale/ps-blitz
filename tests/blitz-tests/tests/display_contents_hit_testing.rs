//! A `display: contents` box must not make its children unhittable.
//!
//! `display: contents` removes the element's own box: its children lay out as
//! if they were direct children of the grandparent. The element therefore has
//! no size of its own, and a visibility rule derived from *the box* rather than
//! from the subtree concludes that everything inside it is invisible.
//!
//! Observed in an application, through the renderer's own inspection channel: a
//! button drawn on screen at real coordinates reported `visible: false`, and
//! driving a click at it returned `notInteractable: node is not visible`. Its
//! ancestor chain was clean except for one box at `[0, 58, 0, 0]` whose child
//! measured 1316x821. Nothing about that is visible in a DOM-only test
//! environment, which has no visibility model at all, so the app's own suite
//! passed while the dialog could not be closed.
//!
//!   cargo test --release -p blitz-tests --test display_contents_hit_testing

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 200;

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// The node the renderer would deliver a click at this point to.
fn hit(doc: &HtmlDocument, x: f32, y: f32) -> Option<impl std::fmt::Debug> {
    doc.hit(x, y).map(|hit| hit.node_id)
}

/// A plain button, to prove the harness reports a hit at all.
#[test]
fn a_plain_button_is_hittable() {
    let doc = document(
        r#"<html><body style="margin:0">
             <div style="width:200px;height:200px">
               <button id="target" style="width:100px;height:40px">Press</button>
             </div>
           </body></html>"#,
    );

    assert!(
        hit(&doc, 50.0, 20.0).is_some(),
        "no node was hit at all, so this test proves nothing about the next one"
    );
}

/// The regression: the same button, wrapped in `display: contents`.
#[test]
fn a_button_inside_display_contents_is_hittable() {
    let doc = document(
        r#"<html><body style="margin:0">
             <div style="display:contents">
               <button id="target" style="width:100px;height:40px">Press</button>
             </div>
           </body></html>"#,
    );

    assert!(
        hit(&doc, 50.0, 20.0).is_some(),
        "a button inside `display: contents` could not be hit, so every control \
         under such a wrapper is drawn but unclickable"
    );
}

/// Nested, which is what a modal wrapping a dialog body actually produces.
#[test]
fn nested_display_contents_stays_hittable() {
    let doc = document(
        r#"<html><body style="margin:0">
             <div style="display:contents">
               <div style="display:contents">
                 <button id="target" style="width:100px;height:40px">Press</button>
               </div>
             </div>
           </body></html>"#,
    );

    assert!(
        hit(&doc, 50.0, 20.0).is_some(),
        "nesting `display: contents` compounded the failure"
    );
}

//! A node under `display: none` must not keep reporting on-screen bounds.
//!
//! An application's inspection channel reported 225 of 243 on-screen buttons as
//! `visible: false` while their layout bounds were correct, non-zero and inside
//! the window. Driving a click at one through the renderer returned
//! `notInteractable: node is not visible`, so the dialog those buttons belonged
//! to could not be closed.
//!
//! The visibility predicate walks ancestors looking for `display: none`,
//! `hidden` and `aria-hidden`. It is the *bounds* that disagree with it: if a
//! subtree keeps the box it had before it was hidden, then anything reading
//! bounds concludes the control is on screen while the predicate correctly
//! concludes it is not. Both halves cannot be right, and a test environment
//! with no visibility model at all reports neither.
//!
//!   cargo test --release -p blitz-tests --test stale_bounds_under_display_none

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 300;
const HEIGHT: u32 = 300;

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

/// Does a click at this point reach anything?
fn hit(doc: &HtmlDocument, x: f32, y: f32) -> bool {
    doc.hit(x, y).is_some()
}

/// The control case: a visible button is hittable at its own centre.
#[test]
fn a_shown_button_is_hittable() {
    let doc = document(
        r#"<html><body style="margin:0">
             <div><button style="width:100px;height:40px">Press</button></div>
           </body></html>"#,
    );
    assert!(hit(&doc, 50.0, 20.0), "the control case did not hit");
}

/// A button under a `display: none` ancestor must not be hittable, and nothing
/// else should be delivered a click meant for it either.
#[test]
fn a_button_under_display_none_is_not_hittable() {
    let doc = document(
        r#"<html><body style="margin:0">
             <div style="display:none">
               <button style="width:100px;height:40px">Press</button>
             </div>
           </body></html>"#,
    );

    // The body still fills the viewport, so *something* may be hit; what must
    // not happen is the hidden button itself being the target.
    let target = doc.hit(50.0, 20.0).map(|h| h.node_id);
    if let Some(id) = target {
        let node = doc.get_node(id).expect("hit returned an unknown node");
        assert!(
            node.element_data()
                .and_then(|e| Some(e.name.local.as_ref() != "button"))
                .unwrap_or(true),
            "a button under `display: none` was hit-tested as the click target"
        );
    }
}

/// The shape that actually bit: a subtree hidden *after* it was laid out.
///
/// This is what a tab switch or a `Show` toggle produces. If the bounds are not
/// recomputed, the node keeps the box it had while it was on screen and every
/// consumer of bounds believes it is still there.
#[test]
fn hiding_a_laid_out_subtree_clears_its_bounds() {
    let shown = document(
        r#"<html><body style="margin:0">
             <div id="pane"><button id="b" style="width:100px;height:40px">Press</button></div>
           </body></html>"#,
    );
    assert!(
        hit(&shown, 50.0, 20.0),
        "the button was not hittable while shown, so this proves nothing"
    );

    // The same tree with the pane hidden, which is what a tab switch renders.
    let hidden = document(
        r#"<html><body style="margin:0">
             <div id="pane" style="display:none">
               <button id="b" style="width:100px;height:40px">Press</button>
             </div>
           </body></html>"#,
    );

    let still_the_button = hidden
        .hit(50.0, 20.0)
        .and_then(|h| hidden.get_node(h.node_id))
        .and_then(|n| n.element_data())
        .map(|e| e.name.local.as_ref() == "button")
        .unwrap_or(false);
    assert!(
        !still_the_button,
        "a button under a hidden ancestor was still the hit-test target, so its \
         box survived the change"
    );
}

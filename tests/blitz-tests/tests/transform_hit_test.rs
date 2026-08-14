//! Does a transformed box take its hits where it paints?
//!
//! `Node::hit_inner` inverts the node's transform before testing, so this is
//! meant to work. It did not for the control that started this: a slider thumb
//! offset with `transform: translateX(-50%)` took its hits at the untransformed
//! position, so pressing the left half of the visible knob missed it entirely.
//!
//! The difference between the two cases below is the only variable: a length
//! against a percentage. A percentage translate resolves against the element's
//! own border box, which is a different piece of information from the one a
//! length needs, and it is resolved at a different point.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 400;
const HEIGHT: u32 = 200;

/// A 100px box at x=100, shifted left by `shift`, over a full-width backdrop.
fn document(shift: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0">
          <div id="backdrop" style="position:absolute;left:0;top:0;width:400px;height:200px">
            <div id="box" style="position:absolute;left:100px;top:50px;width:100px;height:100px;
                                 transform:{shift}"></div>
          </div>
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
    doc
}

/// The id of whatever is under the point, or "" for the backdrop.
fn hit_id(doc: &HtmlDocument, x: f32, y: f32) -> String {
    let Some(hit) = doc.hit(x, y) else {
        return "<nothing>".into();
    };
    doc.get_node(hit.node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("id")))
        .unwrap_or("<unnamed>")
        .to_string()
}

/// A length translate moves the hits with the paint. This is the control: if it
/// ever fails, the problem is not percentages.
#[test]
fn a_length_translate_moves_the_hit_area() {
    let doc = document("translateX(-50px)");

    // The box paints across x 50..150 now, so 60 is inside it and 190 is not.
    assert_eq!(hit_id(&doc, 60.0, 100.0), "box", "left of its layout position");
    assert_eq!(
        hit_id(&doc, 190.0, 100.0),
        "backdrop",
        "the vacated right side should fall through to the backdrop"
    );
}

/// The same shift written as a percentage of the box's own width.
///
/// `translateX(-50%)` on a 100px box is `-50px`, so this must land in exactly
/// the same places as the test above.
#[test]
fn a_percentage_translate_moves_the_hit_area_too() {
    let doc = document("translateX(-50%)");

    assert_eq!(
        hit_id(&doc, 60.0, 100.0),
        "box",
        "a percentage translate must move the hit area, not only the paint"
    );
    assert_eq!(
        hit_id(&doc, 190.0, 100.0),
        "backdrop",
        "the vacated right side should fall through to the backdrop"
    );
}

/// The same percentage translate, on an element that also declares a
/// `transition` for `transform`.
///
/// This is the shape the real control had: PathScale/UI's slider thumb carries
/// `transition: transform 250ms ease` beside its `translate(-50%, -50%)`. If a
/// declared transition leaves the property sitting at its initial value until
/// something drives it, the thumb never moves at all, in paint or in hits, and
/// it renders welded to the end of the fill instead of centred on it.
#[test]
fn a_transitioned_transform_still_applies_at_rest() {
    let doc = document("translateX(-50%); transition: transform 250ms ease");

    assert_eq!(
        hit_id(&doc, 60.0, 100.0),
        "box",
        "declaring a transition must not stop the transform applying at rest"
    );
    assert_eq!(hit_id(&doc, 190.0, 100.0), "backdrop");
}

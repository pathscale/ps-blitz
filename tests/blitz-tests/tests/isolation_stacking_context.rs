//! `isolation: isolate` creates a stacking context.
//!
//! Its whole purpose is to establish one without any other visual effect, so a
//! negative z-index descendant stays inside it. When it is ignored, that
//! descendant is hoisted to an ancestor context instead and painted before the
//! backgrounds of the boxes in between, which hides it completely: the pattern
//! `isolate` + `position:fixed` + `z-index:-10` is the standard way to mount a
//! full-bleed background image, and it renders as a blank page.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// The node that owns the hoisted negative z-index children, if any.
fn hoists_negative_z(doc: &HtmlDocument, selector: &str) -> bool {
    let id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("{selector} not found"));
    doc.get_node(id)
        .unwrap()
        .stacking_context
        .as_ref()
        .is_some_and(|context| context.neg_z_hoisted_children().len() > 0)
}

#[test]
fn isolate_keeps_a_negative_z_index_descendant_in_its_own_context() {
    let doc = document(
        r#"<html><body style="margin:0">
            <div id="root" style="position:relative;isolation:isolate;min-height:600px">
                <div id="bg" style="position:absolute;inset:0;z-index:-10"></div>
            </div>
        </body></html>"#,
    );

    assert!(
        hoists_negative_z(&doc, "#root"),
        "`isolation: isolate` did not establish a stacking context, so the \
         negative z-index child escaped to an ancestor"
    );
}

#[test]
fn without_isolate_the_descendant_escapes_to_an_ancestor() {
    // The same markup minus `isolation`. A `position: relative` box with
    // `z-index: auto` is not a stacking context, so the child is not held here.
    // This is the contrast case: it pins down that the test above is detecting
    // `isolation` specifically and not merely the presence of a positioned
    // ancestor.
    let doc = document(
        r#"<html><body style="margin:0">
            <div id="root" style="position:relative;min-height:600px">
                <div id="bg" style="position:absolute;inset:0;z-index:-10"></div>
            </div>
        </body></html>"#,
    );

    assert!(
        !hoists_negative_z(&doc, "#root"),
        "a `position: relative` box with `z-index: auto` must not establish a \
         stacking context"
    );
}

#[test]
fn isolate_holds_a_fixed_negative_z_descendant() {
    // The pattern this whole file exists for, and the one 24x.ai uses:
    // `isolation: isolate` + `position: fixed` + `z-index: -10` is the standard
    // way to mount a full-bleed background, and it renders in every shipping
    // browser.
    //
    // It needs the containing block and the stacking context to be decided
    // separately. `hoist_fixed_position_nodes` reparents a fixed node onto the
    // root element so its insets resolve against the viewport, which CSS asks
    // for. Letting that also decide where it paints put the layer in the root's
    // stacking context, underneath every background between it and the isolate,
    // so the page came up blank.
    let doc = document(
        r#"<html><body style="margin:0">
            <div id="root" style="position:relative;isolation:isolate;min-height:600px">
                <div id="bg" style="position:fixed;inset:0;z-index:-10"></div>
            </div>
        </body></html>"#,
    );

    assert!(
        hoists_negative_z(&doc, "#root"),
        "a fixed negative z-index descendant must stay in the isolate's context"
    );
}

#[test]
fn a_held_fixed_layer_still_covers_the_viewport() {
    // Putting the layer in the right stacking context is only half of it. Paint
    // draws a hoisted child at its stacking context root's origin plus the
    // recorded offset plus the node's own layout location, and that location is
    // relative to the root element because the hoist made the root its layout
    // parent. So the offset has to carry the difference between the two
    // origins, or the background lands wherever the isolate happens to be.
    //
    // The isolate is pushed down the page here precisely so that a missing
    // compensation shows up as a 200px error rather than as zero.
    let doc = document(
        r#"<html><body style="margin:0">
            <div style="height:200px"></div>
            <div id="root" style="position:relative;isolation:isolate;min-height:600px">
                <div id="bg" style="position:fixed;inset:0;z-index:-10"></div>
            </div>
        </body></html>"#,
    );

    let root_id = doc.query_selector("#root").unwrap().unwrap();
    let bg_id = doc.query_selector("#bg").unwrap().unwrap();

    let host = doc.get_node(root_id).unwrap();
    let context = host
        .stacking_context
        .as_ref()
        .expect("the isolate must own a stacking context");
    let hoisted = context
        .neg_z_hoisted_children()
        .find(|child| child.node_id == bg_id)
        .expect("the fixed layer must be held by the isolate");

    let host_origin = host.absolute_position(0.0, 0.0);
    let bg_layout = doc.get_node(bg_id).unwrap().final_layout().location;
    let painted_x = host_origin.x + hoisted.position.x + bg_layout.x;
    let painted_y = host_origin.y + hoisted.position.y + bg_layout.y;

    assert_eq!(
        (painted_x, painted_y),
        (0.0, 0.0),
        "`inset: 0` must paint at the viewport origin, not at the isolate's"
    );
}

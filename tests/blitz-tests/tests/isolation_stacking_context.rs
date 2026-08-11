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
#[ignore = "fixed descendants are reparented out of the isolate; see the comment"]
fn isolate_holds_a_fixed_negative_z_descendant() {
    // The pattern this whole file exists for, and the one still broken:
    // `isolation: isolate` + `position: fixed` + `z-index: -10` is the standard
    // way to mount a full-bleed background, and it is what 24x.ai uses.
    //
    // The isolation half is fixed — `#root` reports `is_stacking_context_root`
    // and owns a stacking context. The fixed half is not. Upstream's
    // `collect_fixed` reparents fixed descendants out to their real containing
    // block so they resolve against the viewport, which is correct for layout,
    // but it also removes them from the ancestor's `layout_children`, and
    // hoisting into a stacking context reads that list. So `#root`'s
    // `layout_children` comes back empty and the background never joins the
    // context that `isolate` established for it.
    //
    // Per spec these are independent: `isolation` does not make an element a
    // containing block for fixed descendants, but paint order follows the box
    // tree, not the containing block. Separating the two is engine work beyond
    // the isolation property itself.
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

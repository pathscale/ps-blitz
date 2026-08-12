//! A subtree that is skipped must not lose what it contributes to paint.
//!
//! `flush_styles_to_layout` skips a subtree whose damage union is empty and
//! whose `subtree_hoists` bit is clear. The bit is the whole safety argument:
//! an ancestor rebuilds its stacking context from scratch each flush, so a
//! subtree that feeds it and is not walked disappears from paint. These are the
//! cases where that goes wrong, and none of them appear in the performance
//! fixture the skip was measured on.

use blitz_dom::DocumentConfig;
use blitz_dom::{QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

fn document(body: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><head><style>
             .ctx {{ position: relative; z-index: 0 }}
             .raised {{ position: absolute; z-index: 5; top: 0; left: 0 }}
             .pinned {{ position: fixed; z-index: 9; top: 0; left: 0 }}
             .tint {{ color: #111 }}
             .tint.on {{ color: #eee }}
           </style></head>
           <body style="margin:0;width:900px">{body}</body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// How many nodes the root's stacking context is holding.
fn hoisted_count(doc: &HtmlDocument, selector: &str) -> usize {
    let id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no {selector}"));
    doc.get_node(id)
        .unwrap()
        .stacking_context
        .as_ref()
        .map_or(0, |context| context.children.len())
}

/// A raised child inside a subtree that nothing touches, while a *sibling*
/// subtree is mutated. The untouched subtree is exactly what the skip targets.
#[test]
fn a_raised_child_survives_a_frame_that_skips_its_subtree() {
    let mut doc = document(
        r#"<div class="ctx" id="stack">
             <div id="quiet"><span class="raised">raised</span></div>
             <div id="noisy"><span class="tint">tint</span></div>
           </div>"#,
    );

    let before = hoisted_count(&doc, "#stack");
    assert_eq!(
        before, 1,
        "fixture: the raised child should be hoisted once"
    );

    // Touch the other subtree only, so `#quiet` has no damage and is skipped.
    let tint = doc.query_selector(".tint").unwrap().unwrap();
    {
        let mut mutator = doc.mutate();
        mutator.set_attribute(tint, attr_name("class"), "tint on");
    }
    doc.resolve(0.0);

    assert_eq!(
        hoisted_count(&doc, "#stack"),
        before,
        "the raised child fell out of its ancestor's stacking context when its \
         subtree was skipped"
    );
}

/// The same for `position: fixed`, which is hoisted onto the root element and
/// then placed back into the stacking context its box tree gives it. That path
/// reaches a context which is not the one being built, so the bit has to cover
/// it separately.
#[test]
fn a_fixed_child_survives_a_frame_that_skips_its_subtree() {
    let mut doc = document(
        r#"<div class="ctx" id="stack">
             <div id="quiet"><span class="pinned">pinned</span></div>
             <div id="noisy"><span class="tint">tint</span></div>
           </div>"#,
    );

    let before = hoisted_count(&doc, "#stack");

    let tint = doc.query_selector(".tint").unwrap().unwrap();
    {
        let mut mutator = doc.mutate();
        mutator.set_attribute(tint, attr_name("class"), "tint on");
    }
    doc.resolve(0.0);

    assert_eq!(
        hoisted_count(&doc, "#stack"),
        before,
        "the fixed child fell out of its stacking context when its subtree was \
         skipped"
    );
}

/// Several idle frames in a row, because the bit is set while walking: if it is
/// cleared on the frame that skips, the second frame loses what the first kept.
#[test]
fn the_hoist_survives_repeated_idle_frames() {
    let mut doc = document(
        r#"<div class="ctx" id="stack">
             <div id="quiet"><span class="raised">raised</span></div>
           </div>"#,
    );

    let before = hoisted_count(&doc, "#stack");
    for _ in 0..5 {
        doc.resolve(0.0);
    }

    assert_eq!(
        hoisted_count(&doc, "#stack"),
        before,
        "the hoisted child was lost after repeated frames that changed nothing"
    );
}

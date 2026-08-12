//! A hidden pane contributes nothing to the paint list.
//!
//! Paint checks `display: none` at the element it is drawing. That check is
//! enough for children it reaches *through* that element, and useless for the
//! ones that leave: a positioned child with a z-index is hoisted into an
//! ancestor's stacking context and painted from there, so a hidden pane's
//! raised children paint over the tab in front. The application's panel-edge
//! chevron is `absolute left-full z-20`, and it appeared once per retained tab.
//!
//! The walk that publishes those children could not reach a hidden subtree
//! before: hiding a pane emptied its layout children, and stylo discarded its
//! styles. Both survive now, so the walk has to stop on its own.
//!
//!   cargo test --release -p blitz-tests --test hidden_pane_hoisting -- --nocapture

use blitz_dom::{Document as _, DocumentConfig, NodeId, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
/// The two class lists App.tsx swaps between on a retained pane.
const SHOWN: &str = "flex min-h-0 min-w-0 flex-1";
const HIDDEN: &str = "hidden";

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

/// Two panes, each with a raised absolutely-positioned child, which is the
/// shape of the application's panel-edge chevron.
fn two_panes_with_raised_children() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let pane = |i: usize, class: &str| {
        format!(
            r#"<div id="tab{i}" class="{class}">
                 <div style="position:relative; display:flex; flex:1; min-width:0;">
                   <p>pane {i}</p>
                   <button id="chevron{i}" style="position:absolute; top:40px; left:100%; z-index:20; width:14px; height:36px;">&gt;</button>
                 </div>
               </div>"#
        )
    };
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">{}{}</div>
           </body></html>"#,
        pane(0, SHOWN),
        pane(1, HIDDEN)
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn switch_to(doc: &mut HtmlDocument, shown: usize, hidden: usize) {
    let inner = &mut *doc.inner_mut();
    let mut mutator = inner.mutate();
    let shown_id = mutator
        .doc
        .query_selector(&format!("#tab{shown}"))
        .unwrap()
        .unwrap();
    let hidden_id = mutator
        .doc
        .query_selector(&format!("#tab{hidden}"))
        .unwrap()
        .unwrap();
    mutator.set_attribute(hidden_id, attr_name("class"), HIDDEN);
    mutator.set_attribute(shown_id, attr_name("class"), SHOWN);
    drop(mutator);
    inner.resolve(0.0);
}

fn is_within(doc: &HtmlDocument, node: NodeId, ancestor: NodeId) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = doc.get_node(id).and_then(|node| node.parent);
    }
    false
}

/// Every node reachable from a stacking context anywhere in the document as a
/// hoisted (raised or lowered) paint child.
fn hoisted_everywhere(doc: &HtmlDocument) -> Vec<NodeId> {
    let inner = doc.inner();
    let mut found = Vec::new();
    for (id, node) in inner.tree().iter() {
        let Some(context) = node.stacking_context.as_ref() else {
            continue;
        };
        let _ = id;
        found.extend(context.neg_z_hoisted_children().map(|child| child.node_id));
        found.extend(context.pos_z_hoisted_children().map(|child| child.node_id));
    }
    found
}

#[test]
fn a_hidden_panes_raised_child_is_not_painted_by_an_ancestor() {
    let mut doc = two_panes_with_raised_children();

    let tab0 = doc.query_selector("#tab0").unwrap().unwrap();
    let chevron0 = doc.query_selector("#chevron0").unwrap().unwrap();

    assert!(
        hoisted_everywhere(&doc).contains(&chevron0),
        "fixture: a visible pane's z-raised child should be hoisted for paint"
    );

    // Show the other pane. tab0 is hidden now, and must contribute nothing.
    switch_to(&mut doc, 1, 0);

    let ghosts: Vec<NodeId> = hoisted_everywhere(&doc)
        .into_iter()
        .filter(|id| is_within(&doc, *id, tab0))
        .collect();
    assert!(
        ghosts.is_empty(),
        "a hidden pane's raised child is still in a stacking context, so it paints \
         over the tab in front: {ghosts:?}"
    );
}

//! A stacking context must not outlive the nodes it lists.
//!
//! `flush_styles_to_layout` returns early on a `display: none` subtree, so a
//! stacking-context host inside one keeps the child list it was given on the
//! frame before it was hidden. `resolve_hoisted_positions` walks every node
//! that has a stacking context, hidden or not, and indexed the slab directly,
//! so removing one of those listed children panicked with
//! "invalid SlotMap key used" at resolve.rs:863 on the very next resolve.
//!
//! Reported from a signup page whose header menu switched language English,
//! Spanish, English and then opened a chat launcher: 6 crashes out of 6 runs.
//! The shape reproduced here is that sequence with the site removed.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use markup5ever::{QualName, local_name, ns};
use std::sync::Arc;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
    body { margin: 0 }
    #panel { position: relative; z-index: 0; width: 200px; height: 200px }
    #raised { position: absolute; z-index: 3; top: 10px; left: 10px;
              width: 40px; height: 40px; background: red }
</style></head>
<body>
    <div id="menu">
        <div id="panel"><div id="raised"></div></div>
    </div>
</body></html>
"#;

#[test]
fn removing_a_hoisted_child_of_a_hidden_stacking_context_does_not_panic() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(600, 400, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let menu = doc.query_selector("#menu").unwrap().expect("menu");
    let panel = doc.query_selector("#panel").unwrap().expect("panel");
    let raised = doc.query_selector("#raised").unwrap().expect("raised");

    assert!(
        doc.get_node(panel)
            .unwrap()
            .stacking_context
            .as_ref()
            .is_some_and(|context| context.children.iter().any(|child| child.node_id == raised)),
        "fixture must hoist #raised into #panel's stacking context"
    );

    // Hide the menu. The walk stops at it, so #panel keeps the child list it
    // was handed before it went away.
    doc.mutate().set_attribute(
        menu,
        QualName::new(None, ns!(), local_name!("style")),
        "display: none",
    );
    doc.resolve(0.0);

    // Now drop the hoisted child, exactly as a re-render of the hidden subtree
    // would.
    doc.mutate().remove_and_drop_node(raised);

    // Panicked here before: nothing pruned #panel's list.
    doc.resolve(0.0);

    assert!(
        doc.get_node(panel)
            .unwrap()
            .stacking_context
            .as_ref()
            .is_none_or(|context| context.children.iter().all(|child| child.node_id != raised)),
        "the removed child must not stay in the stacking context"
    );
}

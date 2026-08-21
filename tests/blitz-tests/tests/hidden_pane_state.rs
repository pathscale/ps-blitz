//! What a pane keeps while it is hidden.
//!
//! Stylo throws away the computed styles of a `display: none` subtree
//! (`clear_descendant_data`), which is right for Gecko, where that subtree has
//! no frames. For an application that retains its tabs it means a revealed pane
//! has no old style to diff against, so every node comes back fully damaged and
//! the pane is reconstructed, re-shaped and laid out from nothing — on every
//! switch, forever. `blitz-dom` overrides `clear_data` to keep the styles.
//!
//! This is the guard for that override. Without it `tab_switch_cost` still
//! passes: it gets the right answer, slowly.
//!
//!   cargo test --release -p blitz-tests --test hidden_pane_state -- --nocapture

use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
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

fn two_panes() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">
               <div id="tab0" class="{SHOWN}"><div id="col0" style="display:flex; flex-direction:column; flex:1; min-width:0;">{markup}</div></div>
               <div id="tab1" class="{HIDDEN}"><div id="col1" style="display:flex; flex-direction:column; flex:1; min-width:0;">{markup}</div></div>
             </div>
           </body></html>"#
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

/// How many elements deep inside the first pane still have computed styles.
///
/// Counted over every element in the pane, not `span` alone. The selector used
/// to be `#col0 span` with a floor of 20, and the fixture holds 14 of them, so
/// the guard tripped on its own precondition and the test could not run at all.
/// It also measured the wrong thing: what the `clear_data` override has to keep
/// is the *pane's* computed styles, and the fixture's substance is in its 21
/// paragraphs, 16 divs and 15 list items as much as its spans. `*` counts what
/// the assertion is actually about and does not need revisiting when the
/// fixture is regenerated from a real transcript.
fn styled_nodes(doc: &HtmlDocument) -> usize {
    let ids = doc.query_selector_all("#col0 *").unwrap();
    assert!(
        ids.len() > 20,
        "fixture: the pane holds many elements, found {}",
        ids.len()
    );
    ids.iter()
        .filter(|id| {
            doc.get_node(**id)
                .is_some_and(|node| node.primary_styles().is_some())
        })
        .count()
}

#[test]
fn a_hidden_pane_keeps_its_computed_styles() {
    let mut doc = two_panes();
    let while_visible = styled_nodes(&doc);
    assert!(while_visible > 0, "fixture: the visible pane is styled");

    switch_to(&mut doc, 1, 0);
    assert_eq!(
        styled_nodes(&doc),
        while_visible,
        "hiding a pane discarded the computed styles of its subtree. Revealing it \
         again then has no old style to diff against, so every node comes back \
         fully damaged and the whole pane is rebuilt: see `clear_data` in \
         blitz-dom's stylo integration"
    );

    switch_to(&mut doc, 0, 1);
    assert_eq!(
        styled_nodes(&doc),
        while_visible,
        "a pane that came back is missing styles"
    );
}

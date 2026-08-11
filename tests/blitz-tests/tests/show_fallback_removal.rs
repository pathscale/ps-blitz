//! A removed node must leave the layout tree.
//!
//! The application's boot splash is a `<Show>` fallback: once the workspace is
//! ready Solid removes it and mounts the real content. On a live instance the
//! log said `boot: ready` and the splash was still in the tree at 1318x880,
//! covering the whole window. Everything under it laid out correctly and the
//! window looked blank.
//!
//! So: does removing a node actually take its box out of layout, including when
//! it is replaced by a sibling in the same commit?

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn document(incremental: bool) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0">
            <div id="host" style="display:flex; width:800px; height:600px;">
              <div id="splash" style="flex:1; background:#333;">Loading workspace…</div>
            </div>
          </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.set_incremental_layout(incremental);
    doc.resolve(0.0);
    doc
}

fn splash_box(doc: &HtmlDocument) -> Option<(f32, f32)> {
    let id = doc.query_selector("#splash").unwrap()?;
    let layout = doc.get_node(id)?.final_layout();
    Some((layout.size.width, layout.size.height))
}

fn assert_gone(incremental: bool) {
    let mut doc = document(incremental);
    assert!(splash_box(&doc).is_some(), "fixture never had a splash");

    let host = doc.query_selector("#host").unwrap().expect("no host");
    doc.mutate()
        .set_inner_html(host, r#"<div id="content" style="flex:1">ready</div>"#);
    doc.resolve(0.0);

    assert_eq!(
        splash_box(&doc),
        None,
        "the splash still has a layout box after being replaced, incremental={incremental}"
    );
    let content = doc.query_selector("#content").unwrap().expect("no content");
    let layout = doc.get_node(content).unwrap().final_layout();
    assert!(
        layout.size.width > 100.0,
        "the replacement did not lay out: {}x{}",
        layout.size.width,
        layout.size.height
    );
}

#[test]
fn a_replaced_node_leaves_the_layout_tree() {
    assert_gone(true);
}

#[test]
fn a_replaced_node_leaves_the_layout_tree_without_incremental() {
    assert_gone(false);
}

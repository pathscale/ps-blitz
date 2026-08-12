//! Switching tabs: a hidden pane becomes visible, a visible one hides.
//!
//! This is the case the subtree skip is most likely to get wrong. A retained
//! tab's subtree has no damage of its own — nothing in it changed, the tab was
//! simply not on screen — so a walk gated on damage skips it, and if its taffy
//! styles were never flushed it lays out from stale ones the moment it is
//! shown.
//!
//! The fixture is the application's own transcript markup and shipped
//! stylesheet, mounted twice: the application retains nine tabs and switches
//! between them by display, which is what this reproduces.
//!
//!   cargo test -p blitz-tests --test tab_switch_cost --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;
use std::time::Instant;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
const PANES: usize = 6;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

fn tabbed_document() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let mut tabs = String::new();
    for i in 0..PANES {
        let hidden = if i == 0 { "" } else { "display:none" };
        tabs.push_str(&format!(
            r#"<div id="tab{i}" style="display:flex; flex-direction:column; flex:1; min-height:0; {hidden}">{markup}</div>"#
        ));
    }
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">{tabs}</div>
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

/// The width of the conversation section inside a pane, which is what a stale
/// flush gets wrong: it lays out against whatever width it last saw, or none.
fn pane_content_width(doc: &HtmlDocument, tab: usize) -> f32 {
    let id = doc
        .query_selector(&format!("#tab{tab}"))
        .unwrap()
        .unwrap_or_else(|| panic!("no #tab{tab}"));
    let node = doc.get_node(id).unwrap();
    let section = node
        .children
        .first()
        .copied()
        .expect("a pane holds a transcript");
    doc.get_node(section).unwrap().final_layout().size.width
}

#[test]
fn switching_to_a_retained_tab_lays_it_out() {
    let mut doc = tabbed_document();
    let total = doc.inner().tree().len();
    let shown_width = pane_content_width(&doc, 0);
    assert!(shown_width > 0.0, "fixture: the first tab has a width");

    println!("\n== tab switch, {PANES} panes, {total} nodes ==");

    let mut worst = 0u128;
    for tab in 1..PANES {
        {
            let inner = &mut *doc.inner_mut();
            let mut mutator = inner.mutate();
            let previous = inner_id(&mutator, tab - 1);
            let next = inner_id(&mutator, tab);
            mutator.set_attribute(previous, attr_name("style"), "display:none");
            mutator.set_attribute(
                next,
                attr_name("style"),
                "display:flex; flex-direction:column; flex:1; min-height:0",
            );
        }

        let started = Instant::now();
        doc.inner_mut().resolve(0.0);
        let us = started.elapsed().as_micros();
        worst = worst.max(us);

        let c = layout_counters::last();
        let width = pane_content_width(&doc, tab);
        println!(
            "  -> tab{tab}: {:.2}ms  computed={:<5} width={width:.1}",
            us as f64 / 1000.0,
            c.computed
        );

        assert!(
            (width - shown_width).abs() < 1.0,
            "tab{tab} laid out at {width} where the first tab is {shown_width}: the retained \
             subtree was skipped and never flushed"
        );
    }

    println!("  worst switch {:.2}ms\n", worst as f64 / 1000.0);
}

fn inner_id(mutator: &blitz_dom::DocumentMutator<'_>, tab: usize) -> blitz_dom::NodeId {
    mutator
        .doc
        .query_selector(&format!("#tab{tab}"))
        .unwrap()
        .unwrap_or_else(|| panic!("no #tab{tab}"))
}

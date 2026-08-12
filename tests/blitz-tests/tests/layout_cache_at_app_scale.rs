//! The layout cache, measured at the size the consuming application is.
//!
//! Every cache number recorded so far came off a fixture of a few hundred
//! nodes, and the application reports 4,350 for a project chat and 4,506 for
//! settings, with the same role histogram either way: mostly generic
//! containers, then several hundred buttons. A cache that holds at 130 nodes
//! says nothing about one that has to hold at 4,000.
//!
//! This is the baseline the performance workstream asks for, taken headlessly
//! so it costs seconds rather than a driven session against a live instance.
//!
//!   cargo test -p blitz-tests --test layout_cache_at_app_scale --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

/// Rows of a shape the transcript actually has: a bubble, a header line with
/// small chips, and a body paragraph. Roughly seven elements each.
const ROWS: usize = 500;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

fn app_scale_document() -> HtmlDocument {
    let mut rows = String::new();
    for i in 0..ROWS {
        rows.push_str(&format!(
            r#"<div class="bubble">
                 <div class="head"><span class="who">Agent</span><span class="chip">{i} tok</span>
                   <button class="copy">copy</button></div>
                 <p class="body">Reply {i}: a paragraph long enough to wrap over more than one
                    line, so the row owns a real inline layout rather than a single short line,
                    which is what the transcript holds in practice.</p>
               </div>"#
        ));
    }
    let html = format!(
        r#"<html><head><style>
             .bubble {{ padding: 8px; margin: 6px 0; background: #1b1b1b }}
             .head {{ display: flex; gap: 8px; align-items: baseline }}
             .chip {{ font-size: 11px; color: #888 }}
             .body {{ color: #ddd }}
             .body.hot {{ color: #fff }}
           </style></head>
           <body style="margin:0;width:900px"><div id="list">{rows}</div></body></html>"#
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

fn report(label: &str, total: usize) {
    let c = layout_counters::last();
    let hit_rate = if c.lookups == 0 {
        100.0
    } else {
        (c.hits as f64 / c.lookups as f64) * 100.0
    };
    println!(
        "{label:<22} computed={:<6} distinct={:<6} cleared={:<6} lookups={:<7} hits={:.1}%  of {total} nodes",
        c.computed, c.distinct, c.caches_cleared, c.lookups, hit_rate
    );
}

#[test]
fn the_cache_holds_at_four_thousand_nodes() {
    let mut doc = app_scale_document();
    let total = doc.tree().len();
    println!("\n== layout cache at app scale ==");

    doc.resolve(0.0);
    report("idle", total);
    let idle = layout_counters::last();

    // A streamed token: text appended to the last paragraph.
    let last_body = *doc.query_selector_all(".body").unwrap().last().unwrap();
    {
        let mut mutator = doc.mutate();
        let text = mutator.create_text_node(" more");
        mutator.append_children(last_body, &[text]);
    }
    doc.resolve(0.0);
    report("streamed token", total);
    let token = layout_counters::last();

    // A colour-only class toggle, which is what hover and selection do.
    let first_body = doc.query_selector(".body").unwrap().unwrap();
    {
        let mut mutator = doc.mutate();
        mutator.set_attribute(first_body, attr_name("class"), "body hot");
    }
    doc.resolve(0.0);
    report("class toggle", total);
    let toggle = layout_counters::last();

    println!();

    // An idle frame must compute nothing at any size. This is the property the
    // whole incremental path exists for, and the one that silently stops
    // holding as a document grows.
    assert_eq!(
        idle.computed, 0,
        "an idle frame recomputed {} nodes of {total}",
        idle.computed
    );

    // One token touches one paragraph. A document-sized number here is the
    // "any inline reconstruction clears the world" failure the workstream
    // feared; it is measured rather than assumed.
    assert!(
        token.distinct < 32,
        "one streamed token recomputed {} distinct nodes of {total}",
        token.distinct
    );

    assert_eq!(
        toggle.computed, 0,
        "a colour-only class toggle recomputed {} nodes of {total}",
        toggle.computed
    );
}

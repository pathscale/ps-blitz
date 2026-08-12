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
/// A pane is one *tab*, not one captured screenful. The application reports
/// around 4,260 nodes for an open project tab, which is 18 of these; mounting
/// the capture once made each pane a nineteenth of a real tab and every number
/// this test produced too small to mean anything.
const REPEATS: usize = 18;

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

fn tabbed_document() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let pane = markup.repeat(REPEATS);
    let mut tabs = String::new();
    for i in 0..PANES {
        // Toggled by class, the way the application does it: App.tsx swaps
        // `flex min-h-0 min-w-0 flex-1` for `hidden` on a retained pane. A
        // `style` attribute reaches layout by a different and cheaper route
        // through stylo, so measuring one while shipping the other measures
        // the wrong invalidation.
        let class = if i == 0 { SHOWN } else { HIDDEN };
        // The inner column is the tab's own content box. Without it the
        // repeats become 18 flex siblings in a row, each 75px wide, and the
        // whole benchmark turns into a text-wrapping torture test that no
        // real tab performs.
        tabs.push_str(&format!(
            r#"<div id="tab{i}" class="{class}"><div id="col{i}" style="display:flex; flex-direction:column; flex:1; min-width:0;">{pane}</div></div>"#
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
        .query_selector(&format!("#col{tab}"))
        .unwrap()
        .unwrap_or_else(|| panic!("no #col{tab}"));
    doc.get_node(id).unwrap().final_layout().size.width
}

#[test]
fn switching_to_a_retained_tab_lays_it_out() {
    let mut doc = tabbed_document();
    let total = doc.inner().tree().len();
    let shown_width = pane_content_width(&doc, 0);
    assert!(shown_width > 0.0, "fixture: the first tab has a width");

    println!("\n== tab switch, {PANES} panes, {total} nodes ==");

    let mut worst = 0u128;
    // Two laps of the same panes. The second lap reveals a pane that has been
    // shown before and not touched since, which is the question the first lap
    // cannot answer: does hiding a tab throw away the layout it already had?
    // If a re-reveal costs what a first reveal costs, nothing about a retained
    // tab is retained, and the switch is paying to rediscover what it knew.
    for (lap, tab) in (0..2)
        .flat_map(|lap| (0..PANES).map(move |tab| (lap, tab)))
        .skip(1)
    {
        let previous_tab = if tab == 0 { PANES - 1 } else { tab - 1 };
        {
            let inner = &mut *doc.inner_mut();
            let mut mutator = inner.mutate();
            let previous = inner_id(&mutator, previous_tab);
            let next = inner_id(&mutator, tab);
            mutator.set_attribute(previous, attr_name("class"), HIDDEN);
            mutator.set_attribute(next, attr_name("class"), SHOWN);
        }

        let (evictions_before, stores_before) = taffy::eviction_counts();
        let started = Instant::now();
        doc.inner_mut().resolve(0.0);
        let us = started.elapsed().as_micros();
        worst = worst.max(us);

        let c = layout_counters::last();
        let (evictions, stores) = taffy::eviction_counts();
        let (evictions, stores) = (evictions - evictions_before, stores - stores_before);
        let width = pane_content_width(&doc, tab);
        // Re-entry and eviction together say which kind of expensive this is.
        // A node measured twenty times with no evictions is being asked twenty
        // genuinely different questions; the same with evictions is a cache
        // too small for its working set, and only the second is tunable here.
        println!(
            "  {} tab{tab}: {:6.2}ms  computed={:<6} distinct={:<5} re-entry={:.1}x  \
             cache {:>5.1}% hits, {evictions} evicted of {stores} stored  width={width:.0}",
            if lap == 0 { "first  ->" } else { "again  ->" },
            us as f64 / 1000.0,
            c.computed,
            c.distinct,
            c.computed as f64 / c.distinct.max(1) as f64,
            if c.lookups > 0 {
                c.hits as f64 / c.lookups as f64 * 100.0
            } else {
                0.0
            },
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

//! What one attribute write costs the rest of the document.
//!
//! `DocumentMutator::set_attribute` snapshots the node, which is the input
//! Stylo's own invalidation wants, and then overrides that machinery twice by
//! hand: `RestyleHint::restyle_subtree()` plus `ALL_DAMAGE` on the node, and
//! `restyle_subtree()` again on its parent. Its own comment has asked for the
//! parent half to be conditional on `ElementSelectorFlags` since it was
//! written.
//!
//! A UI framework writes attributes constantly, so this is the cost of a class
//! toggle on a button: it should touch that button, not its parent's whole
//! subtree.
//!
//! Run with:
//!   cargo test -p blitz-tests --test attribute_write_cost --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

/// Siblings under one parent, so "the parent's whole subtree" is a number that
/// stands out from "this node".
const SIBLINGS: usize = 40;

fn document() -> HtmlDocument {
    let mut rows = String::new();
    for i in 0..SIBLINGS {
        rows.push_str(&format!(
            r#"<div class="row" style="padding:4px"><span>row {i} with enough text on it to \
               own an inline layout worth recomputing</span></div>"#
        ));
    }
    let html = format!(
        r#"<html><head><style>
             .row {{ color: #222 }}
             .row.on {{ color: #d00 }}
           </style></head>
           <body style="margin:0;width:900px">
             <div id="list">{rows}</div>
           </body></html>"#
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

/// An *attribute* name. The empty namespace, not `html`: an attribute built
/// with `ns!(html)` is stored under a name nothing matches, so the write lands
/// and changes nothing, which reads exactly like an invalidation bug.
fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

/// A class toggle on one row, with a rule that changes only its colour, so
/// nothing about the change implies relayout of anything.
#[test]
fn a_class_toggle_on_one_row_does_not_recompute_its_siblings() {
    let mut doc = document();
    let total = doc.tree().len();

    doc.resolve(0.0);
    let idle = layout_counters::last();

    let row = doc.query_selector(".row").unwrap().expect("no .row");
    {
        let mut mutator = doc.mutate();
        mutator.set_attribute(row, attr_name("class"), "row on");
    }
    doc.resolve(0.0);
    let write = layout_counters::last();

    println!(
        "nodes={total} siblings={SIBLINGS}\n\
         idle:  computed={} distinct={} cleared={}\n\
         write: computed={} distinct={} cleared={} lookups={} hits={}",
        idle.computed,
        idle.distinct,
        idle.caches_cleared,
        write.computed,
        write.distinct,
        write.caches_cleared,
        write.lookups,
        write.hits,
    );

    // A colour-only change to one row. The bound is deliberately generous: the
    // row, its span, and a handful of ancestors is fine. Recomputing on the
    // order of every sibling is the defect.
    assert!(
        write.distinct < SIBLINGS,
        "a colour-only class toggle on one row recomputed {} distinct nodes of {total}",
        write.distinct
    );
}

/// The narrowing must not cost correctness: a sibling combinator anchored on
/// the changed row still has to re-match. `apply_selector_flags` deposits
/// `HAS_SLOW_SELECTOR_LATER_SIBLINGS` on the parent while matching this, which
/// is exactly the flag the parent hint is now gated on.
#[test]
fn a_sibling_selector_still_sees_the_change() {
    let mut doc = {
        let mut rows = String::new();
        for i in 0..4 {
            rows.push_str(&format!(r#"<div class="row"><span>row {i}</span></div>"#));
        }
        let html = format!(
            r#"<html><head><style>
                 .row {{ color: rgb(0, 0, 0) }}
                 .row.on {{ color: rgb(0, 255, 0) }}
                 .row.on ~ .row {{ color: rgb(255, 0, 0) }}
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
    };

    let rows = doc.query_selector_all(".row").unwrap().to_vec();
    assert_eq!(rows.len(), 4, "fixture");
    let last = *rows.last().unwrap();

    let colour_of = |doc: &HtmlDocument, id| {
        let node = doc.get_node(id).unwrap();
        let styles = node.primary_styles().unwrap();
        format!("{:?}", styles.clone_color())
    };

    let before = colour_of(&doc, last);
    {
        let mut mutator = doc.mutate();
        mutator.set_attribute(rows[0], attr_name("class"), "row on");
    }
    doc.resolve(0.0);
    let after = colour_of(&doc, last);

    assert_ne!(
        before, after,
        "a later sibling did not re-match after the class toggle: {before} both times. \
         The parent hint was skipped when a selector depended on it."
    );
}

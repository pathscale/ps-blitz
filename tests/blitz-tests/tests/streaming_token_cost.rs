//! What a single streamed token costs the layout cache.
//!
//! TODO item 13 in the consuming application asks how often a resolve clears
//! the whole document's taffy cache, on the reading that any inline
//! reconstruction does it, and says that if so it outranks every other
//! performance item. This measures it instead of reading it.
//!
//! The workload is the one the question is about: a transcript of many message
//! paragraphs, with one character appended to the last one, which is what
//! arriving text does.
//!
//! Run with the counters on, since they are what is being read:
//!   cargo test -p blitz-tests --test streaming_token_cost --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

/// Message bodies, sized to the shape the application reports: a few thousand
/// nodes with most of them inline text.
const MESSAGES: usize = 60;

fn transcript() -> HtmlDocument {
    let mut body = String::new();
    for i in 0..MESSAGES {
        body.push_str(&format!(
            r#"<div class="msg" style="padding:8px"><p>Message {i} body text that wraps across \
               more than one line so the paragraph owns a real inline layout rather than a \
               single short line, which is what the transcript actually holds.</p></div>"#
        ));
    }
    let html = format!(
        r#"<html><body style="margin:0;width:900px">{body}<p id="tail">streaming</p></body></html>"#
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

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: local.into(),
    }
}

#[test]
fn one_streamed_token_does_not_recompute_the_whole_transcript() {
    let mut doc = transcript();
    let total_nodes = doc.tree().len();

    // The steady state first: resolve with nothing changed, so the cost of a
    // token is read against a frame that should compute almost nothing.
    doc.resolve(0.0);
    let idle = layout_counters::last();

    let tail = doc.query_selector("#tail").unwrap().expect("no #tail");
    {
        let mut mutator = doc.mutate();
        let text = mutator.create_text_node(" more");
        mutator.append_children(tail, &[text]);
    }
    doc.resolve(0.0);
    let token = layout_counters::last();

    println!(
        "nodes={total_nodes}\n\
         idle:  computed={} distinct={} cleared={} lookups={} hits={}\n\
         token: computed={} distinct={} cleared={} lookups={} hits={}",
        idle.computed,
        idle.distinct,
        idle.caches_cleared,
        idle.lookups,
        idle.hits,
        token.computed,
        token.distinct,
        token.caches_cleared,
        token.lookups,
        token.hits,
    );

    // The claim under test: a token reconstructs one inline layout, so it must
    // not recompute a number of distinct nodes comparable to the document. Half
    // is a deliberately loose bound; the interesting case is 60 paragraphs all
    // recomputing for one character in one of them.
    assert!(
        token.distinct < total_nodes / 2,
        "one token recomputed {} distinct nodes of {total_nodes}",
        token.distinct
    );
}

//! A reply streaming into an open project tab, token by token.
//!
//! This is the workload the application actually spends its time in, and the
//! one an idle or scroll benchmark says nothing about: text arrives every few
//! milliseconds into the last message, the paragraph reflows on each arrival,
//! and every so often the reply starts a new block, which constructs boxes
//! rather than reflowing existing ones.
//!
//! The fixture is the application's own transcript markup and its shipped
//! stylesheet, so the streaming message sits under the same flex chain and
//! percentage caps as the real one (see `transcript_frame_cost.rs`).
//!
//!   cargo test -p blitz-tests --test transcript_streaming_cost --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;
use std::time::Instant;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
const REPEATS: usize = 18;

/// Tokens in the reply. A real one runs to thousands; 400 is enough to see the
/// per-token cost and whether it grows as the message does.
const TOKENS: usize = 400;
/// A new paragraph every this many tokens, which is what a reply with headings,
/// lists and code blocks does.
const TOKENS_PER_BLOCK: usize = 40;

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: local.into(),
    }
}

fn project_tab() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let panes = markup.repeat(REPEATS);
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">
               {panes}
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

fn summarise(label: &str, mut frames_us: Vec<u128>) -> f64 {
    frames_us.sort_unstable();
    let n = frames_us.len();
    let mean = frames_us.iter().sum::<u128>() as f64 / n as f64;
    println!(
        "{label:<26} n={n:<4} mean={:.2}ms p50={:.2}ms p95={:.2}ms worst={:.2}ms  over-8.33ms={}",
        mean / 1000.0,
        frames_us[n / 2] as f64 / 1000.0,
        frames_us[(n * 95) / 100] as f64 / 1000.0,
        *frames_us.last().unwrap() as f64 / 1000.0,
        frames_us.iter().filter(|us| **us > 8_330).count(),
    );
    mean / 1000.0
}

#[test]
fn a_reply_streaming_into_an_open_tab() {
    let mut doc = project_tab();
    let total_before = doc.inner().tree().len();

    // The message being written into: the last paragraph of the transcript.
    let paragraphs = doc.inner().query_selector_all("p").unwrap().to_vec();
    let mut block = *paragraphs.last().expect("the transcript has paragraphs");
    let container = doc.inner().get_node(block).unwrap().parent.unwrap();

    let mut text_frames: Vec<u128> = Vec::new();
    let mut block_frames: Vec<u128> = Vec::new();
    let mut computed_total = 0u64;
    let mut lookups_total = 0u64;
    let mut hits_total = 0u64;
    let mut first_half = 0.0;

    for token in 0..TOKENS {
        let starts_block = token > 0 && token % TOKENS_PER_BLOCK == 0;

        {
            let inner = &mut *doc.inner_mut();
            let mut mutator = inner.mutate();
            if starts_block {
                // A new paragraph: box construction, not just reflow.
                let new_block = mutator.create_element(qname("p"), Vec::new());
                mutator.append_children(container, &[new_block]);
                block = new_block;
            }
            let text = mutator.create_text_node("token ");
            mutator.append_children(block, &[text]);
        }

        let started = Instant::now();
        doc.inner_mut().resolve(0.0);
        let elapsed = started.elapsed().as_micros();

        if starts_block {
            block_frames.push(elapsed);
        } else {
            text_frames.push(elapsed);
        }

        let c = layout_counters::last();
        computed_total += c.computed;
        lookups_total += c.lookups;
        hits_total += c.hits;

        if token == TOKENS / 2 {
            first_half =
                text_frames.iter().sum::<u128>() as f64 / text_frames.len() as f64 / 1000.0;
        }
    }

    let total_after = doc.inner().tree().len();
    println!(
        "\n== a reply streaming into a real tab: {TOKENS} tokens, {total_before} -> {total_after} nodes ==",
    );
    let text_mean = summarise("token into a paragraph", text_frames.clone());
    summarise("token that opens a block", block_frames);
    println!(
        "cache hits={:.1}%   computed per token={:.1}\n\
         drift: first half {:.2}ms, whole run {:.2}ms\n",
        (hits_total as f64 / lookups_total as f64) * 100.0,
        computed_total as f64 / TOKENS as f64,
        first_half,
        text_mean,
    );

    // A token lands in one paragraph. The cost of adding it must not grow with
    // the length of the reply, which is the shape that turns a long answer into
    // a stall.
    assert!(
        text_mean < first_half * 2.0 + 1.0,
        "per-token cost grew from {first_half:.2}ms to {text_mean:.2}ms as the reply got longer"
    );
}

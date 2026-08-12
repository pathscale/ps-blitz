//! Fast scrolling on a real project tab, frame by frame.
//!
//! Scrolling is the interaction that actually shows frame cost: it produces a
//! frame per event with no chance to coalesce, over a document that is mostly
//! text the renderer has to keep re-clipping. This drives it the way a trackpad
//! flick does, against the application's own transcript markup and its shipped
//! stylesheet (see `transcript_frame_cost.rs` for what the fixture is).
//!
//! Timing is wall clock around `resolve`, which is the honest measure here: it
//! includes the phases the per-frame line breaks out and the ones it does not.
//! Paint is not in it, so this is layout-side cost only.
//!
//!   cargo test -p blitz-tests --test transcript_scroll_cost --features counters -- --nocapture

#![cfg(feature = "counters")]

use blitz_dom::layout_counters;
use blitz_dom::{Document as _, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;
use std::time::Instant;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
const REPEATS: usize = 18;

/// A flick, not a nudge: 120 frames of 40px, which is a fast trackpad scroll
/// held for two seconds.
const FRAMES: usize = 120;
const PIXELS_PER_FRAME: f64 = 40.0;

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

#[test]
fn a_fast_scroll_through_a_real_transcript() {
    let mut doc = project_tab();
    let total = doc.inner().tree().len();

    // The pane that scrolls, by the label the application gives it.
    let scroller = doc
        .inner()
        .query_selector("section")
        .unwrap()
        .expect("no scroll container");

    let mut frames_us: Vec<u128> = Vec::with_capacity(FRAMES);
    let mut computed_total = 0u64;
    let mut cleared_total = 0u64;
    let mut lookups_total = 0u64;
    let mut hits_total = 0u64;

    for _ in 0..FRAMES {
        doc.inner_mut()
            .scroll_nearest_container_by(scroller, 0.0, -PIXELS_PER_FRAME);

        let started = Instant::now();
        doc.inner_mut().resolve(0.0);
        frames_us.push(started.elapsed().as_micros());

        let c = layout_counters::last();
        computed_total += c.computed;
        cleared_total += c.caches_cleared;
        lookups_total += c.lookups;
        hits_total += c.hits;
    }

    frames_us.sort_unstable();
    let mean = frames_us.iter().sum::<u128>() as f64 / FRAMES as f64;
    let p50 = frames_us[FRAMES / 2] as f64;
    let p95 = frames_us[(FRAMES * 95) / 100] as f64;
    let worst = *frames_us.last().unwrap() as f64;
    let hit_rate = if lookups_total == 0 {
        100.0
    } else {
        (hits_total as f64 / lookups_total as f64) * 100.0
    };

    println!(
        "\n== fast scroll, {FRAMES} frames of {PIXELS_PER_FRAME}px over {total} nodes ==\n\
         resolve   mean={:.2}ms  p50={:.2}ms  p95={:.2}ms  worst={:.2}ms\n\
         per frame computed={:.1}  cleared={:.1}  cache hits={:.1}%\n\
         budget    {} frames over 8.33ms (120Hz), {} over 16.7ms (60Hz)\n",
        mean / 1000.0,
        p50 / 1000.0,
        p95 / 1000.0,
        worst / 1000.0,
        computed_total as f64 / FRAMES as f64,
        cleared_total as f64 / FRAMES as f64,
        hit_rate,
        frames_us.iter().filter(|us| **us > 8_330).count(),
        frames_us.iter().filter(|us| **us > 16_700).count(),
    );

    // A scroll moves a scroll offset. Nothing about it changes a computed
    // style or a box, so the layout tree should be reused wholesale.
    assert!(
        (computed_total as f64 / FRAMES as f64) < 8.0,
        "a scroll frame recomputed {:.1} nodes on average of {total}",
        computed_total as f64 / FRAMES as f64
    );
}

//! What the depth axis costs to paint on a real project tab.
//!
//! Raising AgencyZero's "depth" slider sets `--az-glass-shadow`, which turns on
//! `box-shadow: 0 8px 32px` for every `.rounded-panel` at once. Doing that on a
//! live window blanked it for several seconds, and it only came back after a
//! scroll forced the scene to change.
//!
//! Blank-then-recover has two candidate explanations and they want different
//! fixes: frames that are merely slow, or frames the backend fails to produce.
//! This measures the first, on the application's own markup and its own
//! shipped stylesheet, so the answer is a number rather than an opinion.
//!
//!   cargo test -p blitz-tests --test glass_depth_cost -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
/// Fewer panes than `transcript_frame_cost` uses: this measures paint, which is
/// whole-window every frame, not layout caching.
const REPEATS: usize = 6;

fn project_tab(depth: &str) -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let panes = markup.repeat(REPEATS);
    // Exactly what the slider writes, on the same element it writes it to.
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0; --az-glass-shadow:{depth}">
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

/// Median of several paints, so one scheduling hiccup cannot carry the result.
fn paint_cost(doc: &mut HtmlDocument) -> Duration {
    let mut samples: Vec<Duration> = (0..5)
        .map(|_| {
            let started = Instant::now();
            let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
                |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
                WIDTH,
                HEIGHT,
            );
            started.elapsed()
        })
        .collect();
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
fn raising_depth_does_not_blow_up_the_frame() {
    let mut flat = project_tab("0");
    let mut deep = project_tab("0.6");

    let flat_cost = paint_cost(&mut flat);
    let deep_cost = paint_cost(&mut deep);
    let ratio = deep_cost.as_secs_f64() / flat_cost.as_secs_f64();

    println!(
        "\n== depth axis, {} nodes ==\n  depth 0    {:>8.1?}\n  depth 0.6  {:>8.1?}\n  ratio      {ratio:>8.1}x\n",
        flat.tree().len(),
        flat_cost,
        deep_cost,
    );

    // Deliberately loose. This is not a performance budget, it is a trap for
    // the pathological case: a shadow that costs a small multiple is a tuning
    // question, one that costs an order of magnitude is the bug the window was
    // showing.
    assert!(
        ratio < 4.0,
        "turning on the depth axis made a frame {ratio:.1}x more expensive"
    );
}

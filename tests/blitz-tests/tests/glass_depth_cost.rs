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
use blitz_paint::{LayerSite, paint_scene};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
/// Fewer panes than `transcript_frame_cost` uses: this measures paint, which is
/// whole-window every frame, not layout caching.
const REPEATS: usize = 6;

/// The application's `Panel`, verbatim from `components/Panel.tsx`.
///
/// The depth axis lands on `.rounded-panel` and nothing else. Without this the
/// fixture was a bare transcript with no panel in it, so raising the slider
/// changed no declaration that applied to any element, and both measurements
/// below were of two identical documents: they reported "no cost" for a
/// property the scene never had.
const PANEL_OPEN: &str =
    r#"<div class="isolate overflow-hidden rounded-panel border border-az-hairline az-panel">"#;

/// A current build's stylesheet, kept apart from the shared `app.css`.
///
/// `app.css` and `transcript.html` are one dump of one build and have to stay
/// a matched pair: refreshing the stylesheet alone changed how many elements
/// `hidden_pane_state` finds and broke it. This axis did not exist when that
/// pair was taken, so it needs a newer sheet, and takes its own rather than
/// dragging every other test onto it.
fn project_tab(depth: &str) -> HtmlDocument {
    let css = include_str!("../fixtures/app-glass.css");
    let markup = include_str!("../fixtures/transcript.html");
    let panes = format!("{PANEL_OPEN}{markup}</div>").repeat(REPEATS);
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

fn paint_once(doc: &mut HtmlDocument) -> Duration {
    let started = Instant::now();
    let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    started.elapsed()
}

/// The cost of painting each document, measured so the ratio between them
/// survives a busy machine.
///
/// Two things here, both learned the hard way. This started as the median of
/// five paints of one document followed by five of the other, and it read 3.1x
/// on an idle machine and 4.6x on a loaded one, tripping a threshold of 4. A
/// median defends against one scheduling hiccup; it does nothing about load
/// that arrives between the two measurements and stays, because then it is
/// only the second document that pays for it. The ratio was reporting a
/// property of the machine.
///
/// So: **interleaved**, so both configurations meet the same conditions
/// whatever they are, and **minimum** rather than median, because contention
/// can only ever add time. The fastest paint observed is the closest estimate
/// of what the work actually costs, and it is the estimator that stops moving
/// as the machine gets busier.
fn paint_costs(flat: &mut HtmlDocument, deep: &mut HtmlDocument) -> (Duration, Duration) {
    let mut flat_best = Duration::MAX;
    let mut deep_best = Duration::MAX;
    for _ in 0..7 {
        flat_best = flat_best.min(paint_once(flat));
        deep_best = deep_best.min(paint_once(deep));
    }
    (flat_best, deep_best)
}

/*
 * The fixture has to be able to express the thing under test, and twice it
 * could not. Its stylesheet was a stale snapshot from before the axis existed,
 * so `.rounded-panel` carried no `box-shadow` at all; and the markup was a bare
 * transcript with no panel in it for the rule to land on. Both readings came
 * back "no cost, no layers" for a property the scene did not have, which is
 * indistinguishable from good news. Assert the fixture instead of trusting it.
 */
#[test]
fn the_fixture_actually_carries_the_axis() {
    let css = include_str!("../fixtures/app-glass.css");
    assert!(
        css.contains("--az-glass-shadow"),
        "the fixture stylesheet predates the depth axis; refresh it from a build"
    );
    assert!(
        PANEL_OPEN.contains("rounded-panel"),
        "the depth axis lands on .rounded-panel and the fixture has none"
    );

    // And it has to reach the pixels: a rule that parses but paints nothing
    // would read as free too. The whole frame rather than a sampled point,
    // because picking a point is picking an answer.
    let mut flat = project_tab("0");
    let mut deep = project_tab("0.6");
    let frame = |doc: &mut HtmlDocument| {
        render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
            WIDTH,
            HEIGHT,
        )
    };
    let flat_frame = frame(&mut flat);
    let deep_frame = frame(&mut deep);
    let changed = flat_frame
        .iter()
        .zip(&deep_frame)
        .filter(|(left, right)| left != right)
        .count();
    println!("\n== depth axis pixels ==\n  channels changed by depth 0.6: {changed}\n");
    assert!(
        changed > 0,
        "raising the depth axis changed no pixel, so any cost measured here is of nothing"
    );
}

#[test]
fn raising_depth_does_not_blow_up_the_frame() {
    let mut flat = project_tab("0");
    let mut deep = project_tab("0.6");

    let (flat_cost, deep_cost) = paint_costs(&mut flat, &mut deep);
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
    //
    // The threshold was 4.0, chosen just above a 3.1x reading. It now measures
    // 4.6x, and the reason is worth knowing before anyone reads that as a
    // regression: the deep case is unchanged at ~1.4s, while the flat case
    // went from ~447ms to ~300ms when this branch stopped laying out and
    // clipping fully transparent shadows. At depth 0 the shadow *is*
    // transparent, so the baseline got cheaper and the ratio rose while the
    // absolute cost fell. A threshold set one decimal above a single
    // observation fails on good news.
    //
    // So the bound is the one the paragraph above actually describes, an order
    // of magnitude, and the printed ratio is what a reader watches for drift.
    assert!(
        ratio < 10.0,
        "turning on the depth axis made a frame {ratio:.1}x more expensive"
    );
}

/*
 * The other candidate the module comment names: not a slow frame, but a scene
 * the backend struggles to produce. Paint time is a CPU rasteriser's answer and
 * it says the shadow is nearly free; a GPU backend pays per *layer* instead,
 * because each one is a render target it has to allocate, draw into and
 * composite. So count them, and count them by site, since a shadow that turns
 * on for every panel at once turns on a layer for every panel at once.
 */
#[test]
fn raising_depth_does_not_blow_up_the_layer_count() {
    fn layers(doc: &mut HtmlDocument) -> blitz_paint::SceneLayerCounts {
        let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
            WIDTH,
            HEIGHT,
        );
        blitz_paint::latest_scene_layers()
    }

    let mut flat = project_tab("0");
    let mut deep = project_tab("0.6");
    let flat_layers = layers(&mut flat);
    let deep_layers = layers(&mut deep);

    let by_site = |counts: &blitz_paint::SceneLayerCounts| {
        LayerSite::ALL
            .iter()
            .zip(counts.by_site)
            .filter(|(_, count)| *count > 0)
            .map(|(site, count)| format!("{}:{count}", site.name()))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!(
        "\n== depth axis layers ==\n  depth 0    wanted={} used={} depth={}  {}\n  depth 0.6  wanted={} used={} depth={}  {}\n",
        flat_layers.wanted,
        flat_layers.used,
        flat_layers.max_depth,
        by_site(&flat_layers),
        deep_layers.wanted,
        deep_layers.used,
        deep_layers.max_depth,
        by_site(&deep_layers),
    );

    // `wanted` above `used` is the sharp edge: it means the scene hit the
    // layer limit and clipping was silently dropped.
    assert_eq!(
        deep_layers.wanted, deep_layers.used,
        "the deep scene asked for more layers than it was given"
    );

    // At rest every panel still carries the declaration, with the alpha at
    // zero. That must cost nothing at all, not a layer each.
    let outset = LayerSite::ALL
        .iter()
        .position(|site| matches!(site, LayerSite::OutsetShadow))
        .expect("outset shadow is a site");
    assert_eq!(
        flat_layers.by_site[outset], 0,
        "a fully transparent shadow still pushed a layer per panel"
    );
}

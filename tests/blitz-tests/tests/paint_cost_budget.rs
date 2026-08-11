//! Budgets on what painting a frame costs.
//!
//! A CPU regression reached a release because nothing here could catch one:
//! 345 tests assert what the engine draws and none assert what drawing it
//! costs. The obvious guard is a timing test, and timing tests are too noisy
//! for CI, so the guard never got written and the cost drifted unobserved.
//!
//! These assert deterministic proxies instead. Layer pushes are the one the
//! renderer pays most for — each is a `render_to_texture`, a separate
//! composite, and the largest single item in a frame on a real page — and the
//! count for a given document is exactly reproducible, so it can be asserted
//! in CI without a millisecond anywhere.
//!
//! The numbers are budgets, not truths. Raising one is a normal thing to do
//! when a feature genuinely needs more layers; doing it without noticing is
//! the failure this prevents. If a change here fails, look at whether the
//! extra layers are actually needed before you edit the number.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::{LayerSite, SceneLayerCounts, latest_scene_layers, paint_scene};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::{Arc, Mutex};

/// The layer counters are process-global, so two tests painting at once would
/// read each other's numbers.
static PAINT_LOCK: Mutex<()> = Mutex::new(());

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;

/// Paint `html` once and return what the scene asked the renderer for.
fn paint_cost(html: &str) -> SceneLayerCounts {
    let guard = PAINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );

    let counts = latest_scene_layers();
    drop(guard);
    counts
}

fn overflow_layers(counts: &SceneLayerCounts) -> u32 {
    counts.by_site[LayerSite::Overflow as usize]
}

#[test]
fn a_plain_page_pushes_no_layers() {
    // Nothing here needs clipping, compositing or an effect. A plain document
    // costing layers at all means something is pushing them unconditionally.
    let counts = paint_cost(
        "<html><body style='margin:0'>\
         <div style='width:200px;height:100px;background:#333'>text</div>\
         <p>more text</p>\
         </body></html>",
    );

    assert_eq!(
        counts.wanted, 0,
        "a page with nothing to clip or composite wanted {} layers: {counts:?}",
        counts.wanted
    );
}

#[test]
fn every_layer_asked_for_is_granted() {
    // `wanted` above `used` means the scene hit LAYER_LIMIT and clipping was
    // silently skipped, so content that should have been cut off was drawn in
    // full. That is a correctness failure that only shows up as a cost metric.
    let counts = paint_cost(
        "<html><body style='margin:0'>\
         <div style='overflow:hidden;width:100px;height:50px'>\
         <div style='width:300px;height:200px;background:red'></div>\
         </div>\
         </body></html>",
    );

    assert_eq!(
        counts.wanted,
        counts.used,
        "the scene was denied {} of {} layers, so some clipping was skipped",
        counts.wanted - counts.used,
        counts.wanted
    );
}

#[test]
fn a_clipping_box_costs_one_overflow_layer() {
    let counts = paint_cost(
        "<html><body style='margin:0'>\
         <div style='overflow:hidden;width:100px;height:50px'>\
         <div style='width:300px;height:200px;background:red'></div>\
         </div>\
         </body></html>",
    );

    assert_eq!(
        overflow_layers(&counts),
        1,
        "one clipping box cost {} overflow layers: {counts:?}",
        overflow_layers(&counts)
    );
}

#[test]
fn nested_scroll_containers_do_not_multiply_layers() {
    // Three nested clipping boxes should cost three layers, not more. Depth
    // matters on its own account: it bounds what the rasteriser holds at once.
    let counts = paint_cost(
        "<html><body style='margin:0'>\
         <div style='overflow:hidden;width:300px;height:200px'>\
         <div style='overflow:hidden;width:200px;height:150px'>\
         <div style='overflow:hidden;width:100px;height:100px'>\
         <div style='width:400px;height:400px;background:red'></div>\
         </div></div></div>\
         </body></html>",
    );

    assert_eq!(
        overflow_layers(&counts),
        3,
        "three nested clipping boxes cost {} overflow layers: {counts:?}",
        overflow_layers(&counts)
    );
    assert!(
        counts.max_depth <= 3,
        "nesting reached depth {}, deeper than the three boxes that asked for it",
        counts.max_depth
    );
}

#[test]
fn opacity_costs_one_effect_layer_not_one_per_child() {
    // An opacity group is composited once for the whole subtree. One layer per
    // child would be correct-looking and quadratic.
    let counts = paint_cost(
        "<html><body style='margin:0'>\
         <div style='opacity:0.5'>\
         <div style='background:red;width:50px;height:20px'>a</div>\
         <div style='background:red;width:50px;height:20px'>b</div>\
         <div style='background:red;width:50px;height:20px'>c</div>\
         <div style='background:red;width:50px;height:20px'>d</div>\
         </div></body></html>",
    );

    assert_eq!(
        counts.by_site[LayerSite::Effect as usize],
        1,
        "an opacity group over four children cost {} effect layers: {counts:?}",
        counts.by_site[LayerSite::Effect as usize]
    );
}

#[test]
fn a_page_of_ordinary_content_stays_within_its_layer_budget() {
    // The shape of a real page: sections, a header, cards. None of it asks for
    // clipping or compositing, so none of it should cost a layer. The number
    // is a budget with headroom, not a measurement to match exactly.
    let mut html = String::from("<html><body style='margin:0'><header>nav</header>");
    for i in 0..30 {
        html.push_str(&format!(
            "<section style='padding:8px'><h2>Heading {i}</h2>\
             <p>Some paragraph text that wraps across a couple of lines.</p>\
             <div style='display:flex'><span>a</span><span>b</span></div>\
             </section>"
        ));
    }
    html.push_str("</body></html>");

    let counts = paint_cost(&html);

    assert!(
        counts.wanted <= 2,
        "30 sections of ordinary content wanted {} layers, budget 2: {counts:?}",
        counts.wanted
    );
}

/// Wall-clock cost of resolve and paint, for local investigation.
///
/// Ignored by default and deliberately not a criterion bench: criterion is a
/// heavy dev-dependency that would be compiled on every `cargo test`, and
/// timing has no place in CI here — it is far too noisy to fail a build on,
/// which is exactly why the budgets above assert layer counts instead.
///
/// This is for the other half of the question, the part budgets cannot see: a
/// constant-factor slowdown that pushes no extra layers.
///
/// Read the paint number as a relative signal only. It rasterises on the CPU,
/// where the windowed app hands the encoded scene to the GPU, so it is not the
/// same quantity as `renderer_avg_ms` in the frame log and is much larger.
/// What it is good for is telling you the direction a change moved.
///
/// The resolve number is the one worth staring at: nothing changes between
/// iterations, so in principle it should cost nothing at all. Run it with
///
///     cargo test -p blitz-tests --test paint_cost_budget -- --ignored --nocapture
#[test]
#[ignore = "timing, run explicitly"]
fn report_frame_cost() {
    use std::time::Instant;

    let mut html = String::from("<html><body style='margin:0'><header>nav</header>");
    for i in 0..200 {
        html.push_str(&format!(
            "<section style='padding:8px'><h2>Heading {i}</h2>\
             <p>Some paragraph text that wraps across a couple of lines.</p>\
             <div style='display:flex'><span>a</span><span>b</span></div>\
             </section>"
        ));
    }
    html.push_str("</body></html>");

    let guard = PAINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    // The steady state an animation frame is in: the tree is already built and
    // laid out, and every frame re-resolves and repaints it unchanged. A cold
    // first pass would measure parsing and box construction instead, which is
    // not what runs 30 times a second.
    const ITERATIONS: u32 = 30;
    let mut resolve_total = std::time::Duration::ZERO;
    let mut paint_total = std::time::Duration::ZERO;

    for _ in 0..ITERATIONS {
        let started = Instant::now();
        doc.resolve(0.0);
        resolve_total += started.elapsed();

        let started = Instant::now();
        let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, doc.as_mut(), 1.0, WIDTH, HEIGHT, 0, 0),
            WIDTH,
            HEIGHT,
        );
        paint_total += started.elapsed();
    }

    let counts = latest_scene_layers();
    drop(guard);

    let n = f64::from(ITERATIONS);
    println!(
        "\nsteady-state frame over {ITERATIONS} iterations:\n  \
         resolve {:.3}ms  paint+encode {:.3}ms  total {:.3}ms\n  \
         layers wanted={} used={} depth={}\n",
        resolve_total.as_secs_f64() * 1000.0 / n,
        paint_total.as_secs_f64() * 1000.0 / n,
        (resolve_total + paint_total).as_secs_f64() * 1000.0 / n,
        counts.wanted,
        counts.used,
        counts.max_depth,
    );
}

/// Paint `html` and return the pixel at (x, y) as RGB.
fn pixel_at(html: &str, x: usize, y: usize) -> [u8; 3] {
    let guard = PAINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    drop(guard);
    let idx = (y * WIDTH as usize + x) * 4;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// The safety net for skipping clip layers that clip nothing.
///
/// The budgets above count layers, and a count cannot tell you whether the
/// content was actually cut off. Skipping a layer that was doing real work
/// draws the overflowing part in full, which is a visible defect that no
/// layer count would report. These check the pixels.
#[test]
fn overflowing_content_is_still_clipped() {
    // A 100x50 clipping box holding a 300x200 red child. Everything below and
    // right of the box must stay white.
    let html = "<html><body style='margin:0;background:#fff'>\
                <div style='overflow:hidden;width:100px;height:50px'>\
                <div style='width:300px;height:200px;background:#ff0000'></div>\
                </div></body></html>";

    assert_eq!(
        pixel_at(html, 50, 25),
        [255, 0, 0],
        "inside the clip box should be the child's red"
    );
    assert_eq!(
        pixel_at(html, 150, 25),
        [255, 255, 255],
        "content to the right of a clipping box was not clipped"
    );
    assert_eq!(
        pixel_at(html, 50, 100),
        [255, 255, 255],
        "content below a clipping box was not clipped"
    );
}

#[test]
fn a_scrolled_container_still_clips() {
    // Scrolled containers must still clip. Note this does not isolate the
    // scroll guard in the skip condition: content tall enough to scroll also
    // fails the fits-the-box test, so the layer is kept either way. The guard
    // is there for the case a fits-the-box element still carries a stale
    // offset — content shrinking under a scrolled container, which is what
    // clamp_scroll_offsets exists to correct — and it is cheap defensiveness
    // rather than something this test proves.
    let html = "<html><body style='margin:0;background:#fff'>\
                <div id='s' style='overflow:scroll;width:100px;height:50px'>\
                <div style='width:80px;height:400px;background:#ff0000'></div>\
                </div></body></html>";

    assert_eq!(
        pixel_at(html, 50, 100),
        [255, 255, 255],
        "content below a scroll container was not clipped"
    );
}

#[test]
fn a_rounded_clip_still_cuts_its_corners() {
    // Content fits the rectangle exactly, so the rectangular test says the clip
    // is unnecessary — but a rounded clip still has to cut the corners.
    let html = "<html><body style='margin:0;background:#fff'>\
                <div style='overflow:hidden;border-radius:40px;width:80px;height:80px'>\
                <div style='width:80px;height:80px;background:#ff0000'></div>\
                </div></body></html>";

    assert_eq!(
        pixel_at(html, 1, 1),
        [255, 255, 255],
        "the corner of a rounded clipping box was not cut"
    );
    assert_eq!(
        pixel_at(html, 40, 40),
        [255, 0, 0],
        "the middle of a rounded clipping box should still be painted"
    );
}

/// Cost of painting a long page scrolled near its end.
///
/// This is the case the viewport cull exists for and the one the frame budget
/// above cannot see: everything above the viewport should be discarded before
/// it is encoded. Run with the same command as `report_frame_cost`.
#[test]
#[ignore = "timing, run explicitly"]
fn report_scrolled_frame_cost() {
    use std::time::Instant;

    let mut html = String::from("<html><body style='margin:0'>");
    for i in 0..300 {
        html.push_str(&format!(
            "<section style='padding:8px;height:60px'><h2>Heading {i}</h2>\
             <p>Some paragraph text that wraps across a couple of lines.</p>\
             </section>"
        ));
    }
    html.push_str("</body></html>");

    let guard = PAINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    // Near the bottom, so almost the whole document is above the viewport.
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 20000.0 });
    doc.resolve(0.0);

    const ITERATIONS: u32 = 20;
    let mut paint_total = std::time::Duration::ZERO;
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        let _ = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, doc.as_mut(), 1.0, WIDTH, HEIGHT, 0, 0),
            WIDTH,
            HEIGHT,
        );
        paint_total += started.elapsed();
    }
    drop(guard);

    // Measured with temporary counters at the cull site: 17 elements painted,
    // 297 culled. Culling is healthy, so this number is not off-screen content
    // being drawn — it is the CPU rasteriser, which is why the paint figure here
    // is only ever a relative signal.
    println!(
        "\nscrolled to the bottom of 300 sections:\n  paint+encode {:.3}ms\n",
        paint_total.as_secs_f64() * 1000.0 / f64::from(ITERATIONS),
    );
}

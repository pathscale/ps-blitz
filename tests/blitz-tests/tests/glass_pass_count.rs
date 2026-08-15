//! What a page of glass costs in render passes.
//!
//! `backdrop-filter` cannot be satisfied while a scene is being built: its
//! input is the pixels behind it, so the renderer has to stop, render what it
//! has, filter that, and carry on. Each stop is a render pass.
//!
//! The plan was to batch by backdrop root - panels at one level that do not
//! overlap all see the same thing behind them, so one snapshot serves all of
//! them and six panels cost two passes rather than seven. These tests are that
//! claim measured, and the headline is that it does not hold on AgencyZero's
//! own geometry. See `six_panels_at_the_apps_own_spacing_cannot_batch`.
//!
//! Measured through `anyrender::PlanningScene`, which paints nothing and only
//! plans, so this runs on a machine with no GPU. Pass count is a property of
//! the scene; testing it through a real adapter would trade a fact that holds
//! everywhere for one that depends on the runner.
//!
//!   cargo test -p blitz-tests --test glass_pass_count -- --nocapture

use anyrender::{FramePlan, PlanningScene};
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;
const PANELS: usize = 6;

/// The blur AgencyZero's panels ask for, in CSS pixels.
///
/// This is σ, the gaussian's standard deviation, which is what CSS `blur()`
/// takes and what the numbers below are all relative to.
const SIGMA: f64 = 12.0;

/// How far a gaussian actually reaches, in units of σ.
///
/// The filter graph declares this itself (`Filter::expansion_rect`), and it is
/// three because that is where a gaussian has essentially no weight left: past
/// 3σ under 0.3% of the kernel remains, which cannot move an 8-bit channel.
/// At 1σ, by contrast, 16% of the weight is still outside - a sixth of the
/// result - so there is no tolerance to be had by shading this number down.
const REACH: f64 = 3.0;

/// The gap between stacked panels in the settings column: Tailwind `gap-3`.
///
/// Named rather than inlined because it is the whole finding. See
/// `apps/gui/frontend/src/features/settings/SettingsTab.tsx`, the
/// `flex w-full max-w-[720px] flex-col gap-3` column that holds the Section
/// panels.
const APP_GAP: f64 = 12.0;

/// The application's `Panel`, from `components/Panel.tsx`, plus the one
/// declaration this whole exercise is about.
///
/// The `backdrop-filter` is written here rather than taken from
/// `app-glass.css` because the shipped stylesheet does not put it on a panel:
/// the only `backdrop-filter` in a current build is on `.modal__backdrop--blur`.
/// It is absent because glass does not work, which is the thing being fixed.
/// Declaring it inline is the shape the stylesheet will take, and keeping it
/// visible here means nobody reads this file as measuring something the app
/// already ships.
fn panel_open(extra: &str) -> String {
    format!(
        r#"<div class="isolate overflow-hidden rounded-panel border border-az-hairline az-panel"
             style="backdrop-filter:blur({SIGMA}px);{extra}">"#
    )
}

/// A column of glass panels over the application's own markup and stylesheet.
fn glass_column(gap: f64) -> HtmlDocument {
    let css = include_str!("../fixtures/app-glass.css");
    let markup = include_str!("../fixtures/transcript.html");
    let panel_height = HEIGHT as f64 / PANELS as f64 - gap;
    let panes = (0..PANELS)
        .map(|_| {
            format!(
                "{}{markup}</div>",
                panel_open(&format!("height:{panel_height}px"))
            )
        })
        .collect::<String>();
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; gap:{gap}px; width:{WIDTH}px; height:{HEIGHT}px;">
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

fn plan(doc: &mut HtmlDocument, y_offset: u32) -> FramePlan {
    let mut scene = PlanningScene::new();
    paint_scene(&mut scene, doc, 1.0, WIDTH, HEIGHT, 0, y_offset);
    scene.finish()
}

fn describe(label: &str, plan: &FramePlan) {
    println!(
        "  {label:<12} render passes {}  blurs {}  batch sizes {:?}",
        plan.render_passes(),
        plan.blur_passes(),
        plan.batches
            .iter()
            .map(|batch| batch.ops.len())
            .collect::<Vec<_>>(),
    );
}

/// The fixture has to actually carry glass, or every number below is of nothing.
///
/// The sibling `glass_depth_cost` test learned this the expensive way twice: a
/// stale stylesheet and then a missing panel each made it measure two identical
/// documents and report "free" for a property the scene never had.
#[test]
fn the_fixture_actually_declares_backdrop_filter() {
    let mut doc = glass_column(APP_GAP);
    let plan = plan(&mut doc, 0);
    assert_eq!(
        plan.blur_passes() as usize,
        PANELS,
        "expected one backdrop op per panel, got {plan:?}"
    );
}

/// Batching works, when the panels are far enough apart for it to be sound.
///
/// Sound means: no panel's blur reads a pixel that a previous panel in the
/// batch painted. A gaussian samples about 3σ past its own edge, so the gap has
/// to clear that. Here it does, and six panels cost two passes.
#[test]
fn panels_separated_by_more_than_the_blur_reaches_share_one_snapshot() {
    let gap = SIGMA * REACH + 1.0;
    let mut doc = glass_column(gap);

    println!("\n== {PANELS} panels, {gap}px apart, blur σ={SIGMA} ==");
    let plan = plan(&mut doc, 0);
    describe("separated", &plan);

    assert_eq!(
        plan.render_passes(),
        2,
        "panels this far apart share a snapshot, got {plan:?}"
    );
    assert_eq!(plan.batches.len(), 1);
    assert_eq!(plan.batches[0].ops.len(), PANELS);
    // Batching removes render passes, not blurs. Six panels still blur six times.
    assert_eq!(plan.blur_passes() as usize, PANELS);
}

/*
 * The finding this file exists to record.
 *
 * The design that motivated all of this said six panels cost two passes. On
 * AgencyZero's own layout they cost seven, and no batching rule can change
 * that, because the panels genuinely read each other's pixels.
 *
 * The settings column stacks its Section panels with `gap-3`, twelve pixels.
 * The blur the panels want is σ=12, which reaches 36px. So a panel's blur
 * samples 36px past its own edge, lands 24px inside its neighbour, and the
 * neighbour was painted after the snapshot the batch would share.
 *
 * There is no tolerance available. At a separation of exactly σ, 16% of the
 * gaussian's weight is still outside the gap - a sixth of every edge pixel's
 * value - and shading the reach down to make the batch fit would be trading a
 * visible error for a pass.
 *
 * What follows from it:
 *
 *   - Batching is worth keeping. It is correct, it costs nothing when it does
 *     not apply, and it pays on any layout where glass is sparse. It is simply
 *     not the lever on this app.
 *   - The lever has to be the other two multipliers. Downsampling (σ=12 at
 *     quarter resolution is σ=3 over 1/16th the texels) and caching keyed on
 *     what changed are both untouched by panel spacing, and a still frame
 *     costing zero blurs does not care how many passes a moving one costs.
 *   - If two passes are wanted for their own sake, that is an application
 *     decision with a number attached: the panels need about 37px of clear
 *     space, or the blur needs to come down to about σ=4.
 */
#[test]
fn six_panels_at_the_apps_own_spacing_cannot_batch() {
    let mut doc = glass_column(APP_GAP);

    println!("\n== {PANELS} panels at the app's own gap-3 ({APP_GAP}px), blur σ={SIGMA} ==");
    let plan = plan(&mut doc, 0);
    describe("app layout", &plan);
    println!(
        "  a σ={SIGMA} blur reaches {}px; the panels are {APP_GAP}px apart\n",
        SIGMA * REACH
    );

    assert_eq!(
        plan.render_passes(),
        PANELS as u32 + 1,
        "a gap of {APP_GAP}px is well inside a σ={SIGMA} blur's {}px reach, so \
         every panel needs its own snapshot: {plan:?}",
        SIGMA * REACH
    );
    assert_eq!(plan.blur_passes() as usize, PANELS);
}

/// Where the batch starts working, walked rather than asserted at one point.
///
/// A single-gap test says nothing about whether the rule is the blur's reach or
/// some number that happened to fit. Sweeping the gap across the reach shows the
/// transition landing where the gaussian says it should, and prints the table so
/// a reader can see it rather than take it.
#[test]
fn the_batch_threshold_is_the_blurs_reach() {
    let reach = SIGMA * REACH;
    println!("\n== gap sweep, blur σ={SIGMA}, reach {reach}px ==");
    let mut swept = Vec::new();
    for gap in [6.0, 12.0, 20.0, 30.0, 35.0, 36.0, 37.0, 48.0] {
        let mut doc = glass_column(gap);
        let plan = plan(&mut doc, 0);
        describe(&format!("gap {gap}px"), &plan);
        swept.push((gap, plan.render_passes()));
    }
    println!();

    // The transition is at the reach exactly, from both sides. A gap of exactly
    // 3σ batches, because a blur reading up to a neighbour's edge and a
    // neighbour starting at that edge share no pixel; anything narrower does
    // not. Asserting both directions is what distinguishes "the rule is the
    // filter's own expansion" from "the rule is a number that happened to fit".
    for (gap, passes) in &swept {
        let expected = if *gap >= reach { 2 } else { PANELS as u32 + 1 };
        assert_eq!(
            *passes, expected,
            "a {gap}px gap against a {reach}px reach should cost {expected} passes"
        );
    }
}

/// Scrolling moves the panels. It must not change what they cost.
///
/// A batching rule keyed on anything positional - a node id, a paint index, a
/// cached rect - can silently come apart the moment the content moves, and a
/// scrolling frame is the one that matters most, because it is the frame a user
/// is looking at while they judge whether the window is smooth.
#[test]
fn scrolling_does_not_change_the_pass_count() {
    for gap in [APP_GAP, SIGMA * REACH + 1.0] {
        let mut doc = glass_column(gap);

        println!("\n== scrolling, {PANELS} panels {gap}px apart ==");
        let still = plan(&mut doc, 0);
        describe("still", &still);

        for y_offset in [1, 37, 150, 400] {
            let scrolled = plan(&mut doc, y_offset);
            describe(&format!("y={y_offset}"), &scrolled);
            assert_eq!(
                scrolled.render_passes(),
                still.render_passes(),
                "scrolling to y={y_offset} changed the pass count: {scrolled:?}"
            );
            assert_eq!(
                scrolled.blur_passes(),
                still.blur_passes(),
                "scrolling to y={y_offset} changed the blur count: {scrolled:?}"
            );
        }
    }
    println!();
}

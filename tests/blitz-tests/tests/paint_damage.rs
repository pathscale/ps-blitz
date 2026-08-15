//! Which parts of the document differ between frames.
//!
//! The question a `backdrop-filter` cache has to ask. Blurring what is behind
//! an element costs a render pass and a filter every frame, and the only way
//! that stops being a permanent cost is skipping the elements whose input did
//! not change. Until now nothing could answer: Stylo's damage is an input to
//! layout and `resolve` clears it before paint, so a painter sees a document
//! that is uniformly undamaged.
//!
//! The assertion the whole design rests on is
//! `a_still_frame_reports_nothing_changed`. If a window at rest reports damage,
//! every cache built on this is worthless and it is better to know here.
//!
//!   cargo test -p blitz-tests --test paint_damage -- --nocapture

use blitz_dom::kurbo::Rect;
use blitz_dom::{DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

/// Rewrite an element's inline style and resolve.
fn restyle(doc: &mut HtmlDocument, id: &str, style: &str) {
    let node = node_id(doc, id);
    let mut mutator = doc.mutate();
    mutator.set_attribute(node, attr_name("style"), style);
    drop(mutator);
    doc.resolve(0.0);
}

const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;

/// Two boxes far apart, so "changed here" and "changed there" are separable.
///
/// A single-element fixture cannot tell a working region apart from one that
/// always answers the whole document, which is the failure mode that matters:
/// it is invisible, and it makes every cache a permanent miss.
const HTML: &str = r#"<html><body style="margin:0">
  <div id="top" style="position:absolute;left:0;top:0;width:200px;height:100px;background:#111">
    <span id="text">short</span>
  </div>
  <div id="bottom" style="position:absolute;left:0;top:400px;width:200px;height:100px;background:#222"></div>
</body></html>"#;

fn document() -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc.set_paint_damage_tracking(true);
    // The first tracked frame has no previous frame to compare against, so it
    // reports everything. Settle past it before measuring anything.
    doc.resolve(0.0);
    doc.resolve(0.0);
    doc
}

/// Where `#top` and `#bottom` are, in the document space damage is reported in.
const TOP: Rect = Rect::new(0.0, 0.0, 200.0, 100.0);
const BOTTOM: Rect = Rect::new(0.0, 400.0, 200.0, 500.0);

fn node_id(doc: &HtmlDocument, id: &str) -> blitz_dom::NodeId {
    doc.query_selector(&format!("#{id}"))
        .expect("valid selector")
        .unwrap_or_else(|| panic!("no #{id} in the fixture"))
}

fn describe(label: &str, doc: &HtmlDocument) {
    println!(
        "  {label:<24} generation {}  regions {:?}",
        doc.paint_damage().generation,
        doc.paint_damage().regions(),
    );
}

/// Off by default, and an untracked document must not look like a clean one.
///
/// The dangerous reading is "no regions, so nothing changed, so keep the
/// cache". A consumer has to check that the question is being answered before
/// it trusts the answer, and this is the shape of the trap.
#[test]
fn tracking_is_off_until_it_is_asked_for() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    assert!(!doc.paint_damage_tracking());
    assert!(
        doc.paint_damage().is_empty(),
        "an untracked document reports nothing, which is not the same as clean"
    );

    doc.set_paint_damage_tracking(true);
    assert!(doc.paint_damage_tracking());
    doc.resolve(0.0);
    assert!(
        !doc.paint_damage().is_empty(),
        "the first tracked frame has no predecessor, so everything is new"
    );
}

/*
 * The load-bearing test.
 *
 * A window at rest has to report nothing. Everything downstream - caching a
 * blurred backdrop, skipping a render pass, a still frame costing zero blurs -
 * is worth exactly nothing if resolving an unchanged document reports damage,
 * and a mechanism that quietly always reports damage looks identical to a
 * working one from the outside.
 */
#[test]
fn a_still_frame_reports_nothing_changed() {
    let mut doc = document();

    for frame in 0..5 {
        doc.resolve(0.0);
        describe(&format!("still frame {frame}"), &doc);
        assert!(
            doc.paint_damage().is_empty(),
            "resolving an unchanged document reported damage on frame {frame}: {:?}",
            doc.paint_damage().regions()
        );
    }
}

/// A change reports where it was, and only where it was.
#[test]
fn changing_one_element_does_not_damage_a_distant_one() {
    let mut doc = document();

    restyle(
        &mut doc,
        "top",
        "position:absolute;left:0;top:0;width:200px;height:100px;background:#f00",
    );

    describe("recoloured #top", &doc);
    assert!(
        doc.paint_damage().intersects(TOP),
        "the element that changed must be in the region: {:?}",
        doc.paint_damage().regions()
    );
    assert!(
        !doc.paint_damage().intersects(BOTTOM),
        "an element 400px away did not change: {:?}",
        doc.paint_damage().regions()
    );
}

/// Growing text damages the box it grew, not the whole page.
///
/// The case the application actually lives in: a transcript streaming tokens.
/// If that reports full-frame damage then every glass panel re-blurs on every
/// token, which is the permanent cost this exists to remove.
#[test]
fn growing_text_damages_its_own_box_only() {
    let mut doc = document();

    let text = node_id(&doc, "text");
    let child = doc.get_node(text).expect("the span is live").children[0];
    let mut mutator = doc.mutate();
    mutator.set_node_text(child, "a much longer run of text than before");
    drop(mutator);
    doc.resolve(0.0);

    describe("grew #text", &doc);
    assert!(
        doc.paint_damage().intersects(TOP),
        "the text that changed is inside #top: {:?}",
        doc.paint_damage().regions()
    );
    assert!(
        !doc.paint_damage().intersects(BOTTOM),
        "a box 400px below an absolutely positioned sibling cannot have moved: {:?}",
        doc.paint_damage().regions()
    );
}

/// A removed element damages the space it vacated.
///
/// Nothing else in the pass can see it: it is gone from the tree, so no walk
/// over live nodes reaches it. Missing this is the failure that leaves a stale
/// blur showing content that is no longer on the page.
#[test]
fn removing_an_element_damages_where_it_was() {
    let mut doc = document();

    let bottom = node_id(&doc, "bottom");
    let mut mutator = doc.mutate();
    mutator.remove_node(bottom);
    drop(mutator);
    doc.resolve(0.0);

    describe("removed #bottom", &doc);
    assert!(
        doc.paint_damage().intersects(BOTTOM),
        "the vacated space must be damaged: {:?}",
        doc.paint_damage().regions()
    );
}

/// Moving an element damages both ends of the move.
#[test]
fn moving_an_element_damages_the_space_it_left() {
    let mut doc = document();

    restyle(
        &mut doc,
        "bottom",
        "position:absolute;left:0;top:200px;width:200px;height:100px;background:#222",
    );

    describe("moved #bottom up", &doc);
    let moved_to = Rect::new(0.0, 200.0, 200.0, 300.0);
    assert!(
        doc.paint_damage().intersects(BOTTOM),
        "the space it left must be damaged: {:?}",
        doc.paint_damage().regions()
    );
    assert!(
        doc.paint_damage().intersects(moved_to),
        "the space it took must be damaged: {:?}",
        doc.paint_damage().regions()
    );
}

/*
 * What it costs, on a document the size of a real one.
 *
 * The requirement this was built under is that glass must not become a
 * per-frame CPU cost, so a mechanism whose whole purpose is removing per-frame
 * work has to be cheap itself or it has just moved the bill.
 *
 * Interleaved and minimum rather than averaged, for the reason
 * `glass_depth_cost` records: load arriving partway through and staying is paid
 * only by whichever configuration ran second, and a ratio taken that way
 * reports a property of the machine. Contention can only add time, so the
 * fastest resolve observed is the closest estimate of what the work costs.
 */
#[test]
fn tracking_does_not_meaningfully_slow_a_resolve() {
    use std::time::{Duration, Instant};

    fn transcript(tracking: bool) -> HtmlDocument {
        let css = include_str!("../fixtures/app-glass.css");
        let markup = include_str!("../fixtures/transcript.html");
        let html = format!(
            r#"<html><head><style>{css}</style></head>
               <body class="bg-base-100" style="margin:0">{}</body></html>"#,
            markup.repeat(6)
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
        doc.set_paint_damage_tracking(tracking);
        doc.resolve(0.0);
        doc
    }

    let mut off = transcript(false);
    let mut on = transcript(true);
    let nodes = on.tree().len();

    let mut off_best = Duration::MAX;
    let mut on_best = Duration::MAX;
    for _ in 0..9 {
        let started = Instant::now();
        off.resolve(0.0);
        off_best = off_best.min(started.elapsed());

        let started = Instant::now();
        on.resolve(0.0);
        on_best = on_best.min(started.elapsed());
    }

    let overhead = on_best.saturating_sub(off_best);
    println!(
        "\n== paint damage cost, {nodes} nodes ==\n  \
         off {off_best:>10.1?}\n  on  {on_best:>10.1?}\n  \
         added {overhead:>8.1?} ({:.0} ns/node)\n",
        overhead.as_nanos() as f64 / nodes as f64,
    );

    // A still resolve on this document is already sub-millisecond, so a ratio
    // would be measuring noise against noise. The bound that matters is
    // absolute and per node: the pass is one hash lookup and one rectangle
    // comparison each, and anything an order of magnitude past that means the
    // memo is not working and every node is walking to the root.
    let per_node = overhead.as_nanos() as f64 / nodes as f64;
    assert!(
        per_node < 500.0,
        "paint damage tracking added {per_node:.0} ns per node, which is not a \
         hash lookup and a comparison"
    );
}

/// Turning tracking off and on again must not carry a stale previous frame.
///
/// The map of last frame's boxes is what every comparison is against. Keeping
/// it across a disabled stretch would compare this frame to one from before
/// however much happened in between, and report clean regions that are not.
#[test]
fn re_enabling_starts_from_no_previous_frame() {
    let mut doc = document();

    doc.set_paint_damage_tracking(false);
    let bottom = node_id(&doc, "bottom");
    let mut mutator = doc.mutate();
    mutator.remove_node(bottom);
    drop(mutator);
    doc.resolve(0.0);

    doc.set_paint_damage_tracking(true);
    doc.resolve(0.0);
    describe("re-enabled", &doc);
    assert!(
        !doc.paint_damage().is_empty(),
        "the first frame after re-enabling has no predecessor"
    );

    doc.resolve(0.0);
    assert!(
        doc.paint_damage().is_empty(),
        "and the one after it settles: {:?}",
        doc.paint_damage().regions()
    );
}

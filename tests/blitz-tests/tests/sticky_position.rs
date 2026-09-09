//! `position: sticky` has to pin a box inside the scrollport it lives in.
//!
//! `stylo_taffy` mapped `Position::Sticky` onto `taffy::Position::Relative`, so
//! a sticky box laid out in flow and then scrolled away like any other. Every
//! sticky navbar and document sidebar on the fleet scrolled off the top of
//! documents 5,000 to 16,700px tall.
//!
//! Relative is the right *layout* answer: a sticky box takes its flow position
//! and reserves its space there. What was missing is the adjustment on top of
//! it, which the tests below pin down: nothing before the threshold, a pin
//! after it, and a release when the containing block leaves.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const VIEWPORT: (u32, u32) = (1000, 700);

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(
                VIEWPORT.0,
                VIEWPORT.1,
                1.0,
                ColorScheme::Light,
            )),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// Scroll the viewport down by `amount` CSS pixels.
///
/// `scroll_viewport_by` takes a wheel delta, which points the other way.
fn scroll_down(doc: &mut HtmlDocument, amount: f64) {
    doc.scroll_viewport_by(0.0, -amount);
}

#[track_caller]
fn origin(doc: &HtmlDocument, id: &str) -> (f32, f32) {
    let node_id = doc
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("no element with id {id}"));
    let position = doc.tree()[node_id].absolute_position(0.0, 0.0);
    (position.x, position.y)
}

/// A sticky box that has not reached its threshold sits exactly where a
/// `position: relative` box with no insets would: at its flow position.
///
/// The insets are a threshold, not an offset. Taffy applies the inset of a
/// `Relative` box as a displacement, so `position:sticky;top:24px` was drawn
/// 24px below its flow position before anything had scrolled at all.
#[test]
fn an_unreached_threshold_leaves_the_box_in_flow() {
    let doc = document(
        r#"<html><body style="margin:0">
            <div style="height:100px"></div>
            <div id="header" style="position:sticky;top:24px;height:50px"></div>
            <div style="height:3000px"></div>
        </body></html>"#,
    );

    assert_eq!(
        origin(&doc, "header"),
        (0.0, 100.0),
        "an inset is a threshold, not a relative displacement"
    );
}

/// The headline case: a sticky header on a document taller than the viewport.
#[test]
fn a_sticky_header_pins_once_the_page_scrolls_past_it() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <header id="header" style="position:sticky;top:0;height:50px"></header>
            <main style="height:5000px"></main>
        </body></html>"#,
    );

    assert_eq!(origin(&doc, "header"), (0.0, 0.0));

    scroll_down(&mut doc, 800.0);
    doc.resolve(0.0);

    // Document coordinates: paint subtracts the viewport scroll, so a box that
    // stays at the top of the screen has to track it.
    assert_eq!(
        origin(&doc, "header"),
        (0.0, 800.0),
        "a sticky header must stay at the top of the scrollport"
    );

    scroll_down(&mut doc, 700.0);
    doc.resolve(0.0);
    assert_eq!(origin(&doc, "header"), (0.0, 1500.0));
}

/// A `top` inset is a distance from the top of the scrollport, so the pinned
/// position is the scroll offset plus the inset.
#[test]
fn a_top_inset_offsets_the_pinned_position() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <div id="header" style="position:sticky;top:24px;height:50px"></div>
            <div style="height:5000px"></div>
        </body></html>"#,
    );

    scroll_down(&mut doc, 600.0);
    doc.resolve(0.0);

    assert_eq!(origin(&doc, "header"), (0.0, 624.0));
}

/// Stickiness ends where the containing block does. A box pinned forever would
/// escape its own section and float over the next one.
#[test]
fn the_box_leaves_with_its_containing_block() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <section id="section" style="height:1000px">
                <div id="header" style="position:sticky;top:0;height:50px"></div>
            </section>
            <section style="height:5000px"></section>
        </body></html>"#,
    );

    // Inside the section, pinned to the top of the scrollport.
    scroll_down(&mut doc, 400.0);
    doc.resolve(0.0);
    assert_eq!(origin(&doc, "header"), (0.0, 400.0));

    // The last position at which the box still fits inside its section is
    // 1000 - 50 = 950.
    scroll_down(&mut doc, 550.0);
    doc.resolve(0.0);
    assert_eq!(origin(&doc, "header"), (0.0, 950.0));

    // Past that it travels with the section rather than pinning forever.
    scroll_down(&mut doc, 500.0);
    doc.resolve(0.0);
    assert_eq!(
        origin(&doc, "header"),
        (0.0, 950.0),
        "a sticky box must not outlive its containing block"
    );
}

/// A `bottom` inset pins against the bottom edge of the scrollport, which means
/// a box further down the document than the scrollport's bottom edge is pulled
/// *up* to sit on it, and released once the page scrolls far enough that its
/// flow position rises above the line.
#[test]
fn a_bottom_inset_pins_against_the_bottom_of_the_scrollport() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <div style="height:5000px"></div>
            <div id="footer" style="position:sticky;bottom:0;height:50px"></div>
        </body></html>"#,
    );

    // The scrollport bottom is at document y=700, so the box's own bottom is
    // held there and its top lands at 650 rather than its flow position 5000.
    assert_eq!(origin(&doc, "footer"), (0.0, 650.0));

    scroll_down(&mut doc, 400.0);
    doc.resolve(0.0);
    assert_eq!(origin(&doc, "footer"), (0.0, 1050.0));

    // The document is 5050 tall, so 4350 is the last scroll position. There the
    // flow position and the pinned position coincide and the box is released.
    scroll_down(&mut doc, 3950.0);
    doc.resolve(0.0);
    assert_eq!(origin(&doc, "footer"), (0.0, 5000.0));
}

/// Horizontal stickiness works the same way against a horizontally scrolling
/// container.
#[test]
fn a_left_inset_pins_horizontally() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <div id="scroller" style="width:400px;height:200px;overflow:auto">
                <div style="width:3000px;height:100px;padding-left:100px">
                    <div id="label" style="position:sticky;left:10px;width:60px;height:20px"></div>
                </div>
            </div>
        </body></html>"#,
    );

    // Flow position 100 is to the right of the threshold at x=10, so nothing
    // moves.
    assert_eq!(origin(&doc, "label"), (100.0, 0.0));

    let scroller = doc.get_element_by_id("scroller").unwrap();
    doc.scroll_node_by(scroller, -250.0, 0.0, |_| {});
    doc.resolve(0.0);

    // The scroller's content has moved 250px left, taking the flow position to
    // -150, so the label is held 10px from the scrollport's left edge.
    assert_eq!(origin(&doc, "label"), (10.0, 0.0));
}

/// A `right` inset is the mirror of `left`: the box is pulled back towards the
/// left edge of the scrollport as content to its right scrolls into view.
#[test]
fn a_right_inset_pins_against_the_right_of_the_scrollport() {
    let doc = document(
        r#"<html><body style="margin:0">
            <div id="scroller" style="width:400px;height:200px;overflow:auto">
                <div style="width:3000px;height:100px">
                    <div id="label" style="position:sticky;right:20px;margin-left:2000px;width:60px;height:20px"></div>
                </div>
            </div>
        </body></html>"#,
    );

    // The scrollport's right edge is at x=400, so the box's own right edge is
    // held at 380 and its left edge lands at 320, rather than at its flow
    // position of 2000.
    assert_eq!(origin(&doc, "label"), (320.0, 0.0));
}

/// A sticky box in a nested scroller sticks to *that* scrollport, not to the
/// viewport. The document is not scrolled here at all.
#[test]
fn stickiness_is_relative_to_the_nearest_scrollport() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <div style="height:100px"></div>
            <div id="scroller" style="height:300px;overflow:auto">
                <div id="header" style="position:sticky;top:0;height:40px"></div>
                <div style="height:2000px"></div>
            </div>
        </body></html>"#,
    );

    assert_eq!(origin(&doc, "header"), (0.0, 100.0));

    let scroller = doc.get_element_by_id("scroller").unwrap();
    doc.scroll_node_by(scroller, 0.0, -500.0, |_| {});
    doc.resolve(0.0);

    // The scroller's box starts at y=100 and is not itself scrolled by the
    // page, so its scrollport top stays at 100.
    assert_eq!(
        origin(&doc, "header"),
        (0.0, 100.0),
        "a sticky box in a scroller pins to that scroller, not to the viewport"
    );
}

/// The adjustment has to survive a relayout that does not move anything, and
/// must not accumulate: applying it twice against an already-adjusted box would
/// walk the header down the page one scroll at a time.
#[test]
fn repeated_resolves_do_not_accumulate_the_offset() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <header id="header" style="position:sticky;top:0;height:50px"></header>
            <main style="height:5000px"></main>
        </body></html>"#,
    );

    scroll_down(&mut doc, 900.0);
    for _ in 0..5 {
        doc.resolve(0.0);
        assert_eq!(origin(&doc, "header"), (0.0, 900.0));
    }

    // The non-incremental path rebuilds every box from scratch each pass.
    doc.set_incremental_layout(false);
    for _ in 0..5 {
        doc.resolve(0.0);
        assert_eq!(origin(&doc, "header"), (0.0, 900.0));
    }
}

/// A scroll on its own, with no intervening resolve, still has to move the box:
/// a wheel event does not necessarily produce a full style and layout pass.
#[test]
fn a_scroll_alone_repositions_the_box() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <header id="header" style="position:sticky;top:0;height:50px"></header>
            <main style="height:5000px"></main>
        </body></html>"#,
    );

    scroll_down(&mut doc, 300.0);
    assert_eq!(origin(&doc, "header"), (0.0, 300.0));
}

/// Hit testing has to follow the box. A pinned header is the thing under the
/// pointer at the top of the screen, not whatever content scrolled beneath it.
#[test]
fn a_pinned_header_takes_the_hit() {
    let mut doc = document(
        r#"<html><body style="margin:0">
            <header id="header" style="position:sticky;top:0;height:50px"></header>
            <main id="body-copy" style="height:5000px"></main>
        </body></html>"#,
    );

    scroll_down(&mut doc, 800.0);
    doc.resolve(0.0);

    let header = doc.get_element_by_id("header").unwrap();
    // Hit tests take page coordinates. Screen point (20, 10) is page point
    // (20, 810) once the scroll is added back, which is inside the pinned
    // header.
    let hit_node = doc
        .hit(20.0, 810.0)
        .expect("nothing under the pointer")
        .node_id;

    let mut ancestor = Some(hit_node);
    while let Some(id) = ancestor {
        if id == header {
            return;
        }
        ancestor = doc.tree()[id].parent;
    }
    panic!("expected the pinned header under the pointer, got node {hit_node:?}");
}

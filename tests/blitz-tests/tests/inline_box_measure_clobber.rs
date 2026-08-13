//! A measurement pass must not move inline boxes.
//!
//! `compute_inline_layout_inner` returns early for `ComputeSize` on the
//! horizontal axis, but the vertical one fell through into the loop that writes
//! the size and position of every inline element on every line. `ComputeSize`
//! is asked the same subtree under a sequence of trial widths, so each trial
//! overwrote those boxes and whichever ran last won.
//!
//! Caught on a live instance: an item-reference chip at x=1620 inside a block
//! that had correctly resolved to 713px and correctly wrapped to three lines.
//! The text rewrapped, the elements on it did not move. A probe in the writing
//! loop recorded the passes doing it, including a 166px chip placed at x=0 on a
//! line broken at width 0, from a `known = Some(0.0)` height measurement.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 900;

/// A width-0 measurement is what the probe caught, and a flex row asks for one:
/// the item's automatic minimum size makes the algorithm measure the child
/// under a zero-ish offer to find its min-content contribution.
const HTML: &str = r#"<html><body style="margin:0">
    <div id="row" style="display:flex; width:900px; font-size:14px; line-height:28px;">
      <div id="col" style="display:flex; flex-direction:column; flex:1 1 0%; min-width:0;">
        <p id="prose">
          I am checking the running bundle identity and the active profile path,
          and the reference
          <button id="chip" style="display:inline; font-weight:600;">item-198a4811</button>
          covers it, so the data still exists and both stores are intact and
          readable on disk at this moment.
        </p>
      </div>
      <div id="aside" style="width:200px; flex:0 0 auto;">aside</div>
    </div>
  </body></html>"#;

fn chip_x(doc: &HtmlDocument) -> f32 {
    let id = doc.query_selector("#chip").unwrap().expect("no chip");
    doc.get_node(id).unwrap().final_layout().location.x
}

fn chip_width(doc: &HtmlDocument) -> f32 {
    let id = doc.query_selector("#chip").unwrap().expect("no chip");
    doc.get_node(id).unwrap().final_layout().size.width
}

fn prose_width(doc: &HtmlDocument) -> f32 {
    let id = doc.query_selector("#prose").unwrap().expect("no prose");
    doc.get_node(id).unwrap().final_layout().size.width
}

fn document(incremental: bool) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.set_incremental_layout(incremental);
    doc.resolve(0.0);
    doc
}

/// The chip is on a line inside a block that is about 700px wide, so its full
/// box must fit inside that block. Starting a wrapped line at x=0 is valid.
#[test]
fn an_inline_element_sits_on_the_line_it_was_laid_out_on() {
    let doc = document(true);
    let x = chip_x(&doc);
    let chip_width = chip_width(&doc);
    let width = prose_width(&doc);
    assert!(width > 400.0, "the block did not get a real width: {width}");
    assert!(
        x >= 0.0 && x + chip_width <= width + 0.5,
        "chip spans {x}..{} outside its {width}px block",
        x + chip_width,
    );
}

/// Resolving again with nothing changed must not move it either. A second
/// resolve re-runs the measurement passes, and if those write positions the
/// last trial wins.
#[test]
fn resolving_again_does_not_move_it() {
    let mut doc = document(true);
    let first = chip_x(&doc);
    doc.resolve(0.0);
    let second = chip_x(&doc);
    assert_eq!(
        first, second,
        "the chip moved on a second resolve with nothing changed"
    );
    let chip_width = chip_width(&doc);
    let width = prose_width(&doc);
    assert!(
        second >= 0.0 && second + chip_width <= width + 0.5,
        "chip spans {second}..{} outside its {width}px block after a second resolve",
        second + chip_width,
    );
}

/// And after a mutation elsewhere, which is what a live document does all day:
/// the block is re-measured, and must still be laid out before anything trusts
/// its children's positions.
#[test]
fn a_mutation_elsewhere_does_not_move_it() {
    let mut doc = document(true);
    let before = chip_x(&doc);

    let aside = doc.query_selector("#aside").unwrap().expect("no aside");
    doc.mutate().set_attribute(
        aside,
        blitz_dom::qual_name!("style"),
        "width:320px; flex:0 0 auto;",
    );
    doc.resolve(0.0);

    // The right edge, not the left. A chip at x=0 is a chip that starts a
    // line, which is what happens here once the aside grows and the block
    // narrows: asserting `x > 0` called correct rewrapping a bug and sent two
    // engine changes after something that was never wrong.
    let after = chip_x(&doc);
    let chip = doc.query_selector("#chip").unwrap().unwrap();
    let chip_width = doc.get_node(chip).unwrap().final_layout().size.width;
    let width = prose_width(&doc);
    assert!(
        after >= 0.0 && after + chip_width <= width + 0.5,
        "chip spans {after}..{} outside its {width}px block after the aside grew (was {before})",
        after + chip_width
    );
}

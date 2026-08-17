//! An absolutely positioned box with `inset: 0` fills its containing block.
//!
//! This is the shape every "overlay that covers its parent" uses, and
//! `@pathscale/ui` uses it for the coloured part of a radio:
//!
//!     .radio__control   { position: relative; width: 1rem; height: 1rem }
//!     .radio__indicator { position: absolute; inset: 0 }
//!
//! AgencyZero puts its swatch inside that indicator, so if `inset: 0` does not
//! resolve to a width and a height, the indicator is 0x0, the swatch has
//! nothing to fill, and the control paints as an empty ring. That is exactly
//! what the appearance pane showed: every colour-strength, softness and accent
//! swatch rendered as a hollow outline with no colour in it.

use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    node_id::NodeId,
    shell::{ColorScheme, Viewport},
};
use std::sync::Arc;

fn box_of(html: &str, selector: &str) -> (f32, f32) {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let inner = doc.inner();
    let id: NodeId = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    let layout = inner.get_node(id).unwrap().final_layout();
    (layout.size.width, layout.size.height)
}

/// The library's own shape, verbatim.
const RADIO: &str = r#"
<html><head><style>
  body { margin: 0; }
  .control { position: relative; width: 32px; height: 32px; }
  .indicator { position: absolute; inset: 0; }
  .swatch { width: 100%; height: 100%; background: red; }
</style></head>
<body>
  <span class="control"><span id="indicator" class="indicator"><span id="swatch" class="swatch"></span></span></span>
</body></html>
"#;

#[test]
#[ignore = "known defect: a relative inline box establishes no containing block"]
fn inset_zero_fills_the_containing_block() {
    assert_eq!(
        box_of(RADIO, "#indicator"),
        (32.0, 32.0),
        "`inset: 0` should size the indicator to its positioned parent"
    );
}

/// And the child that asks for 100% of it gets a real box, which is the part
/// that decides whether any colour is painted at all.
#[test]
#[ignore = "known defect: a relative inline box establishes no containing block"]
fn a_percentage_child_of_an_inset_box_has_a_size() {
    assert_eq!(box_of(RADIO, "#swatch"), (32.0, 32.0));
}

/// The longhand spelling, since `inset` is a shorthand and the two can be
/// implemented apart.
#[test]
#[ignore = "known defect: a relative inline box establishes no containing block"]
fn the_longhand_offsets_fill_the_containing_block_too() {
    const LONGHAND: &str = r#"
    <html><head><style>
      body { margin: 0; }
      .control { position: relative; width: 32px; height: 32px; }
      .indicator { position: absolute; top: 0; right: 0; bottom: 0; left: 0; }
    </style></head>
    <body><span class="control"><span id="indicator" class="indicator"></span></span></body></html>
    "#;
    assert_eq!(box_of(LONGHAND, "#indicator"), (32.0, 32.0));
}

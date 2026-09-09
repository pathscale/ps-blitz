//! `table { display: block }` still lays its rows out as a table.
//!
//! This is the standard wide-table horizontal-scroll pattern, and it made every
//! row render SIDE BY SIDE: the header row on one line, then six data rows all
//! at the same y and at increasing x. With the table no longer a table, its
//! `thead` and `tbody` were plain children of a block, and their own displays
//! (`table-header-group`, `table-row-group`) have no mapping in the style
//! conversion, so they fell through to Taffy's default, which is flex. A flex
//! container lays its items out in a row.
//!
//! CSS 2.1 17.2.1 requires an anonymous table box to be generated around
//! misparented table-internal boxes instead.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
    body { margin: 0 }
    .doc-prose table { display: block; overflow-x: auto }
    td, th { padding: 0; height: 20px; width: 100px }
</style></head>
<body>
    <div class="doc-prose">
        <table>
            <thead><tr><th id="h1">Name</th><th id="h2">Size</th></tr></thead>
            <tbody>
                <tr><td id="a1">alpha</td><td id="a2">1</td></tr>
                <tr><td id="b1">beta</td><td id="b2">2</td></tr>
            </tbody>
        </table>
    </div>
</body></html>
"#;

fn origin(doc: &HtmlDocument, selector: &str) -> (f32, f32) {
    let node_id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    let position = doc.get_node(node_id).unwrap().absolute_position(0.0, 0.0);
    (position.x, position.y)
}

#[test]
fn rows_stack_when_the_table_is_display_block() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let head = origin(&doc, "#h1");
    let first = origin(&doc, "#a1");
    let second = origin(&doc, "#b1");

    assert!(
        head.1 < first.1 && first.1 < second.1,
        "rows must stack, not sit side by side: header {head:?} first {first:?} second {second:?}"
    );
    assert_eq!(
        (head.0, first.0),
        (first.0, second.0),
        "the first cell of every row starts at the same x"
    );
}

#[test]
fn cells_sit_side_by_side_when_the_table_is_display_block() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let left = origin(&doc, "#a1");
    let right = origin(&doc, "#a2");

    assert_eq!(left.1, right.1, "cells in one row share a baseline");
    assert!(
        right.0 > left.0,
        "the second cell is to the right of the first: {left:?} {right:?}"
    );
}

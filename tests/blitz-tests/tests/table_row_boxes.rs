//! `<tr>`, `<thead>` and `<tbody>` must report a box.
//!
//! Table layout flattens the rows into a CSS grid of cells: the row and
//! row-group nodes have their box construction damage cleared and never reach
//! Taffy, so every one of them reported 0x0 and "not displayed". Anything that
//! walks a table by row, a QA check included, had nothing to walk.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
    body { margin: 0 }
    table { border-collapse: collapse; width: 400px }
    td, th { padding: 0; height: 20px; width: 200px }
</style></head>
<body>
    <table>
        <thead><tr id="head"><th>Name</th><th>Size</th></tr></thead>
        <tbody id="body">
            <tr id="first"><td>alpha</td><td>1</td></tr>
            <tr id="second"><td>beta</td><td>2</td></tr>
        </tbody>
    </table>
</body></html>
"#;

fn rect(doc: &HtmlDocument, selector: &str) -> (f32, f32, f32, f32) {
    let node_id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    let node = doc.get_node(node_id).unwrap();
    let position = node.absolute_position(0.0, 0.0);
    let layout = node.final_layout();
    (
        position.x,
        position.y,
        layout.size.width,
        layout.size.height,
    )
}

#[test]
fn every_row_reports_the_box_its_cells_occupy() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let head = rect(&doc, "#head");
    let first = rect(&doc, "#first");
    let second = rect(&doc, "#second");

    for (name, row) in [("head", head), ("first", first), ("second", second)] {
        assert!(
            row.2 > 0.0 && row.3 > 0.0,
            "row #{name} has no box: {row:?}"
        );
    }

    assert_eq!(
        (head.0, head.2),
        (first.0, first.2),
        "every row spans the same horizontal extent, the table's"
    );
    assert_eq!((first.0, first.2), (second.0, second.2));
    assert_eq!(head.3, 20.0, "a row is as tall as its cells");

    assert!(
        head.1 < first.1 && first.1 < second.1,
        "rows stack in tree order: {head:?} {first:?} {second:?}"
    );
    assert!(
        head.1 + head.3 <= first.1 && first.1 + first.3 <= second.1,
        "rows do not overlap: {head:?} {first:?} {second:?}"
    );
}

#[test]
fn a_row_group_spans_the_rows_it_holds() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let body = rect(&doc, "#body");
    let first = rect(&doc, "#first");
    let second = rect(&doc, "#second");

    assert_eq!(
        (body.0, body.2),
        (first.0, first.2),
        "a row group spans the table horizontally"
    );
    assert_eq!(body.1, first.1, "tbody starts at its first row");
    assert_eq!(
        body.1 + body.3,
        second.1 + second.3,
        "tbody ends at its last row"
    );
    assert!(body.3 > first.3, "tbody covers both of its rows");
}

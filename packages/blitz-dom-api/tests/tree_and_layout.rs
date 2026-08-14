//! Build a document through the facade alone, then lay it out.
//!
//! The unit tests each check one operation against tree state. This checks
//! that the operations compose into a document the engine agrees with: every
//! node here is created, attributed and attached through `blitz-dom-api`, and
//! then `blitz-dom` is asked to lay it out.
//!
//! Geometry is asserted exactly. A tree assembled wrongly, with the rows as
//! siblings of the panel rather than children of it, still lays out to
//! non-zero boxes, so "has a box" would pass for the wrong document.

use blitz_dom::{BaseDocument, DocumentConfig, NodeId};
use blitz_dom_api::{document, element, geometry, node, style};
use blitz_traits::shell::{ColorScheme, Viewport};

/// `<html><body><div class="panel"><div class="row">…</div>…</div></body></html>`
/// with every box sized in pixels and every margin zeroed, so the expected
/// layout is arithmetic rather than a guess about the user agent stylesheet.
fn build() -> (BaseDocument, Vec<NodeId>, NodeId) {
    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    // The only line that is not the facade: it has no operation for the
    // document node itself, because no DOM method returns one.
    let root_id = doc.root_node().id;

    let html = document::create_element(&mut doc, "html").unwrap();
    let head = document::create_element(&mut doc, "head").unwrap();
    let body = document::create_element(&mut doc, "body").unwrap();
    element::set_attribute(&mut doc, body, "style", "margin: 0; padding: 0").unwrap();
    node::append_child(&mut doc, html, head).unwrap();
    node::append_child(&mut doc, html, body).unwrap();

    let panel = document::create_element(&mut doc, "div").unwrap();
    element::class_list_add(&mut doc, panel, &["panel"]).unwrap();
    style::set_property(&mut doc, panel, "width", "200px").unwrap();
    style::set_property(&mut doc, panel, "margin", "0").unwrap();
    node::append_child(&mut doc, body, panel).unwrap();

    let mut rows = Vec::new();
    for height in [20, 30, 40] {
        let row = document::create_element(&mut doc, "div").unwrap();
        element::class_list_add(&mut doc, row, &["row"]).unwrap();
        element::set_attribute(
            &mut doc,
            row,
            "style",
            &format!("width: 100px; height: {height}px; margin: 0"),
        )
        .unwrap();
        let label = document::create_text_node(&mut doc, &format!("row {height}")).unwrap();
        node::append_child(&mut doc, row, label).unwrap();
        node::append_child(&mut doc, panel, row).unwrap();
        rows.push(row);
    }

    // Attached last, so the tree is only reachable from the document once it
    // is fully built. Layout has not run at any point above.
    node::append_child(&mut doc, root_id, html).unwrap();
    doc.resolve(0.0);

    (doc, rows, panel)
}

#[test]
fn the_facade_builds_a_tree_the_engine_lays_out() {
    let (doc, rows, panel) = build();

    let panel_rect = geometry::bounding_client_rect(&doc, panel).unwrap();
    assert_eq!(panel_rect.x, 0.0, "panel should sit at the viewport origin");
    assert_eq!(panel_rect.y, 0.0, "panel should sit at the viewport origin");
    assert_eq!(panel_rect.width, 200.0);
    assert_eq!(
        panel_rect.height, 90.0,
        "panel should be exactly as tall as its three rows"
    );

    let mut expected_top = 0.0;
    for (row, height) in rows.iter().zip([20.0, 30.0, 40.0]) {
        let rect = geometry::bounding_client_rect(&doc, *row).unwrap();
        assert_eq!(rect.width, 100.0);
        assert_eq!(rect.height, height);
        assert_eq!(rect.x, 0.0, "rows are block children of the panel");
        assert_eq!(
            rect.y, expected_top,
            "row should stack directly under the previous one"
        );
        expected_top += height;
    }
}

#[test]
fn the_tree_is_reachable_through_the_facades_own_queries() {
    let (doc, rows, panel) = build();

    assert_eq!(
        document::query_selector(&doc, ".panel").unwrap(),
        Some(panel)
    );
    assert_eq!(document::query_selector_all(&doc, ".row").unwrap(), rows);
    assert_eq!(node::child_nodes(&doc, panel).unwrap(), rows);
    assert_eq!(node::parent_node(&doc, rows[0]).unwrap(), Some(panel));
    assert_eq!(
        element::closest(&doc, rows[2], ".panel").unwrap(),
        Some(panel)
    );
    assert!(node::contains(&doc, panel, rows[1]).unwrap());

    let body = document::body(&doc).unwrap().expect("body");
    assert_eq!(node::parent_node(&doc, panel).unwrap(), Some(body));
    assert!(document::head(&doc).unwrap().is_some());

    assert_eq!(
        node::text_content(&doc, panel).unwrap(),
        "row 20row 30row 40"
    );
    assert_eq!(element::tag_name(&doc, panel).unwrap(), "DIV");
    assert_eq!(
        style::get_property_value(&doc, panel, "width").unwrap(),
        "200px"
    );
}

/// Mutating after layout and resolving again moves the boxes, which is what a
/// runtime driving this crate does on every update.
#[test]
fn a_mutation_after_layout_moves_the_boxes_once_the_caller_resolves() {
    let (mut doc, rows, panel) = build();

    node::remove_child(&mut doc, panel, rows[0]).unwrap();
    style::set_property(&mut doc, rows[1], "height", "60px").unwrap();
    doc.resolve(0.0);

    let first = geometry::bounding_client_rect(&doc, rows[1]).unwrap();
    let second = geometry::bounding_client_rect(&doc, rows[2]).unwrap();
    assert_eq!(first.y, 0.0);
    assert_eq!(first.height, 60.0);
    assert_eq!(second.y, 60.0);
    assert_eq!(
        geometry::bounding_client_rect(&doc, panel).unwrap().height,
        100.0
    );
}

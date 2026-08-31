//! A `display: contents` child hoists its children into the container's
//! formatting context, and they must be laid out on that container's terms.
//!
//! The element itself generates no box, so its children become children of
//! whatever it sat inside. When that is a flex container, raw text among them
//! is not a flex item: it has to be wrapped in an anonymous one first. Box
//! construction used to hoist through a path that never applied the outer
//! container's wrapping rule, so the text arrived at layout as a direct flex
//! item. Text carries no style, and asking it for one panicked the engine with
//! "`style` is not available on this node kind".
//!
//! Found on a real site, where the shape is ordinary: a link laid out as a
//! flex row wrapping a framework's `display: contents` slot element.

use blitz_dom::{BaseDocument, DocumentConfig, NodeId};
use blitz_dom_api::{document, geometry, node, style};
use blitz_traits::shell::{ColorScheme, Viewport};

/// `<body><div flex><span contents>text</span></div></body>`, with `outer`
/// returned.
fn build(container_display: &str) -> (BaseDocument, NodeId) {
    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    let root_id = doc.root_node().id;

    let html = document::create_element(&mut doc, "html").unwrap();
    let head = document::create_element(&mut doc, "head").unwrap();
    let body = document::create_element(&mut doc, "body").unwrap();
    style::set_property(&mut doc, body, "margin", "0").unwrap();
    node::append_child(&mut doc, html, head).unwrap();
    node::append_child(&mut doc, html, body).unwrap();

    let outer = document::create_element(&mut doc, "div").unwrap();
    style::set_property(&mut doc, outer, "display", container_display).unwrap();
    style::set_property(&mut doc, outer, "margin", "0").unwrap();
    node::append_child(&mut doc, body, outer).unwrap();

    let slot = document::create_element(&mut doc, "span").unwrap();
    style::set_property(&mut doc, slot, "display", "contents").unwrap();
    node::append_child(&mut doc, outer, slot).unwrap();

    let text = document::create_text_node(&mut doc, "hoisted text").unwrap();
    node::append_child(&mut doc, slot, text).unwrap();

    node::append_child(&mut doc, root_id, html).unwrap();
    doc.resolve(0.0);

    (doc, outer)
}

/// The case that panicked: a flex container.
#[test]
fn a_flex_container_lays_out_text_hoisted_through_display_contents() {
    let (doc, outer) = build("flex");

    // Reaching here at all is the assertion: before the fix, `resolve` above
    // panicked with "`style` is not available on this node kind". The box is
    // checked for existence rather than for size, because this document has no
    // font stack and shaped text measures zero in it.
    let rect = geometry::bounding_client_rect(&doc, outer).unwrap();
    assert!(
        rect.width > 0.0,
        "the flex container should have a box, got {rect:?}"
    );
}

/// Grid takes the same construction path, so it carries the same risk.
#[test]
fn a_grid_container_lays_out_text_hoisted_through_display_contents() {
    let (doc, outer) = build("grid");

    let rect = geometry::bounding_client_rect(&doc, outer).unwrap();
    assert!(
        rect.width > 0.0,
        "the grid container should have a box, got {rect:?}"
    );
}

/// The ordinary block case, which already worked, and must keep working: the
/// fix changes the path every container hoists through, not just flex.
#[test]
fn a_block_container_still_lays_out_text_hoisted_through_display_contents() {
    let (doc, outer) = build("block");

    let rect = geometry::bounding_client_rect(&doc, outer).unwrap();
    assert!(
        rect.width > 0.0,
        "the block container should have a box, got {rect:?}"
    );
}

//! Document construction for the unit tests.
//!
//! The HTML parser lives in `blitz-html`, which would be a dev-dependency
//! cycle, so tests build their trees through the mutator the same way
//! `blitz-dom`'s own tests do.

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, qual_name};

/// An empty document with an `<html><head></head><body></body></html>`
/// skeleton, and no viewport.
///
/// Returns the document plus the html, head and body ids, which is what almost
/// every test needs to place a node somewhere reachable.
pub(crate) fn skeleton() -> (BaseDocument, NodeId, NodeId, NodeId) {
    build(BaseDocument::new(DocumentConfig::default()))
}

/// The same skeleton in a document with a viewport, so that `doc.resolve`
/// produces real boxes. `body` carries `margin: 0` to make positions
/// predictable.
pub(crate) fn viewport_skeleton(width: u32, height: u32) -> (BaseDocument, NodeId, NodeId, NodeId) {
    use blitz_traits::shell::{ColorScheme, Viewport};

    let doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(width, height, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    let (mut doc, html, head, body) = build(doc);
    doc.mutate()
        .set_attribute(body, qual_name!("style"), "margin: 0");
    (doc, html, head, body)
}

fn build(mut doc: BaseDocument) -> (BaseDocument, NodeId, NodeId, NodeId) {
    let root_id = doc.root_node().id;

    let mut mutr = doc.mutate();
    let html = mutr.create_element(qual_name!("html"), vec![]);
    let head = mutr.create_element(qual_name!("head"), vec![]);
    let body = mutr.create_element(qual_name!("body"), vec![]);
    mutr.append_children(html, &[head, body]);
    mutr.append_children(root_id, &[html]);
    drop(mutr);

    (doc, html, head, body)
}

/// A document with nothing but the implicit document node.
pub(crate) fn bare() -> BaseDocument {
    BaseDocument::new(DocumentConfig::default())
}

//! `getBoundingClientRect`, the one operation in scope that reads layout.
//!
//! Upstream: `blitz-script/src/dom/element.rs`. See MAPPING.md.

use blitz_dom::{BaseDocument, NodeId};

use crate::Result;

/// A viewport-space rectangle, in zoomed (device-independent) CSS pixels.
///
/// This crate's own type rather than `blitz_dom`'s `BoundingRect`, which
/// `blitz-dom` does not re-export and so cannot be named from outside it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Distance from the left of the viewport.
    pub x: f64,
    /// Distance from the top of the viewport.
    pub y: f64,
    /// Border-box width.
    pub width: f64,
    /// Border-box height.
    pub height: f64,
}

impl Rect {
    /// An all-zero rectangle, which is what a node with no box reports.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    /// `rect.left`.
    pub fn left(&self) -> f64 {
        self.x
    }

    /// `rect.top`.
    pub fn top(&self) -> f64 {
        self.y
    }

    /// `rect.right`.
    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    /// `rect.bottom`.
    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }
}

/// `element.getBoundingClientRect()`.
///
/// **Layout must already be current.** This is the one operation in the crate
/// whose answer depends on resolved layout rather than on tree state, and it
/// is the one place where the facade cannot carry upstream's whole behaviour:
/// `blitz-script` calls `DomCtx::flush_layout` first, which resolves the
/// document if script has mutated it since the last frame. That dirty flag
/// belongs to the binding, not to an operation over a borrowed document, and a
/// facade that resolved unconditionally would turn a cheap read into a full
/// layout pass on every call.
///
/// So the contract is inverted: the caller flushes, then reads. A binding
/// reparenting onto this keeps its `ctx.flush_layout()` line and replaces only
/// the read below it. A caller that forgets gets the geometry from before its
/// own mutations, silently.
///
/// Zeros for a node with no box, matching upstream.
pub fn bounding_client_rect(doc: &BaseDocument, node: NodeId) -> Result<Rect> {
    Ok(match doc.get_client_bounding_rect(node) {
        Some(rect) => Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        },
        None => Rect::ZERO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use crate::element;
    use crate::node;
    use crate::test_support::viewport_skeleton;

    #[test]
    fn bounding_client_rect_reports_the_laid_out_box() {
        let (mut doc, _html, _head, body) = viewport_skeleton(400, 300);
        let id = document::create_element(&mut doc, "div").unwrap();
        element::set_attribute(&mut doc, id, "style", "width: 120px; height: 40px").unwrap();
        node::append_child(&mut doc, body, id).unwrap();
        doc.resolve(0.0);

        let rect = bounding_client_rect(&doc, id).unwrap();
        assert_eq!(rect.width, 120.0);
        assert_eq!(rect.height, 40.0);
        assert_eq!(rect.right(), rect.x + 120.0);
        assert_eq!(rect.bottom(), rect.y + 40.0);
    }

    /// The documented contract, as a test: the read does not flush, so a
    /// mutation made after the last resolve is not visible until the caller
    /// resolves. This is the behaviour a binding has to compensate for.
    #[test]
    fn the_read_does_not_flush_layout() {
        let (mut doc, _html, _head, body) = viewport_skeleton(400, 300);
        let id = document::create_element(&mut doc, "div").unwrap();
        element::set_attribute(&mut doc, id, "style", "width: 120px; height: 40px").unwrap();
        node::append_child(&mut doc, body, id).unwrap();
        doc.resolve(0.0);

        element::set_attribute(&mut doc, id, "style", "width: 200px; height: 40px").unwrap();
        assert_eq!(bounding_client_rect(&doc, id).unwrap().width, 120.0);
        doc.resolve(0.0);
        assert_eq!(bounding_client_rect(&doc, id).unwrap().width, 200.0);
    }

    #[test]
    fn a_detached_node_reports_zeros() {
        let (mut doc, _html, _head, _body) = viewport_skeleton(400, 300);
        let id = document::create_element(&mut doc, "div").unwrap();
        doc.resolve(0.0);
        assert_eq!(bounding_client_rect(&doc, id).unwrap(), Rect::ZERO);
    }
}

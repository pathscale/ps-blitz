//! Document-level operations: node creation and lookup.
//!
//! Upstream: `blitz-script/src/dom/document.rs`. See MAPPING.md.

use blitz_dom::{BaseDocument, LocalName, Namespace, NodeId, QualName};

use crate::Result;
use crate::error::DomError;

/// An HTML-namespaced qualified name.
pub(crate) fn qual_name(local: &str) -> QualName {
    QualName::new(None, markup5ever::ns!(html), LocalName::from(local))
}

/// A qualified name in an explicit namespace.
pub(crate) fn qual_name_ns(local: &str, ns: &str) -> QualName {
    QualName::new(None, Namespace::from(ns), LocalName::from(local))
}

// === Node creation ===

/// `document.createElement(tag)`.
///
/// The tag is ASCII-lowercased, as the HTML parser would have done. The
/// element is created detached; nothing about layout changes until it is
/// inserted, which is why `blitz-script` deliberately does not mark layout
/// dirty here either.
pub fn create_element(doc: &mut BaseDocument, tag: &str) -> Result<NodeId> {
    let tag = tag.to_ascii_lowercase();
    Ok(doc.mutate().create_element(qual_name(&tag), Vec::new()))
}

/// `document.createElementNS(ns, tag)`.
///
/// Argument order follows the DOM method, namespace first. The tag is *not*
/// lowercased: SVG and MathML names are case-sensitive.
pub fn create_element_ns(doc: &mut BaseDocument, ns: &str, tag: &str) -> Result<NodeId> {
    Ok(doc
        .mutate()
        .create_element(qual_name_ns(tag, ns), Vec::new()))
}

/// `document.createTextNode(text)`.
pub fn create_text_node(doc: &mut BaseDocument, text: &str) -> Result<NodeId> {
    Ok(doc.mutate().create_text_node(text))
}

/// `document.createComment(text)`.
pub fn create_comment(doc: &mut BaseDocument, text: &str) -> Result<NodeId> {
    Ok(doc.mutate().create_comment_node(text))
}

/// `document.importNode(node, deep)`.
///
/// Every script-visible node belongs to the one document, so importing is the
/// same structural operation as cloning. It stays a distinct entry point
/// because frameworks take this path specifically for template roots.
pub fn import_node(doc: &mut BaseDocument, node: NodeId, deep: bool) -> Result<NodeId> {
    crate::node::clone_node(doc, node, deep)
}

// === Lookup ===

/// `document.documentElement`.
pub fn document_element(doc: &BaseDocument) -> Result<Option<NodeId>> {
    Ok(doc.try_root_element().map(|root| root.id))
}

/// `document.body`.
pub fn body(doc: &BaseDocument) -> Result<Option<NodeId>> {
    Ok(find_tag(doc, markup5ever::local_name!("body")))
}

/// `document.head`.
pub fn head(doc: &BaseDocument) -> Result<Option<NodeId>> {
    Ok(find_tag(doc, markup5ever::local_name!("head")))
}

/// Find the first element with the given tag name.
///
/// Three stages, in this order, copied from upstream: the root element itself,
/// then its immediate children, then a full pre-order walk. The first two are
/// the shape a parsed document actually has; the walk is the fallback for a
/// tree script assembled by hand.
fn find_tag(doc: &BaseDocument, tag: LocalName) -> Option<NodeId> {
    let root = doc.try_root_element()?;
    if root.data.is_element_with_tag_name(&tag) {
        return Some(root.id);
    }
    root.children
        .iter()
        .copied()
        .find(|child_id| {
            doc.get_node(*child_id)
                .is_some_and(|child| child.data.is_element_with_tag_name(&tag))
        })
        .or_else(|| {
            let mut stack = vec![doc.root_node().id];
            while let Some(node_id) = stack.pop() {
                let node = doc.get_node(node_id)?;
                if node.data.is_element_with_tag_name(&tag) {
                    return Some(node_id);
                }
                stack.extend(node.children.iter().rev().copied());
            }
            None
        })
}

/// `document.getElementById(id)`.
pub fn get_element_by_id(doc: &BaseDocument, id: &str) -> Result<Option<NodeId>> {
    Ok(doc.get_element_by_id(id))
}

/// `document.querySelector(selector)`.
///
/// Deliberate difference: an unparseable selector is an error here.
/// `blitz-script` maps it to `null`; its reparented body is
/// `query_selector(...).ok().flatten()`.
pub fn query_selector(doc: &BaseDocument, selector: &str) -> Result<Option<NodeId>> {
    doc.query_selector(selector)
        .map_err(|_| DomError::InvalidSelector(selector.to_owned()))
}

/// `document.querySelectorAll(selector)`, in tree order.
///
/// Same deliberate difference as [`query_selector`]; upstream's fallback is
/// `.unwrap_or_default()`.
pub fn query_selector_all(doc: &BaseDocument, selector: &str) -> Result<Vec<NodeId>> {
    doc.query_selector_all(selector)
        .map(|matches| matches.into_iter().collect())
        .map_err(|_| DomError::InvalidSelector(selector.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element;
    use crate::node;
    use crate::test_support::{bare, skeleton};

    #[test]
    fn create_element_lowercases_the_tag() {
        let mut doc = bare();
        let id = create_element(&mut doc, "DIV").unwrap();
        assert_eq!(element::tag_name(&doc, id).unwrap(), "DIV");
        assert_eq!(
            doc.get_node(id)
                .and_then(|n| n.element_data())
                .map(|e| e.name.local.to_string()),
            Some("div".to_string())
        );
    }

    #[test]
    fn create_element_ns_keeps_the_namespace_and_the_case() {
        let mut doc = bare();
        let id =
            create_element_ns(&mut doc, "http://www.w3.org/2000/svg", "linearGradient").unwrap();
        let element = doc.get_node(id).unwrap().element_data().unwrap();
        assert_eq!(element.name.local.to_string(), "linearGradient");
        assert_eq!(element.name.ns.to_string(), "http://www.w3.org/2000/svg");
    }

    #[test]
    fn create_text_node_carries_its_text() {
        let mut doc = bare();
        let id = create_text_node(&mut doc, "hello").unwrap();
        assert_eq!(node::text_content(&doc, id).unwrap(), "hello");
    }

    #[test]
    fn create_comment_makes_a_comment_node() {
        let mut doc = bare();
        let id = create_comment(&mut doc, "note").unwrap();
        assert!(matches!(
            doc.get_node(id).map(|n| &n.data),
            Some(blitz_dom::NodeData::Comment { .. })
        ));
    }

    #[test]
    fn import_node_copies_the_subtree_when_deep() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = create_element(&mut doc, "div").unwrap();
        let inner = create_element(&mut doc, "span").unwrap();
        node::append_child(&mut doc, outer, inner).unwrap();
        node::append_child(&mut doc, body, outer).unwrap();

        let copy = import_node(&mut doc, outer, true).unwrap();
        assert_ne!(copy, outer);
        assert_eq!(node::child_nodes(&doc, copy).unwrap().len(), 1);

        let shallow = import_node(&mut doc, outer, false).unwrap();
        assert!(node::child_nodes(&doc, shallow).unwrap().is_empty());
    }

    #[test]
    fn document_element_is_the_root_element() {
        let (doc, html, _head, _body) = skeleton();
        assert_eq!(document_element(&doc).unwrap(), Some(html));
        assert_eq!(document_element(&bare()).unwrap(), None);
    }

    #[test]
    fn body_and_head_are_found_among_the_root_children() {
        let (doc, _html, head_id, body_id) = skeleton();
        assert_eq!(body(&doc).unwrap(), Some(body_id));
        assert_eq!(head(&doc).unwrap(), Some(head_id));
    }

    /// The third stage of the search: a body nested deeper than the root's own
    /// children still resolves.
    #[test]
    fn body_falls_back_to_a_full_tree_walk() {
        let (mut doc, html, _head, _body) = skeleton();
        let wrapper = create_element(&mut doc, "div").unwrap();
        let nested = create_element(&mut doc, "body").unwrap();
        node::append_child(&mut doc, wrapper, nested).unwrap();
        node::append_child(&mut doc, html, wrapper).unwrap();

        // The skeleton's own body is still a direct child, so it wins.
        assert_ne!(body(&doc).unwrap(), Some(nested));

        // With no direct child body, the walk finds the nested one.
        let (mut doc, html, _head, body_id) = skeleton();
        node::remove_child(&mut doc, html, body_id).unwrap();
        let wrapper = create_element(&mut doc, "div").unwrap();
        let nested = create_element(&mut doc, "body").unwrap();
        node::append_child(&mut doc, wrapper, nested).unwrap();
        node::append_child(&mut doc, html, wrapper).unwrap();
        assert_eq!(body(&doc).unwrap(), Some(nested));
    }

    #[test]
    fn get_element_by_id_finds_an_attached_element() {
        let (mut doc, _html, _head, body_id) = skeleton();
        let target = create_element(&mut doc, "div").unwrap();
        element::set_attribute(&mut doc, target, "id", "target").unwrap();
        node::append_child(&mut doc, body_id, target).unwrap();
        assert_eq!(get_element_by_id(&doc, "target").unwrap(), Some(target));
        assert_eq!(get_element_by_id(&doc, "absent").unwrap(), None);
    }

    #[test]
    fn query_selector_returns_the_first_match() {
        let (mut doc, _html, _head, body_id) = skeleton();
        let first = create_element(&mut doc, "p").unwrap();
        let second = create_element(&mut doc, "p").unwrap();
        node::append_child(&mut doc, body_id, first).unwrap();
        node::append_child(&mut doc, body_id, second).unwrap();
        assert_eq!(query_selector(&doc, "p").unwrap(), Some(first));
    }

    #[test]
    fn query_selector_all_returns_every_match_in_tree_order() {
        let (mut doc, _html, _head, body_id) = skeleton();
        let first = create_element(&mut doc, "p").unwrap();
        let second = create_element(&mut doc, "p").unwrap();
        node::append_child(&mut doc, body_id, first).unwrap();
        node::append_child(&mut doc, body_id, second).unwrap();
        assert_eq!(query_selector_all(&doc, "p").unwrap(), vec![first, second]);
    }

    #[test]
    fn an_unparseable_selector_is_an_error_not_an_empty_result() {
        let (doc, _html, _head, _body) = skeleton();
        assert!(matches!(
            query_selector(&doc, "!!!"),
            Err(DomError::InvalidSelector(_))
        ));
        assert!(matches!(
            query_selector_all(&doc, "!!!"),
            Err(DomError::InvalidSelector(_))
        ));
    }
}

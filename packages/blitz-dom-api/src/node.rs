//! Node operations: tree structure, tree mutation and text content.
//!
//! Upstream: `blitz-script/src/dom/node.rs`. See MAPPING.md.

use std::fmt;

use blitz_dom::node::NodeData;
use blitz_dom::{BaseDocument, NodeId};

use crate::Result;
use crate::error::DomError;

/// The bit flags `compare_document_position` returns.
pub mod document_position {
    /// The two nodes are in different trees.
    pub const DISCONNECTED: u16 = 0x01;
    /// The other node precedes this one.
    pub const PRECEDING: u16 = 0x02;
    /// The other node follows this one.
    pub const FOLLOWING: u16 = 0x04;
    /// The other node is an ancestor of this one.
    pub const CONTAINS: u16 = 0x08;
    /// The other node is a descendant of this one.
    pub const CONTAINED_BY: u16 = 0x10;
    /// The relative order of two disconnected nodes is arbitrary but stable.
    pub const IMPLEMENTATION_SPECIFIC: u16 = 0x20;
}

// === Read-only tree structure ===

/// `node.parentNode` (and `parentElement`, which upstream aliases to it).
pub fn parent_node(doc: &BaseDocument, node: NodeId) -> Result<Option<NodeId>> {
    Ok(doc.get_node(node).and_then(|node| node.parent))
}

/// `node.childNodes`, as a snapshot rather than a live list.
///
/// A live `NodeList` would have to hold the document, which the borrow
/// discipline forbids. Every caller upstream materialises the list anyway.
pub fn child_nodes(doc: &BaseDocument, node: NodeId) -> Result<Vec<NodeId>> {
    Ok(doc
        .get_node(node)
        .map(|node| node.children.to_vec())
        .unwrap_or_default())
}

/// `node.firstChild`.
pub fn first_child(doc: &BaseDocument, node: NodeId) -> Result<Option<NodeId>> {
    Ok(doc
        .get_node(node)
        .and_then(|node| node.children.first().copied()))
}

/// `node.previousSibling`.
pub fn previous_sibling(doc: &BaseDocument, node: NodeId) -> Result<Option<NodeId>> {
    Ok(sibling(doc, node, -1))
}

/// `node.nextSibling`.
pub fn next_sibling(doc: &BaseDocument, node: NodeId) -> Result<Option<NodeId>> {
    Ok(sibling(doc, node, 1))
}

fn sibling(doc: &BaseDocument, node_id: NodeId, offset: isize) -> Option<NodeId> {
    let node = doc.get_node(node_id)?;
    let parent = doc.get_node(node.parent?)?;
    let index = parent.index_of_child(node_id)?;
    let sibling_index = index.checked_add_signed(offset)?;
    parent.children.get(sibling_index).copied()
}

/// `node.hasChildNodes()`.
pub fn has_child_nodes(doc: &BaseDocument, node: NodeId) -> Result<bool> {
    Ok(doc
        .get_node(node)
        .is_some_and(|node| !node.children.is_empty()))
}

/// `node.contains(other)`.
///
/// True when `other` is `node` or a descendant of it, matching the DOM's
/// inclusive definition.
pub fn contains(doc: &BaseDocument, node: NodeId, other: NodeId) -> Result<bool> {
    let mut current = other;
    loop {
        if current == node {
            return Ok(true);
        }
        match doc.get_node(current).and_then(|node| node.parent) {
            Some(parent_id) => current = parent_id,
            None => return Ok(false),
        }
    }
}

/// `node.compareDocumentPosition(other)`.
///
/// Deliberate difference: upstream asserts the common-ancestor invariants with
/// `expect` and panics on a malformed tree. Here they are
/// [`DomError::TreeInvariant`], because a facade must not take the process
/// down over document state.
pub fn compare_document_position(doc: &BaseDocument, node: NodeId, other: NodeId) -> Result<u16> {
    use document_position::*;

    if node == other {
        return Ok(0);
    }

    let path = |mut id: NodeId| {
        let mut path = Vec::new();
        loop {
            path.push(id);
            let Some(parent) = doc.get_node(id).and_then(|node| node.parent) else {
                break;
            };
            id = parent;
        }
        path.reverse();
        path
    };
    let node_path = path(node);
    let other_path = path(other);
    if node_path.first() != other_path.first() {
        let order = if node < other { FOLLOWING } else { PRECEDING };
        return Ok(DISCONNECTED | IMPLEMENTATION_SPECIFIC | order);
    }

    let common = node_path
        .iter()
        .zip(&other_path)
        .take_while(|(left, right)| left == right)
        .count();
    if common == node_path.len() {
        return Ok(FOLLOWING | CONTAINED_BY);
    }
    if common == other_path.len() {
        return Ok(PRECEDING | CONTAINS);
    }
    let parent_id = node_path[common - 1];
    let parent = doc
        .get_node(parent_id)
        .ok_or(DomError::TreeInvariant("common ancestor is missing"))?;
    let node_index = parent
        .children
        .iter()
        .position(|child| *child == node_path[common])
        .ok_or(DomError::TreeInvariant("node missing from common ancestor"))?;
    let other_index = parent
        .children
        .iter()
        .position(|child| *child == other_path[common])
        .ok_or(DomError::TreeInvariant(
            "other node missing from common ancestor",
        ))?;
    Ok(if node_index < other_index {
        FOLLOWING
    } else {
        PRECEDING
    })
}

// === Text content ===

/// `node.textContent`, concatenated over the subtree.
///
/// Owned, per the crate's borrow discipline. [`text_content_into`] is the
/// variant that writes into a caller's buffer instead.
pub fn text_content(doc: &BaseDocument, node: NodeId) -> Result<String> {
    Ok(doc
        .get_node(node)
        .map(|node| node.text_content())
        .unwrap_or_default())
}

/// A [`fmt::Write`] sink that only measures.
struct Counting(usize);

impl fmt::Write for Counting {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

/// A [`fmt::Write`] sink that fills a caller's slice.
///
/// Only ever handed a slice already known to be large enough, so the debug
/// assertion below is a check on this crate rather than on its caller.
struct Filling<'out> {
    out: &'out mut [u8],
    at: usize,
}

impl fmt::Write for Filling<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.at + s.len();
        debug_assert!(end <= self.out.len(), "measured length was short");
        self.out[self.at..end].copy_from_slice(s.as_bytes());
        self.at = end;
        Ok(())
    }
}

/// The byte length of `node.textContent`, without building it.
///
/// One traversal, no allocation. Useful on its own for sizing a buffer, and it
/// is the first half of [`text_content_into`].
pub fn text_content_len(doc: &BaseDocument, node: NodeId) -> Result<usize> {
    let mut counting = Counting(0);
    if let Some(node) = doc.get_node(node) {
        node.write_text_content(&mut counting);
    }
    Ok(counting.0)
}

/// `node.textContent`, written into a caller-supplied buffer.
///
/// Returns the **full** byte length, always. The bytes were written to
/// `out[..len]` if and only if `len <= out.len()`; a value that does not fit
/// leaves `out` untouched rather than truncated. Same contract as
/// [`element::get_attribute_into`](crate::element::get_attribute_into).
///
/// # What this trades, which is not what the attribute reader trades
///
/// An attribute's value exists in the document as contiguous bytes, so reading
/// it into a buffer is one `memcpy` and the owning reader's `String` was pure
/// overhead. **`textContent` has no such bytes.** It is a concatenation over a
/// subtree, so it does not exist anywhere until something builds it, and the
/// only question is *where*.
///
/// So this does not remove a copy, it removes an **allocation**: the bytes land
/// in the caller's buffer instead of in a `String` that is then copied there.
/// The price is a second traversal — one pass to measure, one to fill — because
/// the "nothing is written unless it all fits" guarantee cannot be honoured by
/// a single streaming pass that discovers the overflow halfway through.
///
/// Whether one allocation is worth one extra pointer-chasing walk of a subtree
/// is a timing question, and this crate deliberately measures bytes rather than
/// time. What is *not* a timing question: the allocation is gone, and a caller
/// that already knows the length can skip the measuring pass by calling
/// [`text_content_len`] itself.
pub fn text_content_into(doc: &BaseDocument, node: NodeId, out: &mut [u8]) -> Result<usize> {
    let len = text_content_len(doc, node)?;
    if len > out.len() {
        return Ok(len);
    }
    if let Some(node) = doc.get_node(node) {
        let mut filling = Filling { out, at: 0 };
        node.write_text_content(&mut filling);
    }
    Ok(len)
}

/// `node.textContent = text`.
///
/// On a text or comment node this rewrites the node's own text. On anything
/// else it replaces the children with a single text node, or with nothing when
/// the text is empty.
///
/// Existing children are *detached*, not dropped, so that wrappers a runtime
/// still holds for them stay valid.
pub fn set_text_content(doc: &mut BaseDocument, node: NodeId, text: &str) -> Result<()> {
    let is_text_like = matches!(
        doc.get_node(node).map(|node| &node.data),
        Some(NodeData::Text(_)) | Some(NodeData::Comment { .. })
    );

    let mut mutr = doc.mutate();
    if is_text_like {
        mutr.set_node_text(node, text);
    } else {
        for child_id in mutr.child_ids(node) {
            mutr.remove_node(child_id);
        }
        if !text.is_empty() {
            let text_id = mutr.create_text_node(text);
            mutr.append_children(node, &[text_id]);
        }
    }
    Ok(())
}

// === Tree mutation ===

/// `parent.appendChild(child)`, returning the child.
///
/// The child is detached from any current parent first, which is what makes
/// "move to the end of the same parent" behave.
///
/// A runtime that supports custom elements must run its upgrade step after
/// this call: insertion is when `connectedCallback` is due, and constructing a
/// guest object is not something this crate can do.
pub fn append_child(doc: &mut BaseDocument, parent: NodeId, child: NodeId) -> Result<NodeId> {
    let mut mutr = doc.mutate();
    if mutr.node_has_parent(child) {
        mutr.remove_node(child);
    }
    mutr.append_children(parent, &[child]);
    Ok(child)
}

/// `parent.insertBefore(new_node, reference)`, returning the inserted node.
///
/// `reference` of `None` is the DOM's null reference node and means append.
/// Inserting a node before itself is a no-op.
pub fn insert_before(
    doc: &mut BaseDocument,
    parent: NodeId,
    new_node: NodeId,
    reference: Option<NodeId>,
) -> Result<NodeId> {
    if reference == Some(new_node) {
        return Ok(new_node);
    }

    let mut mutr = doc.mutate();
    if mutr.node_has_parent(new_node) {
        mutr.remove_node(new_node);
    }
    match reference {
        Some(reference) if mutr.node_has_parent(reference) => {
            mutr.insert_nodes_before(reference, &[new_node]);
        }
        _ => mutr.append_children(parent, &[new_node]),
    }
    Ok(new_node)
}

/// `parent.removeChild(child)`, returning the child.
///
/// Copied behaviour worth knowing: `parent` is not checked. Upstream removes
/// the child from wherever it actually is rather than raising the DOM's
/// `NotFoundError`, and the node is detached rather than dropped so that
/// wrappers stay valid.
pub fn remove_child(doc: &mut BaseDocument, parent: NodeId, child: NodeId) -> Result<NodeId> {
    let _ = parent;
    doc.mutate().remove_node(child);
    Ok(child)
}

/// `parent.replaceChild(new_node, old_node)`, returning the *replaced* node.
///
/// Argument order follows the DOM method: new first, old second. The return is
/// `old_node`, which is what the DOM specifies and what upstream returns.
pub fn replace_child(
    doc: &mut BaseDocument,
    parent: NodeId,
    new_node: NodeId,
    old_node: NodeId,
) -> Result<NodeId> {
    let _ = parent;
    if new_node != old_node {
        let mut mutr = doc.mutate();
        if mutr.node_has_parent(new_node) {
            mutr.remove_node(new_node);
        }
        mutr.insert_nodes_before(old_node, &[new_node]);
        mutr.remove_node(old_node);
    }
    Ok(old_node)
}

/// `node.cloneNode(deep)`.
///
/// A shallow clone copies an element's name and attributes, a text node's
/// content, or a comment's contents. Anything else clones to an empty comment,
/// which is upstream's placeholder for a node kind it cannot reproduce.
pub fn clone_node(doc: &mut BaseDocument, node: NodeId, deep: bool) -> Result<NodeId> {
    enum CloneSrc {
        Element(blitz_dom::QualName, Vec<blitz_dom::Attribute>),
        Text(String),
        Comment(String),
        Other,
    }

    if deep {
        return Ok(doc.mutate().deep_clone_node(node));
    }

    let src = match doc.get_node(node).map(|node| &node.data) {
        Some(NodeData::Element(data)) => {
            CloneSrc::Element(data.name.clone(), data.attrs().to_vec())
        }
        Some(NodeData::Text(data)) => CloneSrc::Text(data.content.clone()),
        Some(NodeData::Comment { contents }) => CloneSrc::Comment(contents.clone()),
        _ => CloneSrc::Other,
    };
    let mut mutr = doc.mutate();
    Ok(match src {
        CloneSrc::Element(name, attrs) => mutr.create_element(name, attrs),
        CloneSrc::Text(content) => mutr.create_text_node(&content),
        CloneSrc::Comment(contents) => mutr.create_comment_node(&contents),
        CloneSrc::Other => mutr.create_comment_node(""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use crate::element;
    use crate::test_support::skeleton;

    fn div(doc: &mut BaseDocument) -> NodeId {
        document::create_element(doc, "div").unwrap()
    }

    #[test]
    fn append_child_attaches_and_returns_the_child() {
        let (mut doc, _html, _head, body) = skeleton();
        let child = div(&mut doc);
        assert_eq!(append_child(&mut doc, body, child).unwrap(), child);
        assert_eq!(parent_node(&doc, child).unwrap(), Some(body));
    }

    /// The detach-first rule: appending an attached node moves it rather than
    /// giving it a second parent.
    #[test]
    fn append_child_moves_an_already_attached_node() {
        let (mut doc, _html, _head, body) = skeleton();
        let first = div(&mut doc);
        let second = div(&mut doc);
        let child = div(&mut doc);
        append_child(&mut doc, body, first).unwrap();
        append_child(&mut doc, body, second).unwrap();
        append_child(&mut doc, first, child).unwrap();
        append_child(&mut doc, second, child).unwrap();

        assert_eq!(parent_node(&doc, child).unwrap(), Some(second));
        assert!(child_nodes(&doc, first).unwrap().is_empty());
    }

    #[test]
    fn insert_before_places_the_node_ahead_of_the_reference() {
        let (mut doc, _html, _head, body) = skeleton();
        let first = div(&mut doc);
        let second = div(&mut doc);
        append_child(&mut doc, body, first).unwrap();
        insert_before(&mut doc, body, second, Some(first)).unwrap();
        assert_eq!(child_nodes(&doc, body).unwrap(), vec![second, first]);
    }

    #[test]
    fn insert_before_with_no_reference_appends() {
        let (mut doc, _html, _head, body) = skeleton();
        let first = div(&mut doc);
        let second = div(&mut doc);
        append_child(&mut doc, body, first).unwrap();
        insert_before(&mut doc, body, second, None).unwrap();
        assert_eq!(child_nodes(&doc, body).unwrap(), vec![first, second]);
    }

    #[test]
    fn insert_before_itself_is_a_no_op() {
        let (mut doc, _html, _head, body) = skeleton();
        let only = div(&mut doc);
        append_child(&mut doc, body, only).unwrap();
        insert_before(&mut doc, body, only, Some(only)).unwrap();
        assert_eq!(child_nodes(&doc, body).unwrap(), vec![only]);
    }

    #[test]
    fn remove_child_detaches_and_returns_the_child() {
        let (mut doc, _html, _head, body) = skeleton();
        let child = div(&mut doc);
        append_child(&mut doc, body, child).unwrap();
        assert_eq!(remove_child(&mut doc, body, child).unwrap(), child);
        assert_eq!(parent_node(&doc, child).unwrap(), None);
        // Detached, not dropped: the node is still addressable.
        assert!(doc.get_node(child).is_some());
    }

    #[test]
    fn replace_child_swaps_in_place_and_returns_the_old_node() {
        let (mut doc, _html, _head, body) = skeleton();
        let before = div(&mut doc);
        let old = div(&mut doc);
        let after = div(&mut doc);
        let new = div(&mut doc);
        for id in [before, old, after] {
            append_child(&mut doc, body, id).unwrap();
        }
        assert_eq!(replace_child(&mut doc, body, new, old).unwrap(), old);
        assert_eq!(child_nodes(&doc, body).unwrap(), vec![before, new, after]);
        assert_eq!(parent_node(&doc, old).unwrap(), None);
    }

    #[test]
    fn first_child_and_siblings_walk_the_child_list() {
        let (mut doc, _html, _head, body) = skeleton();
        let a = div(&mut doc);
        let b = div(&mut doc);
        let c = div(&mut doc);
        for id in [a, b, c] {
            append_child(&mut doc, body, id).unwrap();
        }
        assert_eq!(first_child(&doc, body).unwrap(), Some(a));
        assert_eq!(next_sibling(&doc, a).unwrap(), Some(b));
        assert_eq!(next_sibling(&doc, c).unwrap(), None);
        assert_eq!(previous_sibling(&doc, b).unwrap(), Some(a));
        assert_eq!(previous_sibling(&doc, a).unwrap(), None);
    }

    #[test]
    fn parent_node_is_none_for_a_detached_node() {
        let (mut doc, _html, _head, _body) = skeleton();
        let orphan = div(&mut doc);
        assert_eq!(parent_node(&doc, orphan).unwrap(), None);
    }

    #[test]
    fn child_nodes_lists_every_child_including_text() {
        let (mut doc, _html, _head, body) = skeleton();
        let element = div(&mut doc);
        let text = document::create_text_node(&mut doc, "x").unwrap();
        append_child(&mut doc, body, element).unwrap();
        append_child(&mut doc, body, text).unwrap();
        assert_eq!(child_nodes(&doc, body).unwrap(), vec![element, text]);
    }

    #[test]
    fn has_child_nodes_tracks_the_child_list() {
        let (mut doc, _html, _head, body) = skeleton();
        assert!(!has_child_nodes(&doc, body).unwrap());
        let child = div(&mut doc);
        append_child(&mut doc, body, child).unwrap();
        assert!(has_child_nodes(&doc, body).unwrap());
    }

    #[test]
    fn contains_is_inclusive_and_transitive() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let inner = div(&mut doc);
        let detached = div(&mut doc);
        append_child(&mut doc, body, outer).unwrap();
        append_child(&mut doc, outer, inner).unwrap();

        assert!(contains(&doc, outer, outer).unwrap());
        assert!(contains(&doc, outer, inner).unwrap());
        assert!(contains(&doc, body, inner).unwrap());
        assert!(!contains(&doc, inner, outer).unwrap());
        assert!(!contains(&doc, outer, detached).unwrap());
    }

    #[test]
    fn clone_node_copies_attributes_and_honours_deep() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let inner = div(&mut doc);
        element::set_attribute(&mut doc, outer, "class", "panel").unwrap();
        append_child(&mut doc, outer, inner).unwrap();
        append_child(&mut doc, body, outer).unwrap();

        let shallow = clone_node(&mut doc, outer, false).unwrap();
        assert_eq!(
            element::get_attribute(&doc, shallow, "class").unwrap(),
            Some("panel".to_string())
        );
        assert!(child_nodes(&doc, shallow).unwrap().is_empty());

        let deep = clone_node(&mut doc, outer, true).unwrap();
        assert_eq!(child_nodes(&doc, deep).unwrap().len(), 1);
    }

    #[test]
    fn clone_node_copies_text_and_comment_contents() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "hello").unwrap();
        let comment = document::create_comment(&mut doc, "note").unwrap();
        let text_copy = clone_node(&mut doc, text, false).unwrap();
        let comment_copy = clone_node(&mut doc, comment, false).unwrap();
        assert_eq!(text_content(&doc, text_copy).unwrap(), "hello");
        assert!(matches!(
            doc.get_node(comment_copy).map(|n| &n.data),
            Some(NodeData::Comment { contents }) if contents == "note"
        ));
    }

    #[test]
    fn text_content_concatenates_the_subtree() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let inner = div(&mut doc);
        let a = document::create_text_node(&mut doc, "one ").unwrap();
        let b = document::create_text_node(&mut doc, "two").unwrap();
        append_child(&mut doc, outer, a).unwrap();
        append_child(&mut doc, inner, b).unwrap();
        append_child(&mut doc, outer, inner).unwrap();
        append_child(&mut doc, body, outer).unwrap();
        assert_eq!(text_content(&doc, outer).unwrap(), "one two");
    }

    /// The buffer variants agree with the owning one over the same subtree.
    ///
    /// All three go through `blitz-dom`'s single `write_text_content`
    /// traversal, which is why this is an equivalence and not three
    /// independently-maintained expectations. A private copy of that walk in
    /// this crate is exactly what this would stop catching.
    #[test]
    fn text_content_into_agrees_with_text_content() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let inner = div(&mut doc);
        let a = document::create_text_node(&mut doc, "one ").unwrap();
        let b = document::create_text_node(&mut doc, "two").unwrap();
        append_child(&mut doc, outer, a).unwrap();
        append_child(&mut doc, inner, b).unwrap();
        append_child(&mut doc, outer, inner).unwrap();
        append_child(&mut doc, body, outer).unwrap();

        let owned = text_content(&doc, outer).unwrap();
        assert_eq!(text_content_len(&doc, outer).unwrap(), owned.len());

        let mut buf = [0u8; 32];
        assert_eq!(
            text_content_into(&doc, outer, &mut buf).unwrap(),
            owned.len()
        );
        assert_eq!(&buf[..owned.len()], owned.as_bytes());

        // An empty subtree is length zero, not an error and not absent: every
        // node has text content. This is why the reader has no `ABSENT` case.
        let empty = div(&mut doc);
        append_child(&mut doc, body, empty).unwrap();
        assert_eq!(text_content_into(&doc, empty, &mut buf).unwrap(), 0);
    }

    /// The `snprintf` contract, across a concatenation rather than a single
    /// stored value: the length is reported and the buffer is left untouched.
    ///
    /// The measure-then-fill split exists for exactly this case. A single
    /// streaming pass would have written `one ` before discovering that `two`
    /// did not fit, and the guarantee the ABI states would be false.
    #[test]
    fn text_content_into_writes_nothing_when_it_does_not_fit() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let a = document::create_text_node(&mut doc, "one ").unwrap();
        let b = document::create_text_node(&mut doc, "two").unwrap();
        append_child(&mut doc, outer, a).unwrap();
        append_child(&mut doc, outer, b).unwrap();
        append_child(&mut doc, body, outer).unwrap();

        let mut buf = [b'.'; 5];
        assert_eq!(text_content_into(&doc, outer, &mut buf).unwrap(), 7);
        assert_eq!(
            &buf, b".....",
            "a partial write would have left `one ` here"
        );
    }

    #[test]
    fn set_text_content_replaces_children_with_one_text_node() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        let old_child = div(&mut doc);
        append_child(&mut doc, outer, old_child).unwrap();
        append_child(&mut doc, body, outer).unwrap();

        set_text_content(&mut doc, outer, "replaced").unwrap();
        assert_eq!(child_nodes(&doc, outer).unwrap().len(), 1);
        assert_eq!(text_content(&doc, outer).unwrap(), "replaced");
        // Detached, not dropped.
        assert!(doc.get_node(old_child).is_some());
        assert_eq!(parent_node(&doc, old_child).unwrap(), None);
    }

    #[test]
    fn set_text_content_empty_leaves_no_children() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = div(&mut doc);
        append_child(&mut doc, body, outer).unwrap();
        set_text_content(&mut doc, outer, "x").unwrap();
        set_text_content(&mut doc, outer, "").unwrap();
        assert!(child_nodes(&doc, outer).unwrap().is_empty());
    }

    #[test]
    fn set_text_content_on_a_text_node_rewrites_it_in_place() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "before").unwrap();
        set_text_content(&mut doc, text, "after").unwrap();
        assert_eq!(text_content(&doc, text).unwrap(), "after");
        assert!(child_nodes(&doc, text).unwrap().is_empty());
    }

    #[test]
    fn compare_document_position_reports_order_and_containment() {
        use document_position::*;
        let (mut doc, _html, _head, body) = skeleton();
        let first = div(&mut doc);
        let second = div(&mut doc);
        let nested = div(&mut doc);
        append_child(&mut doc, body, first).unwrap();
        append_child(&mut doc, body, second).unwrap();
        append_child(&mut doc, first, nested).unwrap();

        assert_eq!(compare_document_position(&doc, first, first).unwrap(), 0);
        assert_eq!(
            compare_document_position(&doc, first, second).unwrap(),
            FOLLOWING
        );
        assert_eq!(
            compare_document_position(&doc, second, first).unwrap(),
            PRECEDING
        );
        assert_eq!(
            compare_document_position(&doc, first, nested).unwrap(),
            FOLLOWING | CONTAINED_BY
        );
        assert_eq!(
            compare_document_position(&doc, nested, first).unwrap(),
            PRECEDING | CONTAINS
        );
    }

    #[test]
    fn compare_document_position_flags_disconnected_nodes() {
        use document_position::*;
        let (mut doc, _html, _head, body) = skeleton();
        let attached = div(&mut doc);
        let detached = div(&mut doc);
        append_child(&mut doc, body, attached).unwrap();
        let result = compare_document_position(&doc, attached, detached).unwrap();
        assert_eq!(result & DISCONNECTED, DISCONNECTED);
        assert_eq!(result & IMPLEMENTATION_SPECIFIC, IMPLEMENTATION_SPECIFIC);
        assert_ne!(result & (PRECEDING | FOLLOWING), 0);
    }
}

//! `CharacterData.data`, which `blitz-script` implements as the `nodeValue`
//! accessor pair shared with `Node`.
//!
//! Upstream: `blitz-script/src/dom/node.rs` (`node_value` / `set_node_value`,
//! registered on the CharacterData prototype as `data`). See MAPPING.md.

use blitz_dom::node::NodeData;
use blitz_dom::{BaseDocument, NodeId};

use crate::Result;

/// `characterData.data`.
///
/// `None` is the DOM's `null`, which is what a node with no character data
/// returns. Note the copied quirk: a **comment reports an empty string**, not
/// its contents, because upstream's `node_value` only special-cases text.
/// [`crate::node::clone_node`] does copy comment contents, so the two disagree
/// and this is the one that is wrong; it is preserved so a reparented
/// `blitz-script` does not change behaviour.
pub fn data(doc: &BaseDocument, node: NodeId) -> Result<Option<String>> {
    Ok(match doc.get_node(node).map(|node| &node.data) {
        Some(NodeData::Text(data)) => Some(data.content.clone()),
        Some(NodeData::Comment { .. }) => Some(String::new()),
        _ => None,
    })
}

/// `characterData.data = text`.
///
/// Applies to any node the mutator will set text on; upstream does not
/// restrict it to character data either.
pub fn set_data(doc: &mut BaseDocument, node: NodeId, text: &str) -> Result<()> {
    doc.mutate().set_node_text(node, text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use crate::test_support::skeleton;

    #[test]
    fn data_reads_a_text_node() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "hello").unwrap();
        assert_eq!(data(&doc, text).unwrap(), Some("hello".to_string()));
    }

    #[test]
    fn data_is_null_for_an_element_and_empty_for_a_comment() {
        let (mut doc, _html, _head, body) = skeleton();
        assert_eq!(data(&doc, body).unwrap(), None);
        let comment = document::create_comment(&mut doc, "note").unwrap();
        assert_eq!(data(&doc, comment).unwrap(), Some(String::new()));
    }

    #[test]
    fn set_data_rewrites_the_text_in_place() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "before").unwrap();
        set_data(&mut doc, text, "after").unwrap();
        assert_eq!(data(&doc, text).unwrap(), Some("after".to_string()));
    }
}

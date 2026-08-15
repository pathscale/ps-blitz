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
/// returns. A comment reports its contents, as `CharacterData` requires.
///
/// That last part was a bug in the fork until this crate was written. The
/// contents were on the node all along (`NodeData::Comment { contents }`), and
/// `clone_node` copied them, but the accessor returned `""` and the mutator
/// dropped writes on the floor, so a script could clone a comment and read
/// back text the original refused to report. Fixed across all three sites in
/// one change, `blitz-script` included, so reparenting is still
/// behaviour-preserving. See MAPPING.md.
pub fn data(doc: &BaseDocument, node: NodeId) -> Result<Option<String>> {
    Ok(match doc.get_node(node).map(|node| &node.data) {
        Some(NodeData::Text(data)) => Some(data.content.clone()),
        Some(NodeData::Comment { contents }) => Some(contents.clone()),
        _ => None,
    })
}

/// `characterData.data = text`.
///
/// Applies to any node the mutator will set text on: text nodes and comments.
/// Upstream does not restrict it to character data either, and on anything
/// else it is a no-op rather than an error.
///
/// A comment write is inert by design. A comment generates no layout box, so
/// this schedules no damage and no relayout, which is asserted in
/// `blitz-dom`'s own `setting_a_comments_data_does_not_dirty_layout`.
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
    fn data_is_null_for_an_element() {
        let (doc, _html, _head, body) = skeleton();
        assert_eq!(data(&doc, body).unwrap(), None);
    }

    #[test]
    fn data_reads_a_comments_contents() {
        let (mut doc, _html, _head, _body) = skeleton();
        let comment = document::create_comment(&mut doc, "note").unwrap();
        assert_eq!(data(&doc, comment).unwrap(), Some("note".to_string()));
    }

    #[test]
    fn set_data_rewrites_the_text_in_place() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "before").unwrap();
        set_data(&mut doc, text, "after").unwrap();
        assert_eq!(data(&doc, text).unwrap(), Some("after".to_string()));
    }

    /// The round trip that would have caught the original disagreement: read,
    /// write, read back, then clone and read the clone. `clone_node` copied
    /// comment contents from the start, so the clone leg is what made the
    /// getter's `""` provably wrong rather than merely undocumented.
    #[test]
    fn a_comments_data_round_trips_through_a_write_and_a_clone() {
        let (mut doc, _html, _head, _body) = skeleton();
        let comment = document::create_comment(&mut doc, "first").unwrap();
        assert_eq!(data(&doc, comment).unwrap(), Some("first".to_string()));

        set_data(&mut doc, comment, "second").unwrap();
        assert_eq!(data(&doc, comment).unwrap(), Some("second".to_string()));

        let copy = crate::node::clone_node(&mut doc, comment, false).unwrap();
        assert_ne!(copy, comment);
        assert_eq!(
            data(&doc, copy).unwrap(),
            data(&doc, comment).unwrap(),
            "a comment and its clone must report the same data"
        );
        assert_eq!(data(&doc, copy).unwrap(), Some("second".to_string()));
    }
}

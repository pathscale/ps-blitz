//! Element operations: attributes, `classList`, `innerHTML`, scoped matching.
//!
//! Upstream: `blitz-script/src/dom/element.rs`. See MAPPING.md.
//!
//! Tolerance is copied from upstream: an operation naming a node that is not
//! an element behaves as though the attribute were simply absent, rather than
//! raising. `blitz-script` has no `NotAnElement` case and neither does this.

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName};

use crate::Result;
use crate::error::DomError;

/// A namespace-less qualified name, which is what an attribute takes.
pub(crate) fn attr_name(local: &str) -> QualName {
    QualName::new(None, markup5ever::ns!(), LocalName::from(local))
}

pub(crate) fn read_attr(doc: &BaseDocument, node_id: NodeId, name: &str) -> Option<String> {
    let element = doc.get_node(node_id)?.element_data()?;
    element
        .attrs()
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.to_string())
}

/// `local == name.to_ascii_lowercase()`, without allocating the lowercased
/// copy.
///
/// `to_ascii_lowercase` maps each ASCII byte to one ASCII byte and leaves every
/// other byte alone, so byte length is preserved and this is exactly the
/// comparison [`read_attr`]'s callers make — one byte at a time instead of one
/// `String` up front.
fn local_matches(local: &str, name: &str) -> bool {
    local.len() == name.len()
        && local
            .bytes()
            .zip(name.bytes())
            .all(|(stored, queried)| stored == queried.to_ascii_lowercase())
}

/// Borrow an attribute's value out of the document, without copying it.
///
/// # Why this is allowed to return a borrow
///
/// The crate's borrow discipline — every reader returns an owned value, so no
/// borrow survives a call and a runtime re-entering between operations cannot
/// find a live one — is a rule about the **public** surface. This is
/// `pub(crate)`, its borrow never escapes the function that took it, and every
/// public caller either clones it or writes it into a buffer the caller
/// supplied. The discipline is intact; what changes is that the copy is no
/// longer mandatory *inside* the crate.
///
/// This is the whole mechanism behind the buffer-writing readers. See
/// MAPPING.md, "Readers allocate a `String`".
pub(crate) fn find_attr<'doc>(
    doc: &'doc BaseDocument,
    node_id: NodeId,
    name: &str,
) -> Option<&'doc str> {
    let element = doc.get_node(node_id)?.element_data()?;
    element
        .attrs()
        .iter()
        .find(|attr| local_matches(&attr.name.local, name))
        .map(|attr| &*attr.value)
}

pub(crate) fn write_attr(doc: &mut BaseDocument, node_id: NodeId, name: &str, value: &str) {
    doc.mutate().set_attribute(node_id, attr_name(name), value);
}

pub(crate) fn clear_attr(doc: &mut BaseDocument, node_id: NodeId, name: &str) {
    doc.mutate().clear_attribute(node_id, attr_name(name));
}

// === Basic element info ===

/// `element.tagName`, upper-cased as the DOM specifies.
///
/// Empty string for a node that is not an element, matching upstream.
pub fn tag_name(doc: &BaseDocument, node: NodeId) -> Result<String> {
    Ok(doc
        .get_node(node)
        .and_then(|node| node.element_data())
        .map(|element| element.name.local.to_uppercase())
        .unwrap_or_default())
}

// === Attributes ===

/// `element.getAttribute(name)`.
///
/// The name is ASCII-lowercased before the lookup, as upstream does.
/// `None` is the DOM's `null`, meaning the attribute is absent, which is
/// distinct from present-and-empty.
pub fn get_attribute(doc: &BaseDocument, node: NodeId, name: &str) -> Result<Option<String>> {
    Ok(read_attr(doc, node, &name.to_ascii_lowercase()))
}

/// `element.setAttribute(name, value)`.
///
/// The caller is responsible for marking layout dirty; see the crate root.
pub fn set_attribute(doc: &mut BaseDocument, node: NodeId, name: &str, value: &str) -> Result<()> {
    write_attr(doc, node, &name.to_ascii_lowercase(), value);
    Ok(())
}

/// `element.removeAttribute(name)`.
pub fn remove_attribute(doc: &mut BaseDocument, node: NodeId, name: &str) -> Result<()> {
    clear_attr(doc, node, &name.to_ascii_lowercase());
    Ok(())
}

/// `element.hasAttribute(name)`.
///
/// Allocates nothing. It used to allocate twice — the lowercased name, and a
/// clone of the attribute's whole value, thrown away to answer a boolean —
/// which is a cost with no boundary traffic at all to justify it. See
/// MAPPING.md.
pub fn has_attribute(doc: &BaseDocument, node: NodeId, name: &str) -> Result<bool> {
    Ok(find_attr(doc, node, name).is_some())
}

/// `element.getAttribute(name)`, written into a caller-supplied buffer.
///
/// **The buffer-writing variant of [`get_attribute`], and the reason it
/// exists**: the owning reader allocates a `String` that a binding then copies
/// again into wherever it actually wanted the bytes. This writes into that
/// destination directly, so the intermediate never exists.
///
/// - `Ok(None)` — the attribute is absent, which is the DOM's `null` and is not
///   the same as present-and-empty. `out` is untouched.
/// - `Ok(Some(len))` — `len` is the value's **full** byte length, always. The
///   bytes were written to `out[..len]` if and only if `len <= out.len()`; a
///   value that does not fit leaves `out` untouched rather than truncated,
///   because half a UTF-8 string is not a string.
///
/// That is `snprintf`'s convention, and it is what lets a caller size a buffer
/// from one call and get the bytes from a second, without the reader ever
/// having to own them.
///
/// [`get_attribute`] is kept and is not deprecated: a caller that wants a
/// `String` should not have to supply a buffer and then build one.
pub fn get_attribute_into(
    doc: &BaseDocument,
    node: NodeId,
    name: &str,
    out: &mut [u8],
) -> Result<Option<usize>> {
    let Some(value) = find_attr(doc, node, name) else {
        return Ok(None);
    };
    let len = value.len();
    if len <= out.len() {
        out[..len].copy_from_slice(value.as_bytes());
    }
    Ok(Some(len))
}

// === classList ===

fn class_tokens(doc: &BaseDocument, node_id: NodeId) -> Vec<String> {
    read_attr(doc, node_id, "class")
        .unwrap_or_default()
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect()
}

fn write_class_tokens(doc: &mut BaseDocument, node_id: NodeId, tokens: &[String]) {
    write_attr(doc, node_id, "class", &tokens.join(" "));
}

/// Validate a `DOMTokenList` token. Upstream raises a `SyntaxError` here.
fn class_token(token: &str) -> Result<&str> {
    if token.is_empty() || token.chars().any(|ch| ch.is_ascii_whitespace()) {
        return Err(DomError::InvalidClassToken(token.to_owned()));
    }
    Ok(token)
}

/// `element.classList.add(...tokens)`.
///
/// Every token is validated before anything is written, so a bad token in the
/// list leaves the attribute untouched. That is upstream's ordering too: it
/// collects all tokens through `class_token` before reading the attribute.
pub fn class_list_add(doc: &mut BaseDocument, node: NodeId, tokens: &[&str]) -> Result<()> {
    let to_add = tokens
        .iter()
        .map(|token| class_token(token).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let mut current = class_tokens(doc, node);
    for token in to_add {
        if !current.contains(&token) {
            current.push(token);
        }
    }
    write_class_tokens(doc, node, &current);
    Ok(())
}

/// `element.classList.remove(...tokens)`.
pub fn class_list_remove(doc: &mut BaseDocument, node: NodeId, tokens: &[&str]) -> Result<()> {
    let to_remove = tokens
        .iter()
        .map(|token| class_token(token).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let mut current = class_tokens(doc, node);
    current.retain(|token| !to_remove.contains(token));
    write_class_tokens(doc, node, &current);
    Ok(())
}

/// `element.classList.toggle(token, force)`, returning whether the token is
/// present afterwards.
pub fn class_list_toggle(
    doc: &mut BaseDocument,
    node: NodeId,
    token: &str,
    force: Option<bool>,
) -> Result<bool> {
    let token = class_token(token)?.to_owned();
    let mut tokens = class_tokens(doc, node);
    let present = tokens.contains(&token);
    let retain = force.unwrap_or(!present);
    if retain && !present {
        tokens.push(token);
    } else if !retain && present {
        tokens.retain(|item| item != &token);
    }
    write_class_tokens(doc, node, &tokens);
    Ok(retain)
}

/// `element.classList.contains(token)`.
pub fn class_list_contains(doc: &BaseDocument, node: NodeId, token: &str) -> Result<bool> {
    let token = class_token(token)?;
    Ok(class_tokens(doc, node).iter().any(|item| item == token))
}

// === innerHTML ===

/// `element.innerHTML`, serialised from the children.
pub fn inner_html(doc: &BaseDocument, node: NodeId) -> Result<String> {
    let mut html = String::new();
    if let Some(node) = doc.get_node(node) {
        for child_id in &node.children {
            if let Some(child) = doc.get_node(*child_id) {
                child.write_outer_html(&mut html);
            }
        }
    }
    Ok(html)
}

/// `element.innerHTML = html`.
///
/// Existing children are detached rather than dropped, so wrappers a runtime
/// still holds for them stay valid. That detach is the only part of this
/// operation the facade performs.
///
/// **Parsing is the document's job, not this crate's.** The new children come
/// from `BaseDocument::html_parser_provider`, and the default provider,
/// [`blitz_dom::DummyHtmlParserProvider`], parses nothing at all: against a
/// document built with `DocumentConfig::default()` this call empties the
/// element and stops. An embedder that wants `innerHTML` to work configures
/// `blitz-html`'s provider. This crate does not depend on `blitz-html`, so it
/// cannot check for you.
pub fn set_inner_html(doc: &mut BaseDocument, node: NodeId, html: &str) -> Result<()> {
    let mut mutr = doc.mutate();
    for child_id in mutr.child_ids(node) {
        mutr.remove_node(child_id);
    }
    mutr.set_inner_html(node, html);
    Ok(())
}

// === Selector matching ===

/// `element.matches(selector)`.
///
/// Deliberate difference: an unparseable selector is an error. Upstream's
/// `is_ok_and` maps it to `false`.
pub fn matches(doc: &BaseDocument, node: NodeId, selector: &str) -> Result<bool> {
    doc.query_selector_all(selector)
        .map(|matches| matches.contains(&node))
        .map_err(|_| DomError::InvalidSelector(selector.to_owned()))
}

/// `element.closest(selector)`: this node or the nearest ancestor that matches.
///
/// Deliberate difference: an unparseable selector is an error. Upstream's
/// `unwrap_or_default` searches an empty match set and returns `null`.
pub fn closest(doc: &BaseDocument, node: NodeId, selector: &str) -> Result<Option<NodeId>> {
    let matches = doc
        .query_selector_all(selector)
        .map_err(|_| DomError::InvalidSelector(selector.to_owned()))?;
    let mut current = Some(node);
    while let Some(id) = current {
        if matches.contains(&id) {
            return Ok(Some(id));
        }
        current = doc.get_node(id).and_then(|node| node.parent);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use crate::node;
    use crate::test_support::skeleton;

    fn attached_div(doc: &mut BaseDocument, parent: NodeId) -> NodeId {
        let id = document::create_element(doc, "div").unwrap();
        node::append_child(doc, parent, id).unwrap();
        id
    }

    #[test]
    fn tag_name_is_upper_case() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        assert_eq!(tag_name(&doc, id).unwrap(), "DIV");
    }

    #[test]
    fn tag_name_of_a_text_node_is_empty() {
        let (mut doc, _html, _head, _body) = skeleton();
        let text = document::create_text_node(&mut doc, "x").unwrap();
        assert_eq!(tag_name(&doc, text).unwrap(), "");
    }

    #[test]
    fn set_and_get_attribute_round_trip() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "data-role", "panel").unwrap();
        assert_eq!(
            get_attribute(&doc, id, "data-role").unwrap(),
            Some("panel".to_string())
        );
    }

    #[test]
    fn attribute_names_are_lowercased_on_both_sides() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "DATA-Role", "panel").unwrap();
        assert_eq!(
            get_attribute(&doc, id, "data-role").unwrap(),
            Some("panel".to_string())
        );
        assert!(has_attribute(&doc, id, "Data-ROLE").unwrap());
    }

    #[test]
    fn get_attribute_is_none_when_absent_and_some_when_empty() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        assert_eq!(get_attribute(&doc, id, "title").unwrap(), None);
        set_attribute(&mut doc, id, "title", "").unwrap();
        assert_eq!(
            get_attribute(&doc, id, "title").unwrap(),
            Some(String::new())
        );
    }

    /// The buffer variant agrees with the owning one, value for value.
    ///
    /// Asserted as an equivalence rather than against literals: the two are a
    /// pair, and the failure worth catching is them drifting apart.
    #[test]
    fn get_attribute_into_agrees_with_get_attribute() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "data-role", "panel").unwrap();
        set_attribute(&mut doc, id, "title", "").unwrap();

        for name in ["data-role", "title", "absent"] {
            let owned = get_attribute(&doc, id, name).unwrap();
            let mut buf = [0u8; 64];
            let written = get_attribute_into(&doc, id, name, &mut buf).unwrap();
            match owned {
                None => assert_eq!(written, None, "{name} should be absent both ways"),
                Some(value) => {
                    assert_eq!(written, Some(value.len()), "{name} length");
                    assert_eq!(&buf[..value.len()], value.as_bytes(), "{name} bytes");
                }
            }
        }
    }

    /// The `snprintf` contract: the full length is always reported, and a value
    /// that does not fit leaves the buffer untouched rather than truncated.
    #[test]
    fn get_attribute_into_reports_the_length_it_could_not_write() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "data-role", "panel").unwrap();

        // Too small by one. Nothing is written — not four bytes of `pane`.
        let mut buf = [b'.'; 4];
        assert_eq!(
            get_attribute_into(&doc, id, "data-role", &mut buf).unwrap(),
            Some(5)
        );
        assert_eq!(
            &buf, b"....",
            "a value that does not fit must not be written"
        );

        // Zero capacity is legal, and is how a caller asks for a length alone.
        assert_eq!(
            get_attribute_into(&doc, id, "data-role", &mut []).unwrap(),
            Some(5)
        );

        // Sized from that answer, it fits exactly.
        let mut buf = vec![0u8; 5];
        assert_eq!(
            get_attribute_into(&doc, id, "data-role", &mut buf).unwrap(),
            Some(5)
        );
        assert_eq!(&buf, b"panel");
    }

    /// The name is matched case-insensitively without the lowercased copy the
    /// owning reader allocates, so the two must still agree on a mixed-case
    /// query.
    #[test]
    fn get_attribute_into_lowercases_the_queried_name() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "data-role", "panel").unwrap();

        let mut buf = [0u8; 16];
        assert_eq!(
            get_attribute_into(&doc, id, "DATA-Role", &mut buf).unwrap(),
            Some(5)
        );
        assert_eq!(&buf[..5], b"panel");
        assert!(has_attribute(&doc, id, "Data-ROLE").unwrap());

        // And a name of a different length is not a match, which is what the
        // byte-length check in `local_matches` is for.
        assert_eq!(
            get_attribute_into(&doc, id, "data-rol", &mut buf).unwrap(),
            None
        );
    }

    #[test]
    fn remove_attribute_clears_it() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "title", "x").unwrap();
        remove_attribute(&mut doc, id, "title").unwrap();
        assert_eq!(get_attribute(&doc, id, "title").unwrap(), None);
    }

    #[test]
    fn has_attribute_tracks_presence() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        assert!(!has_attribute(&doc, id, "hidden").unwrap());
        set_attribute(&mut doc, id, "hidden", "").unwrap();
        assert!(has_attribute(&doc, id, "hidden").unwrap());
    }

    #[test]
    fn class_list_add_is_idempotent_and_ordered() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        class_list_add(&mut doc, id, &["a", "b"]).unwrap();
        class_list_add(&mut doc, id, &["a", "c"]).unwrap();
        assert_eq!(
            get_attribute(&doc, id, "class").unwrap(),
            Some("a b c".to_string())
        );
    }

    #[test]
    fn class_list_remove_drops_every_named_token() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        class_list_add(&mut doc, id, &["a", "b", "c"]).unwrap();
        class_list_remove(&mut doc, id, &["a", "c"]).unwrap();
        assert_eq!(
            get_attribute(&doc, id, "class").unwrap(),
            Some("b".to_string())
        );
    }

    #[test]
    fn class_list_toggle_flips_and_honours_force() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        assert!(class_list_toggle(&mut doc, id, "on", None).unwrap());
        assert!(class_list_contains(&doc, id, "on").unwrap());
        assert!(!class_list_toggle(&mut doc, id, "on", None).unwrap());
        assert!(!class_list_contains(&doc, id, "on").unwrap());

        assert!(class_list_toggle(&mut doc, id, "on", Some(true)).unwrap());
        assert!(class_list_toggle(&mut doc, id, "on", Some(true)).unwrap());
        assert!(class_list_contains(&doc, id, "on").unwrap());
        assert!(!class_list_toggle(&mut doc, id, "on", Some(false)).unwrap());
        assert!(!class_list_contains(&doc, id, "on").unwrap());
    }

    #[test]
    fn class_list_contains_reads_the_class_attribute() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        set_attribute(&mut doc, id, "class", "  a   b ").unwrap();
        assert!(class_list_contains(&doc, id, "a").unwrap());
        assert!(class_list_contains(&doc, id, "b").unwrap());
        assert!(!class_list_contains(&doc, id, "c").unwrap());
    }

    #[test]
    fn an_invalid_class_token_is_rejected_before_anything_is_written() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        class_list_add(&mut doc, id, &["keep"]).unwrap();

        assert!(matches!(
            class_list_add(&mut doc, id, &["ok", "two words"]),
            Err(DomError::InvalidClassToken(_))
        ));
        assert!(matches!(
            class_list_toggle(&mut doc, id, "", None),
            Err(DomError::InvalidClassToken(_))
        ));
        assert!(matches!(
            class_list_contains(&doc, id, "a b"),
            Err(DomError::InvalidClassToken(_))
        ));
        assert_eq!(
            get_attribute(&doc, id, "class").unwrap(),
            Some("keep".to_string())
        );
    }

    /// Built through the facade rather than through `set_inner_html`, because
    /// parsing needs a provider this crate does not depend on. See the test
    /// below.
    #[test]
    fn inner_html_serialises_the_children() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = attached_div(&mut doc, body);
        let child = document::create_element(&mut doc, "span").unwrap();
        set_attribute(&mut doc, child, "class", "x").unwrap();
        let text = document::create_text_node(&mut doc, "hi").unwrap();
        node::append_child(&mut doc, child, text).unwrap();
        node::append_child(&mut doc, outer, child).unwrap();

        assert_eq!(
            inner_html(&doc, outer).unwrap(),
            "<span class=\"x\">hi</span>"
        );
        assert_eq!(inner_html(&doc, child).unwrap(), "hi");
    }

    #[test]
    fn inner_html_of_a_childless_element_is_empty() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = attached_div(&mut doc, body);
        assert_eq!(inner_html(&doc, outer).unwrap(), "");
    }

    /// The half of `set_inner_html` that belongs to this crate: the old
    /// children come off, and they are detached rather than dropped so a
    /// runtime's wrappers for them stay valid.
    #[test]
    fn set_inner_html_detaches_the_previous_children() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = attached_div(&mut doc, body);
        let old = attached_div(&mut doc, outer);
        set_inner_html(&mut doc, outer, "<span></span>").unwrap();
        assert_eq!(node::parent_node(&doc, old).unwrap(), None);
        assert!(
            doc.get_node(old).is_some(),
            "the old child should be detached, not dropped"
        );
    }

    /// The other half is the document's, and against the default provider
    /// there is no other half. Asserted rather than assumed: a binding that
    /// forgets to configure `blitz-html` gets a silently empty element, and
    /// this is the test that says so out loud.
    #[test]
    fn set_inner_html_parses_nothing_without_a_parser_provider() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = attached_div(&mut doc, body);
        set_inner_html(&mut doc, outer, "<span>hi</span>").unwrap();
        assert!(
            node::child_nodes(&doc, outer).unwrap().is_empty(),
            "DummyHtmlParserProvider should have parsed nothing"
        );
    }

    #[test]
    fn matches_tests_this_element_against_the_selector() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        class_list_add(&mut doc, id, &["panel"]).unwrap();
        assert!(matches(&doc, id, ".panel").unwrap());
        assert!(!matches(&doc, id, ".other").unwrap());
    }

    #[test]
    fn closest_checks_this_node_first_then_walks_up() {
        let (mut doc, _html, _head, body) = skeleton();
        let outer = attached_div(&mut doc, body);
        let inner = attached_div(&mut doc, outer);
        class_list_add(&mut doc, outer, &["panel"]).unwrap();

        assert_eq!(closest(&doc, inner, ".panel").unwrap(), Some(outer));
        class_list_add(&mut doc, inner, &["panel"]).unwrap();
        assert_eq!(closest(&doc, inner, ".panel").unwrap(), Some(inner));
        assert_eq!(closest(&doc, inner, ".absent").unwrap(), None);
    }

    #[test]
    fn an_unparseable_selector_is_an_error_in_matches_and_closest() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = attached_div(&mut doc, body);
        assert!(matches!(
            matches(&doc, id, "!!!"),
            Err(DomError::InvalidSelector(_))
        ));
        assert!(matches!(
            closest(&doc, id, "!!!"),
            Err(DomError::InvalidSelector(_))
        ));
    }
}

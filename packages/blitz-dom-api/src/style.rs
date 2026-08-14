//! `CSSStyleDeclaration` operations against the inline `style` attribute.
//!
//! Upstream: `blitz-script/src/dom/style.rs`. See MAPPING.md.
//!
//! These read and write the *inline* style attribute only. None of them
//! consults computed style, which is upstream's limitation as well and is why
//! `getPropertyValue` answers `""` for a property that a stylesheet set.
//!
//! A style write changes geometry. The caller marks layout dirty; see the
//! crate root.

use blitz_dom::{BaseDocument, NodeId};

use crate::Result;
use crate::element::attr_name;

/// Split a style attribute into `(property, value)` pairs.
///
/// A simplification carried over from upstream: it does not handle `;` or `:`
/// inside values, so `background: url(a;b)` parses wrongly.
fn parse_declarations(style_attr: &str) -> Vec<(String, String)> {
    style_attr
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .map(|(prop, value)| (prop.trim().to_string(), value.trim().to_string()))
        .filter(|(prop, _)| !prop.is_empty())
        .collect()
}

fn serialize_declarations(decls: &[(String, String)]) -> String {
    decls
        .iter()
        .map(|(prop, value)| format!("{prop}: {value};"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn style_attr(doc: &BaseDocument, node_id: NodeId) -> String {
    doc.get_node(node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("style")))
        .unwrap_or_default()
        .to_string()
}

fn update_style_attr(
    doc: &mut BaseDocument,
    node_id: NodeId,
    f: impl FnOnce(&mut Vec<(String, String)>),
) {
    let mut decls = parse_declarations(&style_attr(doc, node_id));
    f(&mut decls);
    let new_style = serialize_declarations(&decls);
    doc.mutate()
        .set_attribute(node_id, attr_name("style"), &new_style);
}

/// `style.getPropertyValue(name)`.
///
/// Empty string when the property is not set inline, which is what the DOM
/// specifies for an unset property.
pub fn get_property_value(doc: &BaseDocument, node: NodeId, name: &str) -> Result<String> {
    Ok(parse_declarations(&style_attr(doc, node))
        .into_iter()
        .find(|(prop, _)| prop.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
        .unwrap_or_default())
}

/// `style.setProperty(name, value)`.
///
/// Setting a property to the empty string removes it, matching upstream, and
/// a re-set moves the declaration to the end of the attribute.
pub fn set_property(doc: &mut BaseDocument, node: NodeId, name: &str, value: &str) -> Result<()> {
    let value = value.to_owned();
    update_style_attr(doc, node, |decls| {
        decls.retain(|(prop, _)| !prop.eq_ignore_ascii_case(name));
        if !value.is_empty() {
            decls.push((name.to_ascii_lowercase(), value));
        }
    });
    Ok(())
}

/// `style.removeProperty(name)`, returning the value that was removed.
///
/// Empty string when the property was not set, as the DOM specifies.
pub fn remove_property(doc: &mut BaseDocument, node: NodeId, name: &str) -> Result<String> {
    let mut removed = String::new();
    update_style_attr(doc, node, |decls| {
        if let Some((_, value)) = decls
            .iter()
            .find(|(prop, _)| prop.eq_ignore_ascii_case(name))
        {
            removed = value.clone();
        }
        decls.retain(|(prop, _)| !prop.eq_ignore_ascii_case(name));
    });
    Ok(removed)
}

/// `style.cssText`: the raw inline style attribute.
pub fn css_text(doc: &BaseDocument, node: NodeId) -> Result<String> {
    Ok(style_attr(doc, node))
}

/// `style.cssText = css`: replace the whole inline style attribute.
///
/// The setter half of the `cssText` accessor. Written verbatim, without a
/// parse-and-reserialise round trip, which is what upstream does.
pub fn set_css_text(doc: &mut BaseDocument, node: NodeId, css: &str) -> Result<()> {
    doc.mutate().set_attribute(node, attr_name("style"), css);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use crate::element;
    use crate::node;
    use crate::test_support::skeleton;

    fn styled(doc: &mut BaseDocument, parent: NodeId) -> NodeId {
        let id = document::create_element(doc, "div").unwrap();
        node::append_child(doc, parent, id).unwrap();
        id
    }

    #[test]
    fn set_property_then_get_property_value_round_trips() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_property(&mut doc, id, "height", "70px").unwrap();
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "70px");
    }

    #[test]
    fn get_property_value_is_empty_for_an_unset_property() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "");
    }

    #[test]
    fn set_property_replaces_rather_than_appends_a_duplicate() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_property(&mut doc, id, "height", "10px").unwrap();
        set_property(&mut doc, id, "height", "20px").unwrap();
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "20px");
        assert_eq!(css_text(&doc, id).unwrap().matches("height").count(), 1);
    }

    /// The copied idiom that autosizing depends on: an empty value removes.
    #[test]
    fn setting_a_property_to_the_empty_string_removes_it() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_property(&mut doc, id, "height", "10px").unwrap();
        set_property(&mut doc, id, "height", "").unwrap();
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "");
    }

    #[test]
    fn remove_property_returns_what_it_removed() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_property(&mut doc, id, "height", "10px").unwrap();
        assert_eq!(remove_property(&mut doc, id, "height").unwrap(), "10px");
        assert_eq!(remove_property(&mut doc, id, "height").unwrap(), "");
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "");
    }

    #[test]
    fn css_text_reads_the_inline_style_attribute() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        element::set_attribute(&mut doc, id, "style", "color: red;").unwrap();
        assert_eq!(css_text(&doc, id).unwrap(), "color: red;");
    }

    #[test]
    fn set_css_text_replaces_every_declaration() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_property(&mut doc, id, "height", "10px").unwrap();
        set_css_text(&mut doc, id, "width: 5px;").unwrap();
        assert_eq!(css_text(&doc, id).unwrap(), "width: 5px;");
        assert_eq!(get_property_value(&doc, id, "height").unwrap(), "");
        assert_eq!(get_property_value(&doc, id, "width").unwrap(), "5px");
    }

    #[test]
    fn property_names_match_case_insensitively() {
        let (mut doc, _html, _head, body) = skeleton();
        let id = styled(&mut doc, body);
        set_css_text(&mut doc, id, "Max-Height: 4px;").unwrap();
        assert_eq!(get_property_value(&doc, id, "max-height").unwrap(), "4px");
        assert_eq!(remove_property(&mut doc, id, "MAX-HEIGHT").unwrap(), "4px");
    }
}

//! The role an element has, and the AccessKit tree built out of those roles.

use crate::node::ElementData;
use crate::{BaseDocument, Node as BlitzDomNode, local_name};
use accesskit::{Node as AccessKitNode, NodeId, Role, Tree, TreeId, TreeUpdate};

/// An attribute by local name, for the names that are not static atoms.
///
/// `ElementData::attr` takes something comparable to a `LocalName`, which the
/// `local_name!` macro supplies for the names html5ever interns. `aria-label`
/// and `aria-labelledby` are not among them, so they are matched by string.
fn attr<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attrs()
        .iter()
        .find(|attribute| attribute.name.local.as_ref() == name)
        .map(|attribute| attribute.value.as_ref())
}

/// The role an element has by virtue of being that element, per
/// <https://www.w3.org/TR/html-aam-1.0/>.
///
/// # Why this is public
///
/// There were two of these. This one built the AccessKit tree a screen reader
/// reads; a second copy in `tauri-runtime-blitz`'s `agent.rs` built the
/// semantic tree an agent and the QA harness read, and the two answered
/// differently about the same document. `<th>` was the case that showed it:
/// correct here as a column or row header, reported as a plain `cell` there,
/// so a check that wanted "the Version column" had nothing to ask for while
/// every header was spelled the same as the data beneath it.
///
/// Neither copy was wrong on purpose. They were written months apart against
/// the same specification, and nothing could compare them: this table was
/// private, and the other lived in a different repository. Exporting it is what
/// makes a single answer possible, and the control surface in
/// `blitz-control-protocol` now derives its own role names from this function
/// rather than from a table of its own.
///
/// # What it does not do
///
/// An author's explicit `role` attribute is not consulted. That is an override
/// applied on top of the implicit role, and the two consumers apply it
/// differently: the control surface passes the author's string through
/// verbatim, while this tree would need a full ARIA-name-to-[`Role`] table it
/// does not have. Naming it here would decide that question for both.
pub fn implicit_role(element: &ElementData) -> Role {
    let name = element.name.local.as_ref();
    match name {
        // Document structure
        "article" => Role::Article,
        "aside" => Role::Complementary,
        "footer" => Role::Footer,
        "header" => Role::Header,
        "main" => Role::Main,
        "nav" => Role::Navigation,
        "search" => Role::Search,
        // A named section is a landmark; an unnamed one is a wrapper.
        //
        // HTML-AAM maps `<section>` to `region` when it has an accessible name
        // and leaves it generic otherwise. Both halves matter. A named section
        // is how a page says "this part is the connection settings", and
        // without the rule it arrives indistinguishable from the `<div>`s
        // around it; promoting the unnamed ones would put a landmark around
        // every block on a page that reaches for `<section>` as a synonym for
        // `<div>`.
        //
        // Attributes only. The accessible name is not computed here, and
        // computing it would walk the section's whole subtree for every element
        // in the document. `aria-labelledby` is included so an author who names
        // a section that way still gets the landmark; resolving the reference
        // to the name itself is the caller's job.
        "section" => {
            if ["aria-label", "aria-labelledby", "title"]
                .iter()
                .any(|name| attr(element, name).is_some_and(|value| !value.trim().is_empty()))
            {
                Role::Region
            } else {
                Role::Section
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
        "p" => Role::Paragraph,
        "blockquote" => Role::Blockquote,
        "figure" => Role::Figure,
        "figcaption" | "caption" => Role::Caption,
        "hr" => Role::Splitter,

        // Grouping
        "ul" | "ol" | "menu" => Role::List,
        "li" => Role::ListItem,
        "dl" => Role::DescriptionList,
        "dt" => Role::Term,
        "dd" => Role::Definition,
        "dialog" => Role::Dialog,
        "fieldset" => Role::Group,
        "form" => Role::Form,
        "div" => Role::GenericContainer,

        // Tables
        "table" => Role::Table,
        "thead" | "tbody" | "tfoot" => Role::RowGroup,
        "tr" => Role::Row,
        "td" => Role::Cell,
        // A header cell is not a cell. `scope` decides which kind; without one
        // this is a column header, which is the `<thead>` row case.
        "th" => match element.attr(local_name!("scope")) {
            Some("row") | Some("rowgroup") => Role::RowHeader,
            _ => Role::ColumnHeader,
        },

        // Interactive
        // An <a> is only a link when it has an href.
        "a" => match element.attr(local_name!("href")) {
            Some(_) => Role::Link,
            None => Role::GenericContainer,
        },
        "button" => Role::Button,
        "label" => Role::Label,
        "legend" => Role::Label,
        "select" => match element.attr(local_name!("multiple")) {
            Some(_) => Role::ListBox,
            None => Role::ComboBox,
        },
        "option" => Role::ListBoxOption,
        "textarea" => Role::MultilineTextInput,
        "progress" => Role::ProgressIndicator,
        "meter" => Role::Meter,
        "output" => Role::Status,
        "summary" => Role::DisclosureTriangle,

        // Inline semantics
        "code" => Role::Code,
        "em" => Role::Emphasis,
        "strong" => Role::Strong,
        "mark" => Role::Mark,
        "time" => Role::Time,
        "img" => Role::Image,
        "iframe" => Role::Iframe,

        "input" => {
            let ty = element.attr(local_name!("type")).unwrap_or("text");
            match ty {
                "button" | "submit" | "reset" => Role::Button,
                "checkbox" => Role::CheckBox,
                "color" => Role::ColorWell,
                "date" => Role::DateInput,
                "datetime-local" => Role::DateTimeInput,
                "email" => Role::EmailInput,
                "number" => Role::NumberInput,
                "password" => Role::PasswordInput,
                "radio" => Role::RadioButton,
                "range" => Role::Slider,
                "search" => Role::SearchInput,
                "tel" => Role::PhoneNumberInput,
                "time" => Role::TimeInput,
                _ => Role::TextInput,
            }
        }
        _ => Role::Unknown,
    }
}

impl BaseDocument {
    pub fn build_accessibility_tree(&self) -> TreeUpdate {
        let mut nodes = std::collections::HashMap::new();
        let mut window = AccessKitNode::new(Role::Window);

        self.visit(|node_id, node| {
            let parent = node
                .parent
                .and_then(|parent_id| nodes.get_mut(&parent_id))
                .map(|(_, parent)| parent)
                .unwrap_or(&mut window);
            let (id, builder) = self.build_accessibility_node(node, parent);

            nodes.insert(node_id, (id, builder));
        });

        let mut nodes: Vec<_> = nodes
            .into_iter()
            .map(|(_, (id, node))| (id, node))
            .collect();
        nodes.push((NodeId(u64::MAX), window));

        let tree = Tree::new(NodeId(u64::MAX));
        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(tree),
            focus: NodeId(self.focus_node_id.map(|id| id.as_u64()).unwrap_or(u64::MAX)),
        }
    }

    fn build_accessibility_node(
        &self,
        node: &BlitzDomNode,
        parent: &mut AccessKitNode,
    ) -> (NodeId, AccessKitNode) {
        let id = NodeId(node.id.as_u64());

        let mut builder = AccessKitNode::default();
        if node.parent.is_none() {
            builder.set_role(Role::Window)
        } else if let Some(element_data) = node.element_data() {
            builder.set_role(implicit_role(element_data));
            // Roles alone do not expose a picker's current value or its options.
            // Read live selectedness so keyboard and script changes reach the tree.
            match element_data.name.local.as_ref() {
                "select" => builder.set_value(self.select_label(node.id)),
                "option" => {
                    builder.set_label(self.option_label(node.id));
                    builder.set_selected(self.option_is_selected(node.id));
                }
                _ => {}
            }
            builder.set_html_tag(element_data.name.local.to_string());
        } else if node.is_text_node() {
            builder.set_role(Role::TextRun);
            builder.set_value(node.text_content());
            parent.push_labelled_by(id)
        }

        parent.push_child(id);

        (id, builder)
    }
}

#[cfg(test)]
mod tests {
    use markup5ever::{QualName, ns};

    use crate::node::Attribute;

    use super::*;

    /// One element, built directly.
    ///
    /// `blitz-dom` holds the tree but does not parse HTML, so a fixture here
    /// cannot be a string of markup. The role rules read a tag name and a
    /// handful of attributes and nothing else, which is exactly what this
    /// supplies.
    fn element(tag: &str, attributes: &[(&str, &str)]) -> ElementData {
        ElementData::new(
            QualName::new(None, ns!(html), tag.into()),
            attributes
                .iter()
                .map(|(name, value)| Attribute {
                    name: QualName::new(None, ns!(), (*name).into()),
                    value: (*value).into(),
                })
                .collect(),
        )
    }

    fn role_of(tag: &str, attributes: &[(&str, &str)]) -> Role {
        implicit_role(&element(tag, attributes))
    }

    /// The case that showed there were two tables.
    ///
    /// A header cell reported as a plain cell is not a cosmetic difference: it
    /// is the difference between a document that says which column a value
    /// belongs to and one that does not.
    /// The case that showed there were two tables.
    ///
    /// A header cell reported as a plain cell is not a cosmetic difference: it
    /// is the difference between a document that says which column a value
    /// belongs to and one that does not.
    #[test]
    fn a_header_cell_is_a_header_and_scope_says_which_kind() {
        assert_eq!(role_of("th", &[]), Role::ColumnHeader);
        assert_eq!(role_of("th", &[("scope", "col")]), Role::ColumnHeader);
        assert_eq!(role_of("th", &[("scope", "row")]), Role::RowHeader);
        assert_eq!(role_of("th", &[("scope", "rowgroup")]), Role::RowHeader);
        assert_eq!(role_of("td", &[]), Role::Cell);
    }

    #[test]
    fn a_named_section_is_a_landmark_and_an_unnamed_one_is_not() {
        assert_eq!(
            role_of("section", &[("aria-label", "connection settings")]),
            Role::Region
        );
        assert_eq!(
            role_of("section", &[("aria-labelledby", "heading")]),
            Role::Region
        );
        assert_eq!(role_of("section", &[("title", "notes")]), Role::Region);
        assert_eq!(role_of("section", &[]), Role::Section);
        assert_eq!(
            role_of("section", &[("aria-label", "  ")]),
            Role::Section,
            "whitespace is not a name"
        );
    }

    #[test]
    fn an_anchor_is_a_link_only_when_it_goes_somewhere() {
        assert_eq!(role_of("a", &[("href", "/x")]), Role::Link);
        assert_eq!(role_of("a", &[]), Role::GenericContainer);
    }

    #[test]
    fn an_input_takes_its_role_from_its_type() {
        assert_eq!(role_of("input", &[]), Role::TextInput);
        assert_eq!(role_of("input", &[("type", "checkbox")]), Role::CheckBox);
        assert_eq!(role_of("input", &[("type", "radio")]), Role::RadioButton);
        assert_eq!(role_of("input", &[("type", "range")]), Role::Slider);
        assert_eq!(role_of("input", &[("type", "submit")]), Role::Button);
        assert_eq!(
            role_of("input", &[("type", "password")]),
            Role::PasswordInput
        );
        assert_eq!(
            role_of("input", &[("type", "not-a-type")]),
            Role::TextInput,
            "an unknown input type still edits text"
        );
    }

    #[test]
    fn a_multiple_select_is_a_list_box() {
        assert_eq!(role_of("select", &[]), Role::ComboBox);
        assert_eq!(role_of("select", &[("multiple", "")]), Role::ListBox);
    }
}

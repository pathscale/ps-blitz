//! A `<select>` reports what it offers and what is chosen.
//!
//! The options were already in the accessibility tree, because the traversal
//! walks raw children and `option { display: none }` therefore does not hide
//! them, but they carried a role and nothing else. No label, no selectedness,
//! and no value on the select itself, so a harness could see that a combo box
//! existed and learn nothing further about it. worktables.dev's schema designer
//! could not be driven at all for exactly this reason.

use accesskit::{Node as AccessKitNode, NodeId, Role};
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::collections::HashMap;
use std::sync::Arc;

const PICKER: &str = r#"<html><body style="margin:0">
    <select id="country">
        <option id="us" value="us">United States</option>
        <option id="fr" value="fr" selected>France</option>
        <option id="jp" value="jp">Japan</option>
    </select>
</body></html>"#;

/// The accessibility node of each element named by `ids`, in the order asked
/// for.
///
/// Looked up by element id rather than read off the tree in order, because
/// `build_accessibility_tree` collects through a `HashMap` and the nodes come
/// back in whatever order that iterates.
fn nodes_for(html: &str, ids: &[&str]) -> Vec<AccessKitNode> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let by_id: HashMap<NodeId, AccessKitNode> =
        doc.build_accessibility_tree().nodes.into_iter().collect();

    ids.iter()
        .map(|id| {
            let node_id = doc
                .get_element_by_id(id)
                .unwrap_or_else(|| panic!("no element with id {id}"));
            by_id
                .get(&NodeId(node_id.as_u64()))
                .unwrap_or_else(|| panic!("{id} is not in the accessibility tree"))
                .clone()
        })
        .collect()
}

fn labels_and_selectedness(html: &str, ids: &[&str]) -> Vec<(String, Option<bool>)> {
    nodes_for(html, ids)
        .into_iter()
        .map(|node| {
            assert_eq!(node.role(), Role::ListBoxOption);
            (
                node.label().unwrap_or_default().to_string(),
                node.is_selected(),
            )
        })
        .collect()
}

#[test]
fn options_reach_the_semantic_tree_with_their_labels() {
    let labels: Vec<String> = labels_and_selectedness(PICKER, &["us", "fr", "jp"])
        .into_iter()
        .map(|(label, _)| label)
        .collect();

    assert_eq!(
        labels,
        vec![
            "United States".to_string(),
            "France".to_string(),
            "Japan".to_string()
        ],
        "an option's accessible name must be its label, or a harness cannot tell what the select offers"
    );
}

#[test]
fn an_options_label_attribute_wins_over_its_text() {
    let labels: Vec<String> = labels_and_selectedness(
        r#"<html><body>
            <select id="s"><option id="one" value="1" label="One">ignored</option></select>
        </body></html>"#,
        &["one"],
    )
    .into_iter()
    .map(|(label, _)| label)
    .collect();

    assert_eq!(labels, vec!["One".to_string()]);
}

#[test]
fn the_selected_option_is_marked_selected() {
    assert_eq!(
        labels_and_selectedness(PICKER, &["us", "fr", "jp"])
            .into_iter()
            .map(|(_, selected)| selected)
            .collect::<Vec<_>>(),
        vec![Some(false), Some(true), Some(false)],
        "the `selected` content attribute must seed selectedness, and the others must read as not selected"
    );
}

#[test]
fn a_select_with_no_selected_attribute_falls_back_to_its_first_option() {
    assert_eq!(
        labels_and_selectedness(
            r#"<html><body>
                <select id="s">
                    <option id="a" value="a">Alpha</option>
                    <option id="b" value="b">Beta</option>
                </select>
            </body></html>"#,
            &["a", "b"],
        )
        .into_iter()
        .map(|(_, selected)| selected)
        .collect::<Vec<_>>(),
        vec![Some(true), Some(false)],
        "a drop-down always displays something, so it must select its first option when nothing else does"
    );
}

#[test]
fn a_select_reports_its_current_label_as_its_value() {
    let select = nodes_for(PICKER, &["country"]).remove(0);
    assert_eq!(select.role(), Role::ComboBox);
    assert_eq!(
        select.value().map(str::to_string),
        Some("France".to_string()),
        "the combo box's value is what it displays, which is the selected option's label"
    );
}

#[test]
fn options_inside_an_optgroup_are_still_reached() {
    assert_eq!(
        labels_and_selectedness(
            r#"<html><body>
                <select id="s">
                    <optgroup label="Europe">
                        <option id="fr" value="fr">France</option>
                        <option id="de" value="de">Germany</option>
                    </optgroup>
                </select>
            </body></html>"#,
            &["fr", "de"],
        ),
        vec![
            ("France".to_string(), Some(true)),
            ("Germany".to_string(), Some(false))
        ],
        "an <optgroup> nests its options one level down, and the flattened order is what selectedIndex counts"
    );
}

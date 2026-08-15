//! A template using every node variant survives a round trip.
//!
//! The point is not that serde works. It is that every variant is *reachable*
//! through the encoding: a variant that cannot be written, or that comes back
//! as something else, is a hole in the format that nothing else here would
//! find, because the compiler is perfectly happy with a variant nobody
//! constructs.
//!
//! RON is the encoding, not the format. This file is the only place in the
//! crate that looks at encoded text, and the two tests that do so say why.

use dom_abi::runtime::TemplatesSection;
use dom_abi::template::{
    Atom, AttrTarget, AttrValue, Attribute, Binding, BindingId, BindingKind, Component,
    ContentHash, Element, EventFlags, EventListener, For, Node, Prop, Show, StyleDecl,
    TEMPLATE_FORMAT_VERSION, Template,
};

fn hash(fill: &str) -> ContentHash {
    ContentHash::parse(format!("b3:{}", fill.repeat(32))).expect("a well-formed hash")
}

/// Every [`Node`] variant, every [`AttrValue`] variant, both [`AttrTarget`]
/// variants, every [`BindingKind`], a style declaration, and a listener with a
/// flag set.
fn every_variant() -> Template {
    Template {
        version: TEMPLATE_FORMAT_VERSION,
        hash: hash("ab"),
        name: "TabStrip".to_owned(),
        bindings: vec![
            Binding {
                id: BindingId(0),
                kind: BindingKind::Value,
                debug: "label".to_owned(),
            },
            Binding {
                id: BindingId(1),
                kind: BindingKind::Condition,
                debug: "selected".to_owned(),
            },
            Binding {
                id: BindingId(2),
                kind: BindingKind::List,
                debug: "tabs".to_owned(),
            },
            Binding {
                id: BindingId(3),
                kind: BindingKind::Handler,
                debug: "onClose".to_owned(),
            },
        ],
        roots: vec![Node::Element(Element {
            tag: Atom::from("div"),
            attrs: vec![
                Attribute {
                    target: AttrTarget::Class,
                    value: AttrValue::Variant {
                        base: Atom::from("tab"),
                        on: BindingId(1),
                        when_true: Atom::from("tab--selected"),
                        when_false: Atom::from("tab--idle"),
                    },
                },
                Attribute {
                    target: AttrTarget::Named(Atom::from("role")),
                    value: AttrValue::Static(Atom::from("tablist")),
                },
                Attribute {
                    target: AttrTarget::Named(Atom::from("aria-label")),
                    value: AttrValue::Bind(BindingId(0)),
                },
            ],
            style: vec![StyleDecl {
                property: Atom::from("display"),
                value: AttrValue::Static(Atom::from("flex")),
            }],
            events: vec![EventListener {
                event: Atom::from("click"),
                handler: BindingId(3),
                flags: EventFlags {
                    stop_propagation: true,
                },
            }],
            children: vec![
                Node::Literal("Tabs".to_owned()),
                Node::Children,
                Node::Show(Show {
                    when: BindingId(1),
                    then: vec![Node::Literal("selected".to_owned())],
                }),
                Node::For(For {
                    each: BindingId(2),
                    body: vec![Node::Component(Component {
                        of: hash("cd"),
                        debug: "Tab".to_owned(),
                        props: vec![Prop {
                            name: Atom::from("label"),
                            value: AttrValue::Bind(BindingId(0)),
                        }],
                        attrs: vec![Attribute {
                            target: AttrTarget::Class,
                            value: AttrValue::Static(Atom::from("tab-item")),
                        }],
                        children: vec![Node::Literal("×".to_owned())],
                    })],
                }),
            ],
        })],
    }
}

/// Every variant this crate has, so the test below fails when one is added
/// without being covered.
fn variants_used(template: &Template) -> Vec<&'static str> {
    fn walk(nodes: &[Node], seen: &mut Vec<&'static str>) {
        for node in nodes {
            match node {
                Node::Element(element) => {
                    seen.push("Node::Element");
                    for attr in &element.attrs {
                        match attr.target {
                            AttrTarget::Class => seen.push("AttrTarget::Class"),
                            AttrTarget::Named(_) => seen.push("AttrTarget::Named"),
                        }
                        match attr.value {
                            AttrValue::Static(_) => seen.push("AttrValue::Static"),
                            AttrValue::Bind(_) => seen.push("AttrValue::Bind"),
                            AttrValue::Variant { .. } => seen.push("AttrValue::Variant"),
                        }
                    }
                    if !element.style.is_empty() {
                        seen.push("StyleDecl");
                    }
                    if element.events.iter().any(|e| e.flags.stop_propagation) {
                        seen.push("EventFlags::stop_propagation");
                    }
                    walk(&element.children, seen);
                }
                Node::Literal(_) => seen.push("Node::Literal"),
                Node::Children => seen.push("Node::Children"),
                Node::Show(show) => {
                    seen.push("Node::Show");
                    walk(&show.then, seen);
                }
                Node::For(each) => {
                    seen.push("Node::For");
                    walk(&each.body, seen);
                }
                Node::Component(component) => {
                    seen.push("Node::Component");
                    if !component.props.is_empty() {
                        seen.push("Prop");
                    }
                    walk(&component.children, seen);
                }
            }
        }
    }

    let mut seen = Vec::new();
    for binding in &template.bindings {
        seen.push(match binding.kind {
            BindingKind::Value => "BindingKind::Value",
            BindingKind::Condition => "BindingKind::Condition",
            BindingKind::List => "BindingKind::List",
            BindingKind::Handler => "BindingKind::Handler",
        });
    }
    walk(&template.roots, &mut seen);
    seen.sort_unstable();
    seen.dedup();
    seen
}

#[test]
fn the_fixture_actually_uses_every_variant() {
    let used = variants_used(&every_variant());
    let expected = [
        "AttrTarget::Class",
        "AttrTarget::Named",
        "AttrValue::Bind",
        "AttrValue::Static",
        "AttrValue::Variant",
        "BindingKind::Condition",
        "BindingKind::Handler",
        "BindingKind::List",
        "BindingKind::Value",
        "EventFlags::stop_propagation",
        "Node::Children",
        "Node::Component",
        "Node::Element",
        "Node::For",
        "Node::Literal",
        "Node::Show",
        "Prop",
        "StyleDecl",
    ];
    assert_eq!(
        used, expected,
        "the round-trip fixture stopped being exhaustive"
    );
}

#[test]
fn every_variant_survives_a_round_trip() {
    let template = every_variant();

    let encoded = ron::ser::to_string(&template).expect("serializes");
    let decoded: Template = ron::from_str(&encoded).expect("deserializes");

    assert_eq!(decoded, template);
}

#[test]
fn a_templates_section_round_trips() {
    let section = TemplatesSection {
        templates: vec![every_variant()],
    };

    let encoded = ron::ser::to_string(&section).expect("serializes");
    let decoded: TemplatesSection = ron::from_str(&encoded).expect("deserializes");

    assert_eq!(decoded, section);
}

/// Field order is load-bearing — it is the canonical traversal the content hash
/// is taken over, and `version` is first so that a reader hitting an unexpected
/// shape can say which format it got. serde emits fields in declaration order,
/// so inspecting the text is the way to check the declaration.
#[test]
fn version_is_the_first_field_written() {
    let encoded = ron::ser::to_string(&every_variant()).expect("serializes");

    let version_at = encoded.find("version").expect("a version field");
    let hash_at = encoded.find("hash").expect("a hash field");
    assert!(
        version_at < hash_at,
        "version is not written first:\n{encoded}"
    );
    assert!(
        version_at < 4,
        "version is not the first thing in the document:\n{encoded}"
    );
}

/// Rule 3, the half that is easy to assert.
#[test]
fn an_omitted_collection_defaults_to_empty() {
    let minimal = format!(
        "(version: {TEMPLATE_FORMAT_VERSION}, hash: \"b3:{}\", name: \"Empty\")",
        "00".repeat(32)
    );

    let decoded: Template =
        ron::from_str(&minimal).expect("a template with no bindings and no roots");
    assert!(decoded.bindings.is_empty());
    assert!(decoded.roots.is_empty());
}

/// A malformed hash is rejected at decode time rather than becoming a lookup
/// that never matches anything.
#[test]
fn a_template_carrying_a_bad_hash_does_not_decode() {
    let bad = format!("(version: {TEMPLATE_FORMAT_VERSION}, hash: \"sha256:abc\", name: \"Bad\")");
    assert!(ron::from_str::<Template>(&bad).is_err());
}

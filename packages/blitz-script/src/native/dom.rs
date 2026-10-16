//! DOM bindings over the live document borrowed by each guest operation.
//!
//! This module keeps document borrows inside one call. Guest-facing node values
//! are interned by `Guest::node`, so equality and repeated queries preserve
//! identity without storing a pointer to the document.

use super::*;
use blitz_dom::{BaseDocument, NodeData, NodeId};

const MAX_TREE_VISIT: usize = 100_000;
const MAX_MARKUP_INPUT: usize = 4 * 1024 * 1024;

fn type_error(message: impl Into<String>) -> Error {
    Error::type_error(message)
}

fn dom_error(error: impl std::fmt::Debug) -> Error {
    Error::new("DOMException", format!("{error:?}"))
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn string_arg(args: &[Value], index: usize) -> String {
    arg(args, index).text()
}

fn bool_arg(args: &[Value], index: usize, default: bool) -> bool {
    args.get(index).map(Value::truthy).unwrap_or(default)
}

fn node_arg(value: &Value) -> Result<NodeId> {
    value
        .node_id()
        .ok_or_else(|| type_error("The value is not a DOM node"))
}

fn opt_node(this: &mut Guest<impl RuntimeBridge>, doc: &BaseDocument, id: Option<NodeId>) -> Value {
    id.map(|id| this.typed_node(doc, id)).unwrap_or(Value::Null)
}

fn node_kind(doc: &BaseDocument, id: NodeId) -> (String, u8) {
    match doc.get_node(id).map(|node| &node.data) {
        Some(NodeData::Document(_)) => ("#document".into(), 9),
        Some(NodeData::Element(data)) => (data.name.local.to_string(), 1),
        Some(NodeData::AnonymousBlock(data)) => (data.name.local.to_string(), 1),
        Some(NodeData::Text(_)) => ("#text".into(), 3),
        Some(NodeData::Comment { .. }) => ("#comment".into(), 8),
        Some(NodeData::DocumentFragment) => ("#document-fragment".into(), 11),
        Some(NodeData::ShadowRoot(_)) => ("#shadow-root".into(), 11),
        None => (String::new(), 0),
    }
}

fn node_list(
    this: &mut Guest<impl RuntimeBridge>,
    doc: &BaseDocument,
    ids: impl IntoIterator<Item = NodeId>,
) -> Value {
    let values: Vec<Value> = ids.into_iter().map(|id| this.typed_node(doc, id)).collect();
    let list = Value::object("NodeList");
    if let Some(object) = list.as_object() {
        let mut object = object.borrow_mut();
        object.prototype = Some(this.global.get("NodeList").get("prototype"));
        object.props.insert(
            "length".into(),
            Property::data(Value::number(values.len() as f64)),
        );
        object.indexed = values;
    }
    list
}

fn element_class(doc: &BaseDocument, id: NodeId) -> Option<String> {
    let element = doc.get_node(id)?.element_data()?;
    if element.name.ns.to_string() != "http://www.w3.org/1999/xhtml" {
        return Some("SVGElement".into());
    }
    Some(
        match element.name.local.as_ref() {
            "img" => "HTMLImageElement",
            "audio" => "HTMLAudioElement",
            "video" => "HTMLVideoElement",
            "template" => "HTMLTemplateElement",
            "svg" | "path" | "g" | "circle" | "rect" | "line" | "polygon" | "polyline"
            | "ellipse" | "text" => "SVGElement",
            _ => "HTMLElement",
        }
        .into(),
    )
}

fn elements_by_tag(doc: &BaseDocument, root: NodeId, tag: &str) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let wanted = tag.to_ascii_lowercase();
    let mut visited = 0usize;
    while let Some(id) = stack.pop() {
        if visited >= MAX_TREE_VISIT {
            break;
        }
        visited += 1;
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        if let Some(element) = node.element_data() {
            if tag == "*" || element.name.local.as_ref() == wanted {
                out.push(id);
            }
        }
        stack.extend(node.children.iter().rev().copied());
    }
    out
}

fn scoped_query(doc: &BaseDocument, root: NodeId, selector: &str) -> Result<Vec<NodeId>> {
    let matches = doc.query_selector_all(selector).map_err(dom_error)?;
    let mut out = Vec::new();
    for candidate in matches {
        if candidate != root
            && blitz_dom_api::node::contains(doc, root, candidate).map_err(dom_error)?
        {
            out.push(candidate);
        }
    }
    Ok(out)
}

impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn install_dom(&mut self) {
        let document = Value::object("Document");
        document.put("nodeType", Value::number(9));
        self.global.put("document", document);

        for name in [
            "Node",
            "Element",
            "HTMLElement",
            "Document",
            "CharacterData",
            "Text",
            "Comment",
            "NodeList",
            "HTMLCollection",
            "DocumentFragment",
            "ShadowRoot",
            "HTMLTemplateElement",
            "SVGElement",
            "HTMLImageElement",
            "HTMLMediaElement",
            "HTMLAudioElement",
            "HTMLVideoElement",
            "EventTarget",
        ] {
            let constructor = Value::native(format!("DOM.Constructor.{name}"));
            constructor.put("name", Value::string(name));
            constructor.put("prototype", Value::object(format!("{name}Prototype")));
            self.global.put(name, constructor);
        }
        for (name, parent) in [
            ("Node", "EventTarget"),
            ("Element", "Node"),
            ("Document", "Node"),
            ("CharacterData", "Node"),
            ("HTMLElement", "Element"),
            ("SVGElement", "Element"),
            ("Text", "CharacterData"),
            ("Comment", "CharacterData"),
            ("DocumentFragment", "Node"),
            ("ShadowRoot", "DocumentFragment"),
            ("HTMLTemplateElement", "HTMLElement"),
            ("HTMLImageElement", "HTMLElement"),
            ("HTMLMediaElement", "HTMLElement"),
            ("HTMLAudioElement", "HTMLMediaElement"),
            ("HTMLVideoElement", "HTMLMediaElement"),
        ] {
            let prototype = self.global.get(name).get("prototype");
            prototype.as_object().unwrap().borrow_mut().prototype =
                Some(self.global.get(parent).get("prototype"));
        }
        for name in ["Image", "Audio"] {
            let constructor = Value::native(format!("DOM.Constructor.{name}"));
            constructor.put("name", Value::string(name));
            constructor.put(
                "prototype",
                self.global
                    .get(if name == "Image" {
                        "HTMLImageElement"
                    } else {
                        "HTMLAudioElement"
                    })
                    .get("prototype"),
            );
            self.global.put(name, constructor);
        }
        if let Some(document) = self.global.get("document").as_object() {
            document.borrow_mut().prototype = Some(self.global.get("Document").get("prototype"));
        }

        let filter = Value::object("NodeFilter");
        for (name, value) in [
            ("FILTER_ACCEPT", 1),
            ("FILTER_REJECT", 2),
            ("FILTER_SKIP", 3),
            ("SHOW_ALL", 4294967295i64),
            ("SHOW_ELEMENT", 1),
            ("SHOW_ATTRIBUTE", 2),
            ("SHOW_TEXT", 4),
            ("SHOW_CDATA_SECTION", 8),
            ("SHOW_ENTITY_REFERENCE", 16),
            ("SHOW_ENTITY", 32),
            ("SHOW_PROCESSING_INSTRUCTION", 64),
            ("SHOW_COMMENT", 128),
            ("SHOW_DOCUMENT", 256),
            ("SHOW_DOCUMENT_TYPE", 512),
            ("SHOW_DOCUMENT_FRAGMENT", 1024),
            ("SHOW_NOTATION", 2048),
        ] {
            filter.put(name, Value::number(value as f64));
        }
        if let Some(object) = filter.as_object() {
            object.borrow_mut().frozen = true;
        }
        self.global.put("NodeFilter", filter);
        let media = self.global.get("HTMLMediaElement");
        for (name, value) in [
            ("HAVE_NOTHING", 0.0),
            ("HAVE_METADATA", 1.0),
            ("HAVE_CURRENT_DATA", 2.0),
            ("HAVE_FUTURE_DATA", 3.0),
            ("HAVE_ENOUGH_DATA", 4.0),
        ] {
            media.put(name, Value::Number(value));
        }
    }

    /// Intern a live node and attach the prototype matching its actual node data.
    pub(super) fn typed_node(&mut self, doc: &BaseDocument, id: NodeId) -> Value {
        let value = self.node(id);
        let class = match doc.get_node(id).map(|node| &node.data) {
            Some(NodeData::Document(_)) => "Document".to_owned(),
            Some(NodeData::Element(_)) | Some(NodeData::AnonymousBlock(_)) => {
                element_class(doc, id).unwrap_or_else(|| "Element".into())
            }
            Some(NodeData::Text(_)) => "Text".into(),
            Some(NodeData::Comment { .. }) => "Comment".into(),
            Some(NodeData::DocumentFragment) => "DocumentFragment".into(),
            Some(NodeData::ShadowRoot(_)) => "ShadowRoot".into(),
            None => "Node".into(),
        };
        if let Some(object) = value.as_object() {
            object.borrow_mut().prototype = Some(self.global.get(&class).get("prototype"));
        }
        value
    }

    pub(super) fn dom_get(
        &mut self,
        doc: &mut BaseDocument,
        receiver: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        let kind = receiver.kind();
        if kind == "NodeList" || kind == "NamedNodeMap" {
            if let Ok(index) = key.parse::<usize>() {
                return Ok(receiver.items().get(index).cloned());
            }
        }
        if kind == "Document" {
            let value = match key {
                "nodeType" => Some(Value::number(9)),
                "nodeName" => Some(Value::string("#document")),
                "documentElement" => Some(opt_node(
                    self,
                    doc,
                    blitz_dom_api::document::document_element(doc).map_err(dom_error)?,
                )),
                "body" => Some(opt_node(
                    self,
                    doc,
                    blitz_dom_api::document::body(doc).map_err(dom_error)?,
                )),
                "head" => Some(opt_node(
                    self,
                    doc,
                    blitz_dom_api::document::head(doc).map_err(dom_error)?,
                )),
                "readyState" => Some(Value::string(self.ready_state.clone())),
                "title" => Some(Value::string(
                    doc.query_selector("title")
                        .ok()
                        .flatten()
                        .and_then(|id| blitz_dom_api::node::text_content(doc, id).ok())
                        .unwrap_or_default(),
                )),
                "querySelector" => Some(Value::native("DOM.querySelector")),
                "querySelectorAll" => Some(Value::native("DOM.querySelectorAll")),
                "getElementById" => Some(Value::native("DOM.getElementById")),
                "getElementsByTagName" => Some(Value::native("DOM.getElementsByTagName")),
                "getElementsByClassName" => Some(Value::native("DOM.getElementsByClassName")),
                "createEvent" => Some(Value::native("DOM.createEvent")),
                "createElement" => Some(Value::native("DOM.createElement")),
                "createElementNS" => Some(Value::native("DOM.createElementNS")),
                "createTextNode" => Some(Value::native("DOM.createTextNode")),
                "createComment" => Some(Value::native("DOM.createComment")),
                "createDocumentFragment" => Some(Value::native("DOM.createDocumentFragment")),
                "importNode" => Some(Value::native("DOM.importNode")),
                "adoptNode" => Some(Value::native("DOM.adoptNode")),
                "implementation" => Some(Value::object("DOMImplementation")),
                _ => None,
            };
            return Ok(value);
        }

        if kind == "DOMStyle" {
            let id = receiver
                .get("__owner")
                .node_id()
                .ok_or_else(|| type_error("Invalid style owner"))?;
            return Ok(match key {
                "cssText" => Some(Value::string(
                    blitz_dom_api::style::css_text(doc, id).map_err(dom_error)?,
                )),
                "getPropertyValue" => Some(Value::native("DOMStyle.getPropertyValue")),
                "setProperty" => Some(Value::native("DOMStyle.setProperty")),
                "removeProperty" => Some(Value::native("DOMStyle.removeProperty")),
                _ => None,
            });
        }

        if kind == "DOMTokenList" {
            let id = receiver
                .get("__owner")
                .node_id()
                .ok_or_else(|| type_error("Invalid classList owner"))?;
            let class = blitz_dom_api::element::get_attribute(doc, id, "class")
                .map_err(dom_error)?
                .unwrap_or_default();
            let tokens: Vec<&str> = class.split_ascii_whitespace().collect();
            return Ok(match key {
                "length" => Some(Value::number(tokens.len() as f64)),
                "value" => Some(Value::string(class)),
                "contains" => Some(Value::native("DOMTokenList.contains")),
                "add" => Some(Value::native("DOMTokenList.add")),
                "remove" => Some(Value::native("DOMTokenList.remove")),
                "toggle" => Some(Value::native("DOMTokenList.toggle")),
                "item" => Some(Value::native("DOMTokenList.item")),
                _ => None,
            });
        }

        let Some(id) = receiver.node_id() else {
            return Ok(None);
        };
        let (name, node_type) = node_kind(doc, id);
        let is_element = node_type == 1;
        let is_document = node_type == 9;
        let value = match key {
            "nodeType" => Some(Value::number(node_type as f64)),
            "nodeName" => Some(Value::string(if is_element {
                name.to_ascii_uppercase()
            } else {
                name.clone()
            })),
            "tagName" if is_element => Some(Value::string(name.to_ascii_uppercase())),
            "localName" if is_element => Some(Value::string(name.clone())),
            "parentNode" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::parent_node(doc, id).map_err(dom_error)?,
            )),
            "parentElement" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::parent_node(doc, id)
                    .map_err(dom_error)?
                    .filter(|parent| {
                        doc.get_node(*parent)
                            .and_then(|n| n.element_data())
                            .is_some()
                    }),
            )),
            "childNodes" => Some(node_list(
                self,
                doc,
                blitz_dom_api::node::child_nodes(doc, id).map_err(dom_error)?,
            )),
            "children" => Some(node_list(
                self,
                doc,
                blitz_dom_api::node::child_nodes(doc, id)
                    .map_err(dom_error)?
                    .into_iter()
                    .filter(|child| {
                        doc.get_node(*child)
                            .and_then(|n| n.element_data())
                            .is_some()
                    }),
            )),
            "firstChild" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::first_child(doc, id).map_err(dom_error)?,
            )),
            "lastChild" => Some(opt_node(
                self,
                doc,
                doc.get_node(id)
                    .and_then(|node| node.children.last().copied()),
            )),
            "firstElementChild" => Some(opt_node(
                self,
                doc,
                doc.get_node(id).and_then(|node| {
                    node.children.iter().copied().find(|child| {
                        doc.get_node(*child)
                            .and_then(|n| n.element_data())
                            .is_some()
                    })
                }),
            )),
            "lastElementChild" => Some(opt_node(
                self,
                doc,
                doc.get_node(id).and_then(|node| {
                    node.children.iter().rev().copied().find(|child| {
                        doc.get_node(*child)
                            .and_then(|n| n.element_data())
                            .is_some()
                    })
                }),
            )),
            "childElementCount" => Some(Value::number(
                doc.get_node(id)
                    .map(|node| {
                        node.children
                            .iter()
                            .filter(|child| {
                                doc.get_node(**child)
                                    .and_then(|n| n.element_data())
                                    .is_some()
                            })
                            .count()
                    })
                    .unwrap_or(0) as f64,
            )),
            "nextSibling" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::next_sibling(doc, id).map_err(dom_error)?,
            )),
            "previousSibling" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::previous_sibling(doc, id).map_err(dom_error)?,
            )),
            "nextElementSibling" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::next_sibling(doc, id)
                    .map_err(dom_error)?
                    .filter(|s| doc.get_node(*s).and_then(|n| n.element_data()).is_some()),
            )),
            "previousElementSibling" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::node::previous_sibling(doc, id)
                    .map_err(dom_error)?
                    .filter(|s| doc.get_node(*s).and_then(|n| n.element_data()).is_some()),
            )),
            "textContent" => Some(Value::string(
                blitz_dom_api::node::text_content(doc, id).map_err(dom_error)?,
            )),
            "nodeValue" => Some(match doc.get_node(id).map(|node| &node.data) {
                Some(NodeData::Text(_)) | Some(NodeData::Comment { .. }) => Value::string(
                    blitz_dom_api::character_data::data(doc, id)
                        .map_err(dom_error)?
                        .unwrap_or_default(),
                ),
                _ => Value::Null,
            }),
            "data" if node_type == 3 || node_type == 8 => Some(Value::string(
                blitz_dom_api::character_data::data(doc, id)
                    .map_err(dom_error)?
                    .unwrap_or_default(),
            )),
            "length" if node_type == 3 || node_type == 8 => Some(Value::number(
                blitz_dom_api::character_data::data(doc, id)
                    .map_err(dom_error)?
                    .unwrap_or_default()
                    .encode_utf16()
                    .count() as f64,
            )),
            "substringData" if node_type == 3 || node_type == 8 => {
                Some(Value::native("DOM.CharacterData.substringData"))
            }
            "appendData" if node_type == 3 || node_type == 8 => {
                Some(Value::native("DOM.CharacterData.appendData"))
            }
            "insertData" if node_type == 3 || node_type == 8 => {
                Some(Value::native("DOM.CharacterData.insertData"))
            }
            "deleteData" if node_type == 3 || node_type == 8 => {
                Some(Value::native("DOM.CharacterData.deleteData"))
            }
            "replaceData" if node_type == 3 || node_type == 8 => {
                Some(Value::native("DOM.CharacterData.replaceData"))
            }
            "host"
                if matches!(
                    doc.get_node(id).map(|node| &node.data),
                    Some(NodeData::ShadowRoot(_))
                ) =>
            {
                Some(opt_node(
                    self,
                    doc,
                    doc.get_node(id)
                        .and_then(|node| node.shadow_root_data())
                        .map(|root| root.host),
                ))
            }
            "mode"
                if matches!(
                    doc.get_node(id).map(|node| &node.data),
                    Some(NodeData::ShadowRoot(_))
                ) =>
            {
                Some(Value::string(
                    doc.get_node(id)
                        .and_then(|node| node.shadow_root_data())
                        .map(|root| {
                            if root.mode == blitz_dom::node::ShadowRootMode::Open {
                                "open"
                            } else {
                                "closed"
                            }
                        })
                        .unwrap_or("open"),
                ))
            }
            "innerHTML" if is_element => {
                let content = doc
                    .get_node(id)
                    .and_then(|node| node.element_data())
                    .and_then(|element| element.template_contents)
                    .unwrap_or(id);
                Some(Value::string(
                    blitz_dom_api::element::inner_html(doc, content).map_err(dom_error)?,
                ))
            }
            "outerHTML" if is_element => Some(Value::string(
                doc.get_node(id)
                    .map(|node| {
                        let mut html = String::new();
                        node.write_outer_html(&mut html);
                        html
                    })
                    .unwrap_or_default(),
            )),
            "id" if is_element => Some(Value::string(
                blitz_dom_api::element::get_attribute(doc, id, "id")
                    .map_err(dom_error)?
                    .unwrap_or_default(),
            )),
            "className" if is_element => Some(Value::string(
                blitz_dom_api::element::get_attribute(doc, id, "class")
                    .map_err(dom_error)?
                    .unwrap_or_default(),
            )),
            "classList" if is_element => {
                let owner = self.typed_node(doc, id);
                let existing = owner.get("__classList");
                let list = if existing.kind() == "DOMTokenList" {
                    existing
                } else {
                    let list = Value::object("DOMTokenList");
                    list.put_hidden("__owner", owner.clone());
                    owner.put_hidden("__classList", list.clone());
                    list
                };
                Some(list)
            }
            "style" if is_element => {
                let owner = self.typed_node(doc, id);
                let existing = owner.get("__style");
                let style = if existing.kind() == "DOMStyle" {
                    existing
                } else {
                    let style = Value::object("DOMStyle");
                    style.put_hidden("__owner", owner.clone());
                    owner.put_hidden("__style", style.clone());
                    style
                };
                Some(style)
            }
            "attributes" if is_element => Some(attributes_value(doc, id)),
            "hasAttributes" if is_element => Some(Value::native("DOM.Element.hasAttributes")),
            "content" if is_element && name.eq_ignore_ascii_case("template") => Some(opt_node(
                self,
                doc,
                doc.get_node(id)
                    .and_then(|node| node.element_data())
                    .and_then(|element| element.template_contents),
            )),
            "shadowRoot" if is_element => Some(opt_node(
                self,
                doc,
                doc.get_node(id)
                    .and_then(|node| node.shadow_root_id())
                    .filter(|root| {
                        doc.get_node(*root)
                            .and_then(|node| node.shadow_root_data())
                            .is_some_and(|root| root.mode == blitz_dom::node::ShadowRootMode::Open)
                    }),
            )),
            "value" | "name" | "type" | "href" | "src" | "title" | "role" | "placeholder"
                if is_element =>
            {
                Some(Value::string(
                    blitz_dom_api::element::get_attribute(doc, id, key)
                        .map_err(dom_error)?
                        .unwrap_or_default(),
                ))
            }
            "width" | "height" if is_element => Some(Value::Number(
                blitz_dom_api::element::get_attribute(doc, id, key)
                    .map_err(dom_error)?
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(if name.eq_ignore_ascii_case("canvas") {
                        if key == "width" { 300.0 } else { 150.0 }
                    } else {
                        0.0
                    }),
            )),
            "complete" if name.eq_ignore_ascii_case("img") => Some(Value::Bool(
                blitz_dom_api::element::raster_image(doc, id).is_some(),
            )),
            "checked" | "disabled" | "selected" | "multiple" | "hidden" if is_element => {
                Some(Value::Bool(
                    blitz_dom_api::element::has_attribute(doc, id, key).map_err(dom_error)?,
                ))
            }
            "hasChildNodes" => Some(Value::native("DOM.Node.hasChildNodes")),
            "contains" => Some(Value::native("DOM.Node.contains")),
            "appendChild" => Some(Value::native("DOM.Node.appendChild")),
            "insertBefore" => Some(Value::native("DOM.Node.insertBefore")),
            "removeChild" => Some(Value::native("DOM.Node.removeChild")),
            "replaceChild" => Some(Value::native("DOM.Node.replaceChild")),
            "cloneNode" => Some(Value::native("DOM.Node.cloneNode")),
            "getRootNode" => Some(Value::native("DOM.Node.getRootNode")),
            "compareDocumentPosition" => Some(Value::native("DOM.Node.compareDocumentPosition")),
            "getAttribute" if is_element => Some(Value::native("DOM.Element.getAttribute")),
            "setAttribute" if is_element => Some(Value::native("DOM.Element.setAttribute")),
            "removeAttribute" if is_element => Some(Value::native("DOM.Element.removeAttribute")),
            "hasAttribute" if is_element => Some(Value::native("DOM.Element.hasAttribute")),
            "toggleAttribute" if is_element => Some(Value::native("DOM.Element.toggleAttribute")),
            "querySelector" if is_element => Some(Value::native("DOM.Element.querySelector")),
            "querySelectorAll" if is_element => Some(Value::native("DOM.Element.querySelectorAll")),
            "getElementsByTagName" if is_element => {
                Some(Value::native("DOM.Element.getElementsByTagName"))
            }
            "getElementsByClassName" if is_element => {
                Some(Value::native("DOM.Element.getElementsByClassName"))
            }
            "attachShadow" if is_element => Some(Value::native("DOM.Element.attachShadow")),
            "matches" if is_element => Some(Value::native("DOM.Element.matches")),
            "closest" if is_element => Some(Value::native("DOM.Element.closest")),
            "insertAdjacentHTML" if is_element => {
                Some(Value::native("DOM.Element.insertAdjacentHTML"))
            }
            "remove" if is_element || matches!(node_type, 3 | 8) => {
                Some(Value::native("DOM.Element.remove"))
            }
            "before" if is_element => Some(Value::native("DOM.Element.before")),
            "after" if is_element => Some(Value::native("DOM.Element.after")),
            "replaceWith" if is_element => Some(Value::native("DOM.Element.replaceWith")),
            "item" if kind == "NodeList" || kind == "NamedNodeMap" => {
                Some(Value::native("DOM.NodeList.item"))
            }
            "length" if kind == "NodeList" || kind == "NamedNodeMap" => {
                Some(Value::number(receiver.items().len() as f64))
            }
            _ => None,
        };
        let _ = is_document;
        Ok(value)
    }

    pub(super) fn dom_set(
        &mut self,
        doc: &mut BaseDocument,
        receiver: &Value,
        key: &str,
        value: Value,
    ) -> Result<bool> {
        if receiver.kind() == "Document" {
            if key == "title" {
                if let Some(title) = doc.query_selector("title").ok().flatten() {
                    blitz_dom_api::node::set_text_content(doc, title, &value.text())
                        .map_err(dom_error)?;
                }
                return Ok(true);
            }
            return Ok(false);
        }
        if let Some(id) = receiver.node_id() {
            if (key == "nodeValue" || key == "data")
                && matches!(
                    doc.get_node(id).map(|node| &node.data),
                    Some(NodeData::Text(_)) | Some(NodeData::Comment { .. })
                )
            {
                blitz_dom_api::character_data::set_data(doc, id, &value.text())
                    .map_err(dom_error)?;
                return Ok(true);
            }
        }
        if receiver.kind() == "DOMStyle" {
            let Some(id) = receiver.get("__owner").node_id() else {
                return Ok(false);
            };
            if key == "cssText" {
                blitz_dom_api::style::set_css_text(doc, id, &value.text()).map_err(dom_error)?;
                return Ok(true);
            }
        }
        if receiver.kind() == "DOMTokenList" {
            let Some(id) = receiver.get("__owner").node_id() else {
                return Ok(false);
            };
            if key == "value" {
                blitz_dom_api::element::set_attribute(doc, id, "class", &value.text())
                    .map_err(dom_error)?;
                return Ok(true);
            }
        }
        let Some(id) = receiver.node_id() else {
            return Ok(false);
        };
        let Some(node) = doc.get_node(id) else {
            return Ok(false);
        };
        let is_element = node.element_data().is_some();
        if key == "textContent" {
            blitz_dom_api::node::set_text_content(doc, id, &value.text()).map_err(dom_error)?;
            return Ok(true);
        }
        if is_element {
            match key {
                "width" | "height" => {
                    let number = value.number_value();
                    if !number.is_finite() || !(0.0..=32768.0).contains(&number) {
                        return Err(Error::new(
                            "RangeError",
                            "element dimension exceeds the supported limit",
                        ));
                    }
                    blitz_dom_api::element::set_attribute(
                        doc,
                        id,
                        key,
                        &(number as u32).to_string(),
                    )
                    .map_err(dom_error)?;
                    return Ok(true);
                }
                "innerHTML" => {
                    let markup = value.text();
                    if markup.len() > MAX_MARKUP_INPUT {
                        return Err(type_error("Markup input exceeds the DOM limit"));
                    }
                    blitz_dom_api::element::set_inner_html(doc, id, &markup).map_err(dom_error)?;
                    return Ok(true);
                }
                "id" => {
                    blitz_dom_api::element::set_attribute(doc, id, "id", &value.text())
                        .map_err(dom_error)?;
                    return Ok(true);
                }
                "className" => {
                    blitz_dom_api::element::set_attribute(doc, id, "class", &value.text())
                        .map_err(dom_error)?;
                    return Ok(true);
                }
                "value" | "name" | "type" | "href" | "src" | "title" | "role" | "placeholder" => {
                    blitz_dom_api::element::set_attribute(doc, id, key, &value.text())
                        .map_err(dom_error)?;
                    return Ok(true);
                }
                "checked" | "disabled" | "selected" | "multiple" | "hidden" => {
                    if value.truthy() {
                        blitz_dom_api::element::set_attribute(doc, id, key, "")
                            .map_err(dom_error)?;
                    } else {
                        blitz_dom_api::element::remove_attribute(doc, id, key)
                            .map_err(dom_error)?;
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(false)
    }

    pub(super) fn dom_call(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Option<Value>> {
        if let Some(constructor) = name.strip_prefix("DOM.Constructor.") {
            return self
                .dom_construct(doc, constructor, args, construct)
                .map(Some);
        }
        if name == "DOM.createElement"
            || name == "DOM.createElementNS"
            || name == "DOM.createTextNode"
            || name == "DOM.createComment"
            || name == "DOM.createDocumentFragment"
            || name == "DOM.importNode"
            || name == "DOM.adoptNode"
        {
            let id = match name {
                "DOM.createElement" => {
                    blitz_dom_api::document::create_element(doc, &string_arg(&args, 0))
                        .map_err(dom_error)?
                }
                "DOM.createElementNS" => blitz_dom_api::document::create_element_ns(
                    doc,
                    &string_arg(&args, 0),
                    &string_arg(&args, 1),
                )
                .map_err(dom_error)?,
                "DOM.createTextNode" => {
                    blitz_dom_api::document::create_text_node(doc, &string_arg(&args, 0))
                        .map_err(dom_error)?
                }
                "DOM.createComment" => {
                    blitz_dom_api::document::create_comment(doc, &string_arg(&args, 0))
                        .map_err(dom_error)?
                }
                "DOM.createDocumentFragment" => doc.mutate().create_document_fragment(),
                "DOM.importNode" | "DOM.adoptNode" => blitz_dom_api::document::import_node(
                    doc,
                    node_arg(&arg(&args, 0))?,
                    bool_arg(&args, 1, false),
                )
                .map_err(dom_error)?,
                _ => return Err(type_error("Unknown document construction method")),
            };
            return Ok(Some(self.typed_node(doc, id)));
        }

        if name == "DOM.createEvent" {
            let interface = string_arg(&args, 0);
            if !matches!(
                interface.as_str(),
                "Event"
                    | "Events"
                    | "HTMLEvents"
                    | "MouseEvent"
                    | "UIEvent"
                    | "KeyboardEvent"
                    | "CustomEvent"
            ) {
                return Err(Error::new(
                    "NotSupportedError",
                    format!("unsupported event interface {interface}"),
                ));
            }
            let event = Value::object("Event");
            event.put("type", Value::string(""));
            event.put("bubbles", Value::Bool(false));
            event.put("cancelable", Value::Bool(false));
            event.put("composed", Value::Bool(false));
            event.put("defaultPrevented", Value::Bool(false));
            event.put("target", Value::Null);
            event.put("currentTarget", Value::Null);
            event.put("eventPhase", Value::number(0));
            event.put("timeStamp", Value::number(0));
            event.put_hidden("__interface", Value::string(interface));
            return Ok(Some(event));
        }

        if name == "DOM.querySelector" || name == "DOM.Element.querySelector" {
            let root = if name == "DOM.querySelector" {
                None
            } else {
                Some(node_arg(&this)?)
            };
            let selector = string_arg(&args, 0);
            let found = if let Some(root) = root {
                scoped_query(doc, root, &selector)?.into_iter().next()
            } else {
                blitz_dom_api::document::query_selector(doc, &selector).map_err(dom_error)?
            };
            return Ok(Some(opt_node(self, doc, found)));
        }
        if name == "DOM.querySelectorAll" || name == "DOM.Element.querySelectorAll" {
            let selector = string_arg(&args, 0);
            let ids = if name == "DOM.querySelectorAll" {
                blitz_dom_api::document::query_selector_all(doc, &selector).map_err(dom_error)?
            } else {
                scoped_query(doc, node_arg(&this)?, &selector)?
            };
            return Ok(Some(node_list(self, doc, ids)));
        }
        if name == "DOM.getElementById" {
            return Ok(Some(opt_node(
                self,
                doc,
                blitz_dom_api::document::get_element_by_id(doc, &string_arg(&args, 0))
                    .map_err(dom_error)?,
            )));
        }
        if name == "DOM.getElementsByTagName" || name == "DOM.Element.getElementsByTagName" {
            let root = if name == "DOM.getElementsByTagName" {
                doc.root_node().id
            } else {
                node_arg(&this)?
            };
            let mut ids = elements_by_tag(doc, root, &string_arg(&args, 0));
            if name == "DOM.Element.getElementsByTagName" {
                ids.retain(|candidate| *candidate != root);
            }
            return Ok(Some(node_list(self, doc, ids)));
        }
        if name == "DOM.getElementsByClassName" || name == "DOM.Element.getElementsByClassName" {
            let document_query = name == "DOM.getElementsByClassName";
            let root = if document_query {
                doc.root_node().id
            } else {
                node_arg(&this)?
            };
            let classes: Vec<String> = string_arg(&args, 0)
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect();
            if classes.is_empty() {
                return Ok(Some(node_list(self, doc, Vec::new())));
            }
            let mut found = Vec::new();
            for candidate in elements_by_tag(doc, root, "*") {
                if !document_query && candidate == root {
                    continue;
                }
                let value = blitz_dom_api::element::get_attribute(doc, candidate, "class")
                    .map_err(dom_error)?
                    .unwrap_or_default();
                if classes
                    .iter()
                    .all(|class| value.split_ascii_whitespace().any(|token| token == class))
                {
                    found.push(candidate);
                }
            }
            return Ok(Some(node_list(self, doc, found)));
        }

        let Some(id) = this.node_id() else {
            if name == "DOMStyle.getPropertyValue"
                || name == "DOMStyle.setProperty"
                || name == "DOMStyle.removeProperty"
            {
                let owner = this
                    .get("__owner")
                    .node_id()
                    .ok_or_else(|| type_error("Invalid style owner"))?;
                match name {
                    "DOMStyle.getPropertyValue" => {
                        return Ok(Some(Value::string(
                            blitz_dom_api::style::get_property_value(
                                doc,
                                owner,
                                &string_arg(&args, 0),
                            )
                            .map_err(dom_error)?,
                        )));
                    }
                    "DOMStyle.setProperty" => {
                        blitz_dom_api::style::set_property(
                            doc,
                            owner,
                            &string_arg(&args, 0),
                            &string_arg(&args, 1),
                        )
                        .map_err(dom_error)?;
                        return Ok(Some(Value::Undefined));
                    }
                    _ => {
                        return Ok(Some(Value::string(
                            blitz_dom_api::style::remove_property(
                                doc,
                                owner,
                                &string_arg(&args, 0),
                            )
                            .map_err(dom_error)?,
                        )));
                    }
                }
            }
            if name == "DOM.NodeList.item" {
                return Ok(Some(
                    this.items()
                        .get(arg(&args, 0).number_value() as usize)
                        .cloned()
                        .unwrap_or(Value::Null),
                ));
            }
            if name.starts_with("DOMTokenList.") {
                let owner = this
                    .get("__owner")
                    .node_id()
                    .ok_or_else(|| type_error("Invalid classList owner"))?;
                let current = blitz_dom_api::element::get_attribute(doc, owner, "class")
                    .map_err(dom_error)?
                    .unwrap_or_default();
                let tokens: Vec<String> = current
                    .split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect();
                match name {
                    "DOMTokenList.contains" => {
                        return Ok(Some(Value::Bool(
                            tokens.iter().any(|token| token == &string_arg(&args, 0)),
                        )));
                    }
                    "DOMTokenList.add" | "DOMTokenList.remove" => {
                        let requested: Vec<String> = args.iter().map(Value::text).collect();
                        let refs: Vec<&str> = requested.iter().map(String::as_str).collect();
                        if name == "DOMTokenList.add" {
                            blitz_dom_api::element::class_list_add(doc, owner, &refs)
                                .map_err(dom_error)?;
                        } else {
                            blitz_dom_api::element::class_list_remove(doc, owner, &refs)
                                .map_err(dom_error)?;
                        }
                        return Ok(Some(Value::Undefined));
                    }
                    "DOMTokenList.toggle" => {
                        let force = args.get(1).map(Value::truthy);
                        return Ok(Some(Value::Bool(
                            blitz_dom_api::element::class_list_toggle(
                                doc,
                                owner,
                                &string_arg(&args, 0),
                                force,
                            )
                            .map_err(dom_error)?,
                        )));
                    }
                    "DOMTokenList.item" => {
                        return Ok(Some(
                            tokens
                                .get(arg(&args, 0).number_value() as usize)
                                .map(Value::string)
                                .unwrap_or(Value::Null),
                        ));
                    }
                    _ => {}
                }
            }
            return Ok(None);
        };

        let result = match name {
            "DOM.CharacterData.substringData"
            | "DOM.CharacterData.appendData"
            | "DOM.CharacterData.insertData"
            | "DOM.CharacterData.deleteData"
            | "DOM.CharacterData.replaceData" => {
                let mut units = blitz_dom_api::character_data::data(doc, id)
                    .map_err(dom_error)?
                    .unwrap_or_default()
                    .encode_utf16()
                    .collect::<Vec<_>>();
                let offset = if name == "DOM.CharacterData.appendData" {
                    units.len()
                } else {
                    arg(&args, 0).number_value().max(0.0) as usize
                };
                let count = arg(&args, 1).number_value().max(0.0) as usize;
                if offset > units.len() {
                    return Err(Error::new(
                        "IndexSizeError",
                        "CharacterData offset exceeds its length",
                    ));
                }
                let end = offset.saturating_add(count).min(units.len());
                match name {
                    "DOM.CharacterData.substringData" => {
                        return Ok(Some(Value::string(String::from_utf16_lossy(
                            &units[offset..end],
                        ))));
                    }
                    "DOM.CharacterData.appendData" => {
                        units.extend(arg(&args, 0).text().encode_utf16())
                    }
                    "DOM.CharacterData.insertData" => {
                        units.splice(offset..offset, arg(&args, 1).text().encode_utf16());
                    }
                    "DOM.CharacterData.deleteData" => {
                        units.drain(offset..end);
                    }
                    _ => {
                        units.splice(offset..end, arg(&args, 2).text().encode_utf16());
                    }
                }
                blitz_dom_api::character_data::set_data(doc, id, &String::from_utf16_lossy(&units))
                    .map_err(dom_error)?;
                return Ok(Some(Value::Undefined));
            }
            "DOM.Node.hasChildNodes" => Some(Value::Bool(
                blitz_dom_api::node::has_child_nodes(doc, id).map_err(dom_error)?,
            )),
            "DOM.Node.contains" => Some(Value::Bool(
                blitz_dom_api::node::contains(doc, id, node_arg(&arg(&args, 0))?)
                    .map_err(dom_error)?,
            )),
            "DOM.Node.appendChild" => {
                let child = node_arg(&arg(&args, 0))?;
                let inserted =
                    blitz_dom_api::node::append_child(doc, id, child).map_err(dom_error)?;
                Some(self.typed_node(doc, inserted))
            }
            "DOM.Node.insertBefore" => {
                let child = node_arg(&arg(&args, 0))?;
                let reference = if args
                    .get(1)
                    .is_none_or(|value| matches!(value, Value::Null | Value::Undefined))
                {
                    None
                } else {
                    Some(node_arg(&arg(&args, 1))?)
                };
                let inserted = blitz_dom_api::node::insert_before(doc, id, child, reference)
                    .map_err(dom_error)?;
                Some(self.typed_node(doc, inserted))
            }
            "DOM.Node.removeChild" => {
                let removed = blitz_dom_api::node::remove_child(doc, id, node_arg(&arg(&args, 0))?)
                    .map_err(dom_error)?;
                Some(self.typed_node(doc, removed))
            }
            "DOM.Node.replaceChild" => {
                let replaced = blitz_dom_api::node::replace_child(
                    doc,
                    id,
                    node_arg(&arg(&args, 0))?,
                    node_arg(&arg(&args, 1))?,
                )
                .map_err(dom_error)?;
                Some(self.typed_node(doc, replaced))
            }
            "DOM.Node.cloneNode" => {
                let cloned = blitz_dom_api::node::clone_node(doc, id, bool_arg(&args, 0, false))
                    .map_err(dom_error)?;
                Some(self.typed_node(doc, cloned))
            }
            "DOM.Node.getRootNode" => {
                let mut root = id;
                let mut remaining = MAX_TREE_VISIT;
                let composed = arg(&args, 0).get("composed").truthy();
                while remaining > 0 {
                    remaining -= 1;
                    if !composed && doc.get_node(root).is_some_and(|node| node.is_shadow_root()) {
                        break;
                    }
                    match doc.get_node(root).and_then(|node| node.parent) {
                        Some(parent) => root = parent,
                        None => break,
                    }
                }
                Some(self.typed_node(doc, root))
            }
            "DOM.Node.compareDocumentPosition" => Some(Value::number(
                blitz_dom_api::node::compare_document_position(doc, id, node_arg(&arg(&args, 0))?)
                    .map_err(dom_error)? as f64,
            )),
            "DOM.Element.getAttribute" => Some(
                blitz_dom_api::element::get_attribute(doc, id, &string_arg(&args, 0))
                    .map_err(dom_error)?
                    .map(Value::string)
                    .unwrap_or(Value::Null),
            ),
            "DOM.Element.setAttribute" => {
                blitz_dom_api::element::set_attribute(
                    doc,
                    id,
                    &string_arg(&args, 0),
                    &string_arg(&args, 1),
                )
                .map_err(dom_error)?;
                Some(Value::Undefined)
            }
            "DOM.Element.removeAttribute" => {
                blitz_dom_api::element::remove_attribute(doc, id, &string_arg(&args, 0))
                    .map_err(dom_error)?;
                Some(Value::Undefined)
            }
            "DOM.Element.hasAttribute" => Some(Value::Bool(
                blitz_dom_api::element::has_attribute(doc, id, &string_arg(&args, 0))
                    .map_err(dom_error)?,
            )),
            "DOM.Element.hasAttributes" => Some(Value::Bool(
                doc.get_node(id)
                    .and_then(|node| node.element_data())
                    .is_some_and(|element| !element.attrs().is_empty()),
            )),
            "DOM.Element.attachShadow" => {
                let mode_name = arg(&args, 0).get("mode").text();
                #[cfg(feature = "shadow-dom")]
                {
                    let mode = match mode_name.as_str() {
                        "open" => blitz_dom::node::ShadowRootMode::Open,
                        "closed" => blitz_dom::node::ShadowRootMode::Closed,
                        _ => {
                            return Err(type_error(
                                "attachShadow requires mode 'open' or 'closed'",
                            ));
                        }
                    };
                    let root = doc.attach_shadow(id, mode);
                    Some(self.typed_node(doc, root))
                }
                #[cfg(not(feature = "shadow-dom"))]
                {
                    let _ = mode_name;
                    return Err(Error::new("NotSupportedError", "Shadow DOM is disabled"));
                }
            }
            "DOM.Element.toggleAttribute" => {
                let name = string_arg(&args, 0);
                let old =
                    blitz_dom_api::element::has_attribute(doc, id, &name).map_err(dom_error)?;
                let present = args.get(1).map(Value::truthy).unwrap_or(!old);
                if present {
                    blitz_dom_api::element::set_attribute(doc, id, &name, "").map_err(dom_error)?;
                } else {
                    blitz_dom_api::element::remove_attribute(doc, id, &name).map_err(dom_error)?;
                }
                Some(Value::Bool(present))
            }
            "DOM.Element.matches" => Some(Value::Bool(
                blitz_dom_api::element::matches(doc, id, &string_arg(&args, 0))
                    .map_err(dom_error)?,
            )),
            "DOM.Element.closest" => Some(opt_node(
                self,
                doc,
                blitz_dom_api::element::closest(doc, id, &string_arg(&args, 0))
                    .map_err(dom_error)?,
            )),
            "DOM.Element.insertAdjacentHTML" => {
                let position = string_arg(&args, 0).to_ascii_lowercase();
                let markup = string_arg(&args, 1);
                if markup.len() > MAX_MARKUP_INPUT {
                    return Err(type_error("Markup input exceeds the DOM limit"));
                }
                let context =
                    blitz_dom_api::document::create_element(doc, "div").map_err(dom_error)?;
                blitz_dom_api::element::set_inner_html(doc, context, &markup).map_err(dom_error)?;
                let children = blitz_dom_api::node::child_nodes(doc, context).map_err(dom_error)?;
                match position.as_str() {
                    "beforeend" => {
                        for child in children {
                            blitz_dom_api::node::append_child(doc, id, child).map_err(dom_error)?;
                        }
                    }
                    "afterbegin" => {
                        let reference = doc
                            .get_node(id)
                            .and_then(|node| node.children.first().copied());
                        for child in children.into_iter().rev() {
                            blitz_dom_api::node::insert_before(doc, id, child, reference)
                                .map_err(dom_error)?;
                        }
                    }
                    "beforebegin" | "afterend" => {
                        if let Some(parent) = doc.get_node(id).and_then(|node| node.parent) {
                            let reference = if position == "beforebegin" {
                                Some(id)
                            } else {
                                blitz_dom_api::node::next_sibling(doc, id).map_err(dom_error)?
                            };
                            for child in children {
                                blitz_dom_api::node::insert_before(doc, parent, child, reference)
                                    .map_err(dom_error)?;
                            }
                        }
                    }
                    _ => {
                        return Err(type_error(
                            "insertAdjacentHTML position must be beforebegin, afterbegin, beforeend, or afterend",
                        ));
                    }
                }
                Some(Value::Undefined)
            }
            "DOM.Element.remove" => {
                if let Some(parent) = doc.get_node(id).and_then(|node| node.parent) {
                    blitz_dom_api::node::remove_child(doc, parent, id).map_err(dom_error)?;
                }
                Some(Value::Undefined)
            }
            "DOM.Element.before" | "DOM.Element.after" | "DOM.Element.replaceWith" => {
                let parent = doc.get_node(id).and_then(|node| node.parent);
                if let Some(parent) = parent {
                    let ref_id = if name == "DOM.Element.before" {
                        Some(id)
                    } else {
                        blitz_dom_api::node::next_sibling(doc, id).map_err(dom_error)?
                    };
                    for value in args {
                        let child = if let Some(child) = value.node_id() {
                            child
                        } else {
                            blitz_dom_api::document::create_text_node(doc, &value.text())
                                .map_err(dom_error)?
                        };
                        blitz_dom_api::node::insert_before(doc, parent, child, ref_id)
                            .map_err(dom_error)?;
                    }
                    if name == "DOM.Element.replaceWith" {
                        blitz_dom_api::node::remove_child(doc, parent, id).map_err(dom_error)?;
                    }
                }
                Some(Value::Undefined)
            }
            _ => None,
        };
        let _ = construct;
        Ok(result)
    }

    fn dom_construct(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Value> {
        if !construct && !matches!(name, "Image" | "Audio" | "EventTarget") {
            return Err(type_error(format!("{name} constructor requires new")));
        }
        match name {
            "Node" => Err(type_error("Illegal constructor")),
            "Text" => {
                let id = blitz_dom_api::document::create_text_node(doc, &string_arg(&args, 0))
                    .map_err(dom_error)?;
                Ok(self.typed_node(doc, id))
            }
            "Comment" => {
                let id = blitz_dom_api::document::create_comment(doc, &string_arg(&args, 0))
                    .map_err(dom_error)?;
                Ok(self.typed_node(doc, id))
            }
            "DocumentFragment" => {
                let id = doc.mutate().create_document_fragment();
                Ok(self.typed_node(doc, id))
            }
            "Element" | "HTMLElement" | "Document" | "SVGElement" => {
                Err(type_error("Illegal constructor"))
            }
            "Image" => {
                let id = blitz_dom_api::document::create_element(doc, "img").map_err(dom_error)?;
                if !matches!(arg(&args, 0), Value::Undefined) {
                    blitz_dom_api::element::set_attribute(doc, id, "width", &arg(&args, 0).text())
                        .map_err(dom_error)?;
                }
                if !matches!(arg(&args, 1), Value::Undefined) {
                    blitz_dom_api::element::set_attribute(doc, id, "height", &arg(&args, 1).text())
                        .map_err(dom_error)?;
                }
                Ok(self.typed_node(doc, id))
            }
            "Audio" => {
                let id =
                    blitz_dom_api::document::create_element(doc, "audio").map_err(dom_error)?;
                if !matches!(arg(&args, 0), Value::Undefined) {
                    blitz_dom_api::element::set_attribute(doc, id, "src", &arg(&args, 0).text())
                        .map_err(dom_error)?;
                }
                Ok(self.typed_node(doc, id))
            }
            "HTMLTemplateElement"
            | "HTMLImageElement"
            | "HTMLMediaElement"
            | "HTMLAudioElement"
            | "HTMLVideoElement"
            | "ShadowRoot" => Err(type_error("Illegal constructor")),
            "EventTarget" => {
                let target = Value::object("EventTarget");
                target.as_object().unwrap().borrow_mut().prototype =
                    Some(self.global.get("EventTarget").get("prototype"));
                Ok(target)
            }
            _ => Err(type_error(format!("Unknown DOM constructor {name}"))),
        }
    }
}

fn attributes_value(doc: &BaseDocument, id: NodeId) -> Value {
    let attrs = Value::object("NamedNodeMap");
    let mut indexed = Vec::new();
    if let Some(element) = doc.get_node(id).and_then(|node| node.element_data()) {
        for attribute in element.attrs() {
            let item = Value::object("Attr");
            item.put("name", Value::string(&attribute.name.local));
            item.put("value", Value::string(&attribute.value));
            item.put("nodeName", Value::string(&attribute.name.local));
            item.put("nodeValue", Value::string(&attribute.value));
            indexed.push(item);
        }
    }
    if let Some(object) = attrs.as_object() {
        let mut object = object.borrow_mut();
        object.props.insert(
            "length".into(),
            Property::data(Value::number(indexed.len() as f64)),
        );
        object.indexed = indexed;
    }
    attrs
}

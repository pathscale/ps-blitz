//! Attr and live NamedNodeMap bindings.

use std::sync::{Arc, RwLock};

use blitz_dom::node::{AttrAtom, AttributeNode};
use blitz_dom::{LocalName, Namespace, NodeId, QualName};
use boa_engine::object::builtins::{JsArray, JsProxyBuilder};
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace, js_string,
};

use super::{DomaState, dom_error, index_arg, install_iterator};
use crate::dom::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_wrapper, this_node_id,
    to_rust_string,
};
use crate::state::DomCtx;

const MAP_OWNER: &str = "__blitz_internal_named_node_map_owner__";
const MAP_CACHE: &str = "__blitz_internal_named_node_map__";

#[derive(Trace, Finalize, JsData)]
pub(crate) struct AttrRef {
    #[unsafe_ignore_trace]
    pub handle: Arc<RwLock<AttributeNode>>,
    pub owner: boa_gc::GcRefCell<Option<JsObject>>,
    document: boa_gc::GcRefCell<JsObject>,
}

#[derive(Trace, Finalize, JsData)]
struct MapRef {
    owner: JsObject,
}

pub(crate) fn install(
    attr: &JsObject,
    map: &JsObject,
    element: &JsObject,
    document: &JsObject,
    context: &mut Context,
) {
    define_accessor(attr, "name", Some(name), None, context);
    define_accessor(attr, "nodeName", Some(name), None, context);
    define_accessor(attr, "localName", Some(local_name), None, context);
    define_accessor(attr, "prefix", Some(prefix), None, context);
    define_accessor(attr, "namespaceURI", Some(namespace_uri), None, context);
    define_accessor(attr, "value", Some(value), Some(set_value), context);
    define_accessor(attr, "nodeValue", Some(value), Some(set_value), context);
    define_accessor(attr, "textContent", Some(value), Some(set_value), context);
    define_accessor(attr, "ownerElement", Some(owner_element), None, context);
    define_accessor(attr, "ownerDocument", Some(owner_document), None, context);
    define_accessor(attr, "specified", Some(specified), None, context);
    define_accessor(attr, "nodeType", Some(node_type), None, context);
    define_accessor(attr, "isConnected", Some(disconnected), None, context);
    for property in ["parentNode", "parentElement", "firstChild", "lastChild", "nextSibling", "previousSibling"] {
        define_accessor(attr, property, Some(null), None, context);
    }
    define_accessor(attr, "childNodes", Some(no_children), None, context);
    define_method(attr, "hasChildNodes", 0, disconnected, context);
    define_method(attr, "cloneNode", 1, clone_attr, context);
    define_method(attr, "contains", 1, contains, context);

    define_accessor(map, "length", Some(map_length), None, context);
    define_method(map, "item", 1, map_item, context);
    define_method(map, "getNamedItem", 1, map_get, context);
    define_method(map, "setNamedItem", 1, map_set, context);
    define_method(map, "removeNamedItem", 1, map_remove, context);
    define_method(map, "getNamedItemNS", 2, map_get_ns, context);
    define_method(map, "setNamedItemNS", 1, map_set, context);
    define_method(map, "removeNamedItemNS", 2, map_remove_ns, context);
    install_iterator(map, context);

    define_accessor(element, "attributes", Some(attributes), None, context);
    define_method(element, "getAttributeNames", 0, attribute_names, context);
    define_method(element, "getAttributeNode", 1, get_attribute_node, context);
    define_method(element, "setAttributeNode", 1, set_attribute_node, context);
    define_method(element, "getAttributeNodeNS", 2, get_attribute_node_ns, context);
    define_method(element, "setAttributeNodeNS", 1, set_attribute_node, context);
    define_method(element, "removeAttributeNode", 1, remove_attribute_node, context);
    define_method(element, "toggleAttribute", 1, toggle_attribute, context);
    define_method(element, "getAttributeNS", 2, get_attribute_ns, context);
    define_method(element, "setAttributeNS", 3, set_attribute_ns, context);
    define_method(element, "removeAttributeNS", 2, remove_attribute_ns, context);
    define_method(element, "hasAttributeNS", 2, has_attribute_ns, context);
    define_method(element, "hasAttributes", 0, has_attributes, context);
    define_method(element, "getAttribute", 1, get_attribute, context);
    define_method(element, "setAttribute", 2, set_attribute, context);
    define_method(element, "removeAttribute", 1, remove_attribute, context);
    define_method(element, "hasAttribute", 1, has_attribute, context);
    define_method(document, "createAttribute", 1, create_attribute, context);
    define_method(document, "createAttributeNS", 2, create_attribute_ns, context);
}

pub(crate) fn attr_object(value: &JsValue) -> Option<JsObject> {
    value
        .as_object()
        .filter(|object| object.downcast_ref::<AttrRef>().is_some())
}

fn require_attr(this: &JsValue) -> JsResult<JsObject> {
    attr_object(this).ok_or_else(|| JsNativeError::typ().with_message("receiver is not an Attr").into())
}

fn require_element(this: &JsValue, ctx: &DomCtx) -> JsResult<NodeId> {
    let id = this_node_id(this)?;
    if !ctx.doc.borrow().get_node(id).is_some_and(|node| node.is_element()) {
        return Err(JsNativeError::typ().with_message("receiver is not an Element").into());
    }
    Ok(id)
}

pub(crate) fn snapshot(object: &JsObject) -> (QualName, AttrAtom) {
    let data = object.downcast_ref::<AttrRef>().expect("checked Attr");
    let slot = data.handle.read().unwrap();
    (slot.name.clone(), slot.value.clone())
}

fn qualified(name: &QualName) -> String {
    match &name.prefix {
        Some(prefix) => format!("{prefix}:{}", name.local),
        None => name.local.to_string(),
    }
}

fn name_start(ch: char) -> bool {
    matches!(
        ch,
        ':' | '_' | 'A'..='Z' | 'a'..='z'
            | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}'
            | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}'
            | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}'
            | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}'
            | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}'
            | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}'
    )
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(name_start)
        && chars.all(|ch| {
            name_start(ch)
                || matches!(ch, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')
        })
}

fn check_name(name: &str, context: &mut Context) -> JsResult<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(dom_error("InvalidCharacterError", "Invalid attribute name", context))
    }
}

fn namespace_arg(value: &JsValue, context: &mut Context) -> JsResult<String> {
    if value.is_null_or_undefined() {
        Ok(String::new())
    } else {
        to_rust_string(value, context)
    }
}

fn qualified_ns(ns: &str, name: &str, context: &mut Context) -> JsResult<QualName> {
    check_name(name, context)?;
    let (prefix, local) = match name.split_once(':') {
        Some((prefix, local))
            if !prefix.is_empty()
                && !local.is_empty()
                && !local.contains(':')
                && valid_name(prefix)
                && valid_name(local)
                && !prefix.contains(':') => (Some(prefix), local),
        Some(_) => return Err(dom_error("InvalidCharacterError", "Invalid qualified name", context)),
        None => (None, name),
    };
    const XML: &str = "http://www.w3.org/XML/1998/namespace";
    const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
    if (prefix.is_some() && ns.is_empty())
        || (prefix == Some("xml") && ns != XML)
        || ((prefix == Some("xmlns") || name == "xmlns") && ns != XMLNS)
        || (ns == XMLNS && prefix != Some("xmlns") && name != "xmlns")
    {
        return Err(dom_error("NamespaceError", "Qualified name and namespace disagree", context));
    }
    Ok(QualName::new(
        prefix.map(Into::into),
        Namespace::from(ns),
        LocalName::from(local),
    ))
}

fn html_name(ctx: &DomCtx, id: NodeId, name: String) -> String {
    if ctx.doc.borrow().get_node(id).and_then(|node| node.element_data())
        .is_some_and(|element| element.name.ns == markup5ever::ns!(html))
    {
        name.to_ascii_lowercase()
    } else {
        name
    }
}

fn names(ctx: &DomCtx, id: NodeId) -> Vec<QualName> {
    ctx.doc.borrow().get_node(id).and_then(|node| node.element_data())
        .map(|element| element.attrs().iter().map(|attr| attr.name.clone()).collect())
        .unwrap_or_default()
}

fn find_name(ctx: &DomCtx, id: NodeId, name: &str) -> Option<QualName> {
    names(ctx, id).into_iter().find(|candidate| qualified(candidate) == name)
}

fn find_ns(ctx: &DomCtx, id: NodeId, ns: &str, local: &str) -> Option<QualName> {
    names(ctx, id).into_iter().find(|name| &*name.ns == ns && &*name.local == local)
}

fn document_for(ctx: &DomCtx, mut id: NodeId, context: &mut Context) -> JsObject {
    let document_id = {
        let doc = ctx.doc.borrow();
        loop {
            let Some(node) = doc.get_node(id) else {
                break doc.root_node().id;
            };
            if matches!(node.data, blitz_dom::node::NodeData::Document(_)) {
                break id;
            }
            match node.parent {
                Some(parent) => id = parent,
                None => break doc.root_node().id,
            }
        }
    };
    node_wrapper(ctx, document_id, context)
}

fn new_attr(
    handle: Arc<RwLock<AttributeNode>>,
    owner: Option<JsObject>,
    document: JsObject,
    context: &mut Context,
) -> JsObject {
    let proto = context.get_data::<DomaState>().expect("DOMA state").attr_proto.clone();
    let object = JsObject::from_proto_and_data(
        Some(proto),
        AttrRef {
            handle: Arc::clone(&handle),
            owner: boa_gc::GcRefCell::new(owner),
            document: boa_gc::GcRefCell::new(document),
        },
    );
    let state = context.get_data::<DomaState>().expect("DOMA state");
    let mut attrs = state.attrs.borrow_mut();
    attrs.retain(|(slot, wrapper)| slot.strong_count() != 0 && wrapper.upgrade().is_some());
    attrs.push((Arc::downgrade(&handle), object.downgrade()));
    object
}

fn wrap(ctx: &DomCtx, id: NodeId, name: &QualName, context: &mut Context) -> JsValue {
    let handle = {
        let doc = ctx.doc.borrow();
        doc.get_node(id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.attrs.attribute_node(name))
    };
    let Some(handle) = handle else {
        return JsValue::null();
    };
    let existing = {
        let state = context.get_data::<DomaState>().expect("DOMA state");
        state.attrs.borrow().iter().find_map(|(slot, wrapper)| {
            slot.upgrade()
                .filter(|slot| Arc::ptr_eq(slot, &handle))
                .and_then(|_| wrapper.upgrade())
        })
    };
    if let Some(existing) = existing {
        return existing.into();
    }
    let owner = node_wrapper(ctx, id, context);
    let document = document_for(ctx, id, context);
    new_attr(handle, Some(owner), document, context).into()
}

pub(crate) fn owner(object: &JsObject) -> Option<JsObject> {
    let data = object.downcast_ref::<AttrRef>().expect("checked Attr");
    if !data.handle.read().unwrap().attached {
        data.owner.borrow_mut().take();
    }
    data.owner.borrow().clone()
}

fn name(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_str(&qualified(&snapshot(&require_attr(this)?).0)))
}

fn local_name(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_str(&snapshot(&require_attr(this)?).0.local))
}

fn prefix(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (name, _) = snapshot(&require_attr(this)?);
    Ok(name.prefix.as_deref().map_or_else(JsValue::null, js_str))
}

fn namespace_uri(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (name, _) = snapshot(&require_attr(this)?);
    Ok(if name.ns.is_empty() { JsValue::null() } else { js_str(&name.ns) })
}

fn value(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = require_attr(this)?;
    let _ = owner(&object);
    Ok(js_str(&snapshot(&object).1))
}

fn set_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = require_attr(this)?;
    let value = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let (name, _) = snapshot(&object);
    if let Some(owner) = owner(&object) {
        let ctx = dom_ctx(context)?;
        let id = this_node_id(&owner.into())?;
        ctx.mutate_doc().mutate().set_attribute(id, name, &value);
    } else {
        object.downcast_ref::<AttrRef>().expect("checked Attr")
            .handle.write().unwrap().value = AttrAtom::from(value.as_str());
    }
    Ok(JsValue::undefined())
}

fn owner_element(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(owner(&require_attr(this)?).map_or_else(JsValue::null, Into::into))
}

fn owner_document(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = require_attr(this)?;
    let document = object.downcast_ref::<AttrRef>().expect("checked Attr").document.borrow().clone();
    Ok(document.into())
}

fn specified(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    require_attr(this)?;
    Ok(true.into())
}

fn node_type(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    require_attr(this)?;
    Ok(2.into())
}

fn disconnected(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    require_attr(this)?;
    Ok(false.into())
}

fn null(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    require_attr(this)?;
    Ok(JsValue::null())
}

fn no_children(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    require_attr(this)?;
    Ok(JsArray::from_iter(Vec::<JsValue>::new(), context).into())
}

fn contains(this: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = require_attr(this)?;
    Ok(args.first().and_then(attr_object).is_some_and(|other| JsObject::equals(&object, &other)).into())
}

fn clone_attr(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = require_attr(this)?;
    let (name, value) = snapshot(&object);
    let document = object.downcast_ref::<AttrRef>().expect("checked Attr").document.borrow().clone();
    Ok(new_attr(Arc::new(RwLock::new(AttributeNode {
        name, value, attached: false,
    })), None, document, context).into())
}

fn create_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    check_name(&name, context)?;
    let name = name.to_ascii_lowercase();
    let document = this.as_object().expect("checked document");
    Ok(new_attr(Arc::new(RwLock::new(AttributeNode {
        name: QualName::new(None, markup5ever::ns!(), LocalName::from(name)),
        value: AttrAtom::from(""),
        attached: false,
    })), None, document, context).into())
}

fn create_attribute_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    this_node_id(this)?;
    let ns = namespace_arg(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let name = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let name = qualified_ns(&ns, &name, context)?;
    Ok(new_attr(Arc::new(RwLock::new(AttributeNode {
        name, value: AttrAtom::from(""), attached: false,
    })), None, this.as_object().expect("checked document"), context).into())
}

fn get_attribute_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let input = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let input = html_name(&ctx, id, input);
    Ok(find_name(&ctx, id, &input).map_or_else(JsValue::null, |name| wrap(&ctx, id, &name, context)))
}

fn get_attribute_node_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let ns = namespace_arg(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let local = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    Ok(find_ns(&ctx, id, &ns, &local).map_or_else(JsValue::null, |name| wrap(&ctx, id, &name, context)))
}

fn set_attribute_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let supplied = args.first().and_then(attr_object)
        .ok_or_else(|| JsNativeError::typ().with_message("argument is not an Attr"))?;
    if let Some(current_owner) = owner(&supplied) {
        if this_node_id(&current_owner.into())? != id {
            return Err(dom_error("InUseAttributeError", "Attr belongs to another element", context));
        }
    }
    let (name, value) = snapshot(&supplied);
    let old_name = find_ns(&ctx, id, &name.ns, &name.local);
    let old = old_name.as_ref().map_or_else(JsValue::null, |name| wrap(&ctx, id, name, context));
    if old.as_object().is_some_and(|object| JsObject::equals(&object, &supplied)) {
        return Ok(old);
    }
    if let Some(old_object) = attr_object(&old) {
        old_object.downcast_ref::<AttrRef>().expect("checked Attr").owner.borrow_mut().take();
    }
    let handle = supplied.downcast_ref::<AttrRef>().expect("checked Attr").handle.clone();
    {
        let mut doc = ctx.mutate_doc();
        if let Some(old_name) = &old_name {
            doc.get_node(id).and_then(|node| node.element_data())
                .expect("checked element").attrs.detach_attribute_node(old_name);
        }
        doc.mutate().set_attribute(id, name, &value);
        doc.get_node(id).and_then(|node| node.element_data())
            .expect("checked element").attrs.bind_attribute_node(&handle);
    }
    let document = document_for(&ctx, id, context);
    let data = supplied.downcast_ref::<AttrRef>().expect("checked Attr");
    *data.owner.borrow_mut() = this.as_object();
    *data.document.borrow_mut() = document;
    Ok(old)
}

fn remove_named(ctx: &DomCtx, id: NodeId, name: QualName, context: &mut Context) -> JsValue {
    let old = wrap(ctx, id, &name, context);
    if let Some(object) = attr_object(&old) {
        object.downcast_ref::<AttrRef>().expect("checked Attr").owner.borrow_mut().take();
    }
    ctx.mutate_doc().mutate().clear_attribute(id, name);
    old
}

fn remove_attribute_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let object = args.first().and_then(attr_object)
        .ok_or_else(|| JsNativeError::typ().with_message("argument is not an Attr"))?;
    if owner(&object).and_then(|owner| this_node_id(&owner.into()).ok()) != Some(id) {
        return Err(dom_error("NotFoundError", "Attr is not owned by this element", context));
    }
    Ok(remove_named(&ctx, id, snapshot(&object).0, context))
}

fn get_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let attr = get_attribute_node(this, args, context)?;
    match attr_object(&attr) {
        Some(object) => Ok(js_str(&snapshot(&object).1)),
        None => Ok(JsValue::null()),
    }
}

fn set_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let input = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    check_name(&input, context)?;
    let input = html_name(&ctx, id, input);
    let value = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let name = find_name(&ctx, id, &input)
        .unwrap_or_else(|| QualName::new(None, markup5ever::ns!(), LocalName::from(input)));
    ctx.mutate_doc().mutate().set_attribute(id, name, &value);
    Ok(JsValue::undefined())
}

fn remove_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let input = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let input = html_name(&ctx, id, input);
    if let Some(name) = find_name(&ctx, id, &input) {
        remove_named(&ctx, id, name, context);
    }
    Ok(JsValue::undefined())
}

fn has_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(!get_attribute_node(this, args, context)?.is_null()))
}

fn toggle_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let input = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    check_name(&input, context)?;
    let input = html_name(&ctx, id, input);
    let existing = find_name(&ctx, id, &input);
    let present = existing.is_some();
    let desired = args.get(1).filter(|value| !value.is_undefined())
        .map_or(!present, JsValue::to_boolean);
    if desired && !present {
        ctx.mutate_doc().mutate().set_attribute(
            id, QualName::new(None, markup5ever::ns!(), LocalName::from(input)), "",
        );
    } else if !desired {
        if let Some(name) = existing {
            remove_named(&ctx, id, name, context);
        }
    }
    Ok(desired.into())
}

fn get_attribute_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let attr = get_attribute_node_ns(this, args, context)?;
    Ok(attr_object(&attr).map_or_else(JsValue::null, |object| js_str(&snapshot(&object).1)))
}

fn set_attribute_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let ns = namespace_arg(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let input = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let name = qualified_ns(&ns, &input, context)?;
    let value = to_rust_string(args.get(2).unwrap_or(&JsValue::undefined()), context)?;
    ctx.mutate_doc().mutate().set_attribute(id, name, &value);
    Ok(JsValue::undefined())
}

fn remove_attribute_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    let ns = namespace_arg(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let local = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    if let Some(name) = find_ns(&ctx, id, &ns, &local) {
        remove_named(&ctx, id, name, context);
    }
    Ok(JsValue::undefined())
}

fn has_attribute_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(!get_attribute_node_ns(this, args, context)?.is_null()))
}

fn has_attributes(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(JsValue::from(!names(&ctx, require_element(this, &ctx)?).is_empty()))
}

fn attribute_names(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = require_element(this, &ctx)?;
    Ok(JsArray::from_iter(names(&ctx, id).iter().map(|name| js_str(&qualified(name))), context).into())
}

fn map_owner(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let object = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("invalid NamedNodeMap"))?;
    let owner = object.get(JsString::from(MAP_OWNER), context)?;
    this_node_id(&owner)?;
    Ok(owner)
}

fn map_length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = map_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    Ok((names(&ctx, this_node_id(&owner)?).len() as f64).into())
}

fn map_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = map_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    let id = this_node_id(&owner)?;
    let index = index_arg(args, context)?;
    Ok(names(&ctx, id).get(index).map_or_else(JsValue::null, |name| wrap(&ctx, id, name, context)))
}

fn map_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    get_attribute_node(&map_owner(this, context)?, args, context)
}

fn map_get_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    get_attribute_node_ns(&map_owner(this, context)?, args, context)
}

fn map_set(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_attribute_node(&map_owner(this, context)?, args, context)
}

fn map_remove(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = map_owner(this, context)?;
    let old = get_attribute_node(&owner, args, context)?;
    if old.is_null() {
        return Err(dom_error("NotFoundError", "No attribute with this name", context));
    }
    remove_attribute_node(&owner, &[old], context)
}

fn map_remove_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = map_owner(this, context)?;
    let old = get_attribute_node_ns(&owner, args, context)?;
    if old.is_null() {
        return Err(dom_error("NotFoundError", "No attribute with this namespace and name", context));
    }
    remove_attribute_node(&owner, &[old], context)
}

fn proxy_target(args: &[JsValue]) -> JsResult<JsObject> {
    args.first().and_then(JsValue::as_object)
        .filter(|object| object.downcast_ref::<MapRef>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("invalid NamedNodeMap target").into())
}

fn key_string(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::String(key) => Some(key.to_std_string_lossy()),
        PropertyKey::Index(key) => Some(key.get().to_string()),
        PropertyKey::Symbol(_) => None,
    }
}

fn supported(target: &JsObject, key: &PropertyKey, context: &mut Context) -> JsResult<Option<QualName>> {
    let Some(key) = key_string(key) else { return Ok(None); };
    let owner = target.downcast_ref::<MapRef>().expect("checked target").owner.clone();
    let ctx = dom_ctx(context)?;
    let id = this_node_id(&owner.into())?;
    let attrs = names(&ctx, id);
    if let Ok(index) = key.parse::<usize>() {
        if index.to_string() == key {
            return Ok(attrs.get(index).cloned());
        }
    }
    if target.has_property(JsString::from(key.as_str()), context)? {
        return Ok(None);
    }
    Ok(attrs.into_iter().find(|name| qualified(name) == key))
}

fn proxy_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = proxy_target(args)?;
    let key = args.get(1).unwrap_or(&JsValue::undefined()).to_property_key(context)?;
    if let Some(name) = supported(&target, &key, context)? {
        let owner = target.downcast_ref::<MapRef>().expect("checked target").owner.clone();
        let ctx = dom_ctx(context)?;
        return Ok(wrap(&ctx, this_node_id(&owner.into())?, &name, context));
    }
    target.get(key, context)
}

fn proxy_has(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = proxy_target(args)?;
    let key = args.get(1).unwrap_or(&JsValue::undefined()).to_property_key(context)?;
    Ok((supported(&target, &key, context)?.is_some() || target.has_property(key, context)?).into())
}

fn proxy_keys(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = proxy_target(args)?;
    let owner = target.downcast_ref::<MapRef>().expect("checked target").owner.clone();
    let ctx = dom_ctx(context)?;
    let attrs = names(&ctx, this_node_id(&owner.into())?);
    let mut keys: Vec<String> = (0..attrs.len()).map(|index| index.to_string()).collect();
    for name in attrs {
        let key = qualified(&name);
        if !keys.contains(&key) && !target.has_property(JsString::from(key.as_str()), context)? {
            keys.push(key);
        }
    }
    Ok(JsArray::from_iter(keys.iter().map(|key| js_str(key)), context).into())
}

fn proxy_descriptor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = proxy_target(args)?;
    let key = args.get(1).unwrap_or(&JsValue::undefined()).to_property_key(context)?;
    let Some(name) = supported(&target, &key, context)? else {
        return Ok(JsValue::undefined());
    };
    let owner = target.downcast_ref::<MapRef>().expect("checked target").owner.clone();
    let ctx = dom_ctx(context)?;
    let value = wrap(&ctx, this_node_id(&owner.into())?, &name, context);
    let enumerable = key_string(&key).is_some_and(|key| {
        key.parse::<usize>().is_ok_and(|index| index.to_string() == key)
    });
    Ok(ObjectInitializer::new(context)
        .property(js_string!("value"), value, Attribute::all())
        .property(js_string!("writable"), false, Attribute::all())
        .property(js_string!("enumerable"), enumerable, Attribute::all())
        .property(js_string!("configurable"), true, Attribute::all())
        .build().into())
}

fn attributes(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    require_element(this, &ctx)?;
    let owner = this.as_object().expect("checked element");
    let cached = owner.get(JsString::from(MAP_CACHE), context)?;
    if cached.is_object() {
        return Ok(cached);
    }
    let proto = context.get_data::<DomaState>().expect("DOMA state").map_proto.clone();
    let target = JsObject::from_proto_and_data(Some(proto), MapRef { owner: owner.clone() });
    define_value(&target, MAP_OWNER, owner.clone().into(), context);
    let map: JsObject = JsProxyBuilder::new(target)
        .get(proxy_get)
        .has(proxy_has)
        .own_keys(proxy_keys)
        .get_own_property_descriptor(proxy_descriptor)
        .build(context)?.into();
    define_value(&owner, MAP_CACHE, map.clone().into(), context);
    Ok(map.into())
}


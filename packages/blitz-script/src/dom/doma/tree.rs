//! ParentNode, ChildNode and Node algorithms over the native tree.

use blitz_dom::node::NodeData;
use blitz_dom::{NodeId, QualName};
use boa_engine::object::JsObject;
use boa_engine::object::builtins::{JsArray, JsProxyBuilder};
use boa_engine::property::PropertyKey;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace, js_string,
};

use super::{attr, dom_error, index_arg, install_iterator};
use crate::dom::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_id_of_value, node_or_null,
    node_wrapper, this_node_id, to_rust_string,
};
use crate::state::DomCtx;

const CHILDREN_CACHE: &str = "__blitz_internal_element_children__";
const CHILDREN_OWNER: &str = "__blitz_internal_element_children_owner__";

pub(crate) fn install_node(proto: &JsObject, context: &mut Context) {
    define_method(proto, "getRootNode", 0, get_root_node, context);
    define_method(proto, "isEqualNode", 1, is_equal_node, context);
    define_method(proto, "isSameNode", 1, is_same_node, context);
    define_method(proto, "normalize", 0, normalize, context);
    define_method(
        proto,
        "lookupNamespaceURI",
        1,
        lookup_namespace_uri,
        context,
    );
    define_method(proto, "append", 0, append, context);
    define_method(proto, "prepend", 0, prepend, context);
    define_method(proto, "replaceChildren", 0, replace_children, context);
    define_method(proto, "appendChild", 1, append_child, context);
    define_method(proto, "insertBefore", 2, insert_before, context);
    define_method(proto, "replaceChild", 2, replace_child, context);
}

pub(crate) fn install_parent(proto: &JsObject, context: &mut Context) {
    define_accessor(
        proto,
        "firstElementChild",
        Some(first_element_child),
        None,
        context,
    );
    define_accessor(
        proto,
        "lastElementChild",
        Some(last_element_child),
        None,
        context,
    );
    define_accessor(
        proto,
        "childElementCount",
        Some(child_element_count),
        None,
        context,
    );
    define_accessor(proto, "children", Some(children), None, context);
}

pub(crate) fn install_child(proto: &JsObject, context: &mut Context) {
    define_accessor(
        proto,
        "nextElementSibling",
        Some(next_element_sibling),
        None,
        context,
    );
    define_accessor(
        proto,
        "previousElementSibling",
        Some(previous_element_sibling),
        None,
        context,
    );
    define_method(proto, "before", 0, before, context);
    define_method(proto, "after", 0, after, context);
    define_method(proto, "replaceWith", 0, replace_with, context);
}

pub(crate) fn install_fragment(proto: &JsObject, context: &mut Context) {
    define_method(proto, "querySelector", 1, query_selector, context);
    define_method(proto, "querySelectorAll", 1, query_selector_all, context);
    define_method(proto, "getElementById", 1, get_element_by_id, context);
}

fn child_ids(ctx: &DomCtx, id: NodeId) -> Vec<NodeId> {
    ctx.doc
        .borrow()
        .get_node(id)
        .map(|node| node.children.to_vec())
        .unwrap_or_default()
}

fn element_ids(ctx: &DomCtx, id: NodeId) -> Vec<NodeId> {
    let doc = ctx.doc.borrow();
    doc.get_node(id)
        .map(|node| {
            node.children
                .iter()
                .copied()
                .filter(|id| doc.get_node(*id).is_some_and(|node| node.is_element()))
                .collect()
        })
        .unwrap_or_default()
}

fn first_element_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(
        &ctx,
        element_ids(&ctx, this_node_id(this)?).first().copied(),
        context,
    ))
}

fn last_element_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(
        &ctx,
        element_ids(&ctx, this_node_id(this)?).last().copied(),
        context,
    ))
}

fn child_element_count(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok((element_ids(&ctx, this_node_id(this)?).len() as f64).into())
}

fn element_sibling(ctx: &DomCtx, id: NodeId, forward: bool) -> Option<NodeId> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(id)?;
    let siblings = &doc.get_node(node.parent?)?.children;
    let position = siblings.iter().position(|candidate| *candidate == id)?;
    if forward {
        siblings[position + 1..]
            .iter()
            .copied()
            .find(|id| doc.get_node(*id).is_some_and(|node| node.is_element()))
    } else {
        siblings[..position]
            .iter()
            .rev()
            .copied()
            .find(|id| doc.get_node(*id).is_some_and(|node| node.is_element()))
    }
}

fn next_element_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(
        &ctx,
        element_sibling(&ctx, this_node_id(this)?, true),
        context,
    ))
}

fn previous_element_sibling(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(
        &ctx,
        element_sibling(&ctx, this_node_id(this)?, false),
        context,
    ))
}

#[derive(Trace, Finalize, JsData)]
struct ChildrenRef {
    owner: JsObject,
}

fn collection_owner(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("invalid children collection"))?;
    let owner = object.get(JsString::from(CHILDREN_OWNER), context)?;
    this_node_id(&owner)?;
    Ok(owner)
}

fn collection_length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = collection_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    Ok((element_ids(&ctx, this_node_id(&owner)?).len() as f64).into())
}

fn collection_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = collection_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    let index = index_arg(args, context)?;
    Ok(node_or_null(
        &ctx,
        element_ids(&ctx, this_node_id(&owner)?).get(index).copied(),
        context,
    ))
}

fn collection_named(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = collection_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ids = element_ids(&ctx, this_node_id(&owner)?);
    let id = if name.is_empty() {
        None
    } else {
        let doc = ctx.doc.borrow();
        ids.into_iter().find(|id| {
            doc.get_node(*id)
                .and_then(|node| node.element_data())
                .is_some_and(|element| {
                    element.attr(markup5ever::local_name!("id")) == Some(name.as_str())
                        || (element.name.ns == markup5ever::ns!(html)
                            && element.attr(markup5ever::local_name!("name"))
                                == Some(name.as_str()))
                })
        })
    };
    Ok(node_or_null(&ctx, id, context))
}

fn collection_target(args: &[JsValue]) -> JsResult<JsObject> {
    args.first()
        .and_then(JsValue::as_object)
        .filter(|object| object.downcast_ref::<ChildrenRef>().is_some())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("invalid children target")
                .into()
        })
}

fn collection_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = collection_target(args)?;
    let key = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_property_key(context)?;
    let string = match &key {
        PropertyKey::String(key) => Some(key.to_std_string_lossy()),
        PropertyKey::Index(key) => Some(key.get().to_string()),
        PropertyKey::Symbol(_) => None,
    };
    if let Some(string) = string {
        if let Ok(index) = string.parse::<usize>() {
            if index.to_string() == string {
                let owner = target
                    .downcast_ref::<ChildrenRef>()
                    .expect("checked collection")
                    .owner
                    .clone();
                let ctx = dom_ctx(context)?;
                let ids = element_ids(&ctx, this_node_id(&owner.into())?);
                return Ok(ids.get(index).map_or_else(JsValue::undefined, |id| {
                    node_wrapper(&ctx, *id, context).into()
                }));
            }
        }
        if !target.has_property(key.clone(), context)? {
            let named = collection_named(&target.clone().into(), &[js_str(&string)], context)?;
            if !named.is_null() {
                return Ok(named);
            }
        }
    }
    target.get(key, context)
}

fn children(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    this_node_id(this)?;
    let owner = this.as_object().expect("checked node");
    let cached = owner.get(JsString::from(CHILDREN_CACHE), context)?;
    if cached.is_object() {
        return Ok(cached);
    }
    let interface = context
        .global_object()
        .clone()
        .get(js_string!("HTMLCollection"), context)?;
    let proto = match interface.as_object() {
        Some(interface) => interface.get(js_string!("prototype"), context)?.as_object(),
        None => None,
    }
    .unwrap_or_else(|| context.intrinsics().constructors().object().prototype());
    let target = JsObject::from_proto_and_data(
        Some(proto),
        ChildrenRef {
            owner: owner.clone(),
        },
    );
    define_value(&target, CHILDREN_OWNER, owner.clone().into(), context);
    define_accessor(&target, "length", Some(collection_length), None, context);
    define_method(&target, "item", 1, collection_item, context);
    define_method(&target, "namedItem", 1, collection_named, context);
    install_iterator(&target, context);
    let collection: JsObject = JsProxyBuilder::new(target)
        .get(collection_get)
        .build(context)?
        .into();
    define_value(&owner, CHILDREN_CACHE, collection.clone().into(), context);
    Ok(collection.into())
}

fn query_selector(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let found = ctx
        .doc
        .borrow()
        .query_selector_in(id, &selector)
        .map_err(|_| crate::dom::element::invalid_selector(&selector, context))?;
    Ok(node_or_null(&ctx, found, context))
}

fn query_selector_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ids = ctx
        .doc
        .borrow()
        .query_selector_all_in(id, &selector)
        .map_err(|_| crate::dom::element::invalid_selector(&selector, context))?;
    let wrappers: Vec<JsValue> = ids
        .into_iter()
        .map(|id| node_wrapper(&ctx, id, context).into())
        .collect();
    Ok(JsArray::from_iter(wrappers, context).into())
}

fn get_element_by_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let root = this_node_id(this)?;
    let wanted = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let found = {
        let doc = ctx.doc.borrow();
        let mut stack: Vec<NodeId> = doc
            .get_node(root)
            .map(|node| node.children.iter().rev().copied().collect())
            .unwrap_or_default();
        let mut found = None;
        while let Some(id) = stack.pop() {
            let Some(node) = doc.get_node(id) else {
                continue;
            };
            if !wanted.is_empty()
                && node
                    .element_data()
                    .and_then(|element| element.attr(markup5ever::local_name!("id")))
                    == Some(wanted.as_str())
            {
                found = Some(id);
                break;
            }
            stack.extend(node.children.iter().rev().copied());
        }
        found
    };
    Ok(node_or_null(&ctx, found, context))
}

fn get_root_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if attr::attr_object(this).is_some() {
        return Ok(this.clone());
    }
    let ctx = dom_ctx(context)?;
    let mut id = this_node_id(this)?;
    let composed = match args.first().and_then(JsValue::as_object) {
        Some(options) => options.get(js_string!("composed"), context)?.to_boolean(),
        None => false,
    };
    {
        let doc = ctx.doc.borrow();
        loop {
            let node = doc
                .get_node(id)
                .ok_or_else(|| JsNativeError::typ().with_message("node no longer exists"))?;
            if let NodeData::ShadowRoot(root) = &node.data {
                if !composed {
                    break;
                }
                id = root.host;
            } else if let Some(parent) = node.parent {
                id = parent;
            } else {
                break;
            }
        }
    }
    Ok(node_wrapper(&ctx, id, context).into())
}

fn is_same_node(this: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    if node_id_of_value(this).is_none() && attr::attr_object(this).is_none() {
        return Err(JsNativeError::typ()
            .with_message("receiver is not a Node")
            .into());
    }
    Ok(
        match (this.as_object(), args.first().and_then(JsValue::as_object)) {
            (Some(left), Some(right)) => JsObject::equals(&left, &right),
            _ => false,
        }
        .into(),
    )
}

fn same_element(left: &blitz_dom::node::ElementData, right: &blitz_dom::node::ElementData) -> bool {
    left.name == right.name
        && left.attrs().len() == right.attrs().len()
        && left
            .attrs()
            .iter()
            .all(|attr| right.attrs().iter().any(|other| other == attr))
}

fn is_equal_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(left) = attr::attr_object(this) {
        return Ok(args
            .first()
            .and_then(attr::attr_object)
            .is_some_and(|right| attr::snapshot(&left) == attr::snapshot(&right))
            .into());
    }
    let ctx = dom_ctx(context)?;
    let left = this_node_id(this)?;
    let Some(right) = args.first().and_then(node_id_of_value) else {
        return Ok(false.into());
    };
    let doc = ctx.doc.borrow();
    let mut stack = vec![(left, right)];
    while let Some((left, right)) = stack.pop() {
        let (Some(left), Some(right)) = (doc.get_node(left), doc.get_node(right)) else {
            return Ok(false.into());
        };
        let equal = match (&left.data, &right.data) {
            (NodeData::Element(left), NodeData::Element(right)) => same_element(left, right),
            (NodeData::AnonymousBlock(left), NodeData::AnonymousBlock(right)) => {
                same_element(left, right)
            }
            (NodeData::Text(left), NodeData::Text(right)) => left.content == right.content,
            (NodeData::Comment { contents: left }, NodeData::Comment { contents: right }) => {
                left == right
            }
            (NodeData::Document(_), NodeData::Document(_))
            | (NodeData::DocumentFragment, NodeData::DocumentFragment)
            | (NodeData::ShadowRoot(_), NodeData::ShadowRoot(_)) => true,
            _ => false,
        };
        if !equal || left.children.len() != right.children.len() {
            return Ok(false.into());
        }
        stack.extend(
            left.children
                .iter()
                .copied()
                .zip(right.children.iter().copied()),
        );
    }
    Ok(true.into())
}

fn normalize(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if attr::attr_object(this).is_some() {
        return Ok(JsValue::undefined());
    }
    let ctx = dom_ctx(context)?;
    let mut stack = vec![this_node_id(this)?];
    while let Some(parent) = stack.pop() {
        let children = child_ids(&ctx, parent);
        let mut index = 0;
        while index < children.len() {
            let id = children[index];
            let first_text = {
                let doc = ctx.doc.borrow();
                doc.get_node(id)
                    .and_then(|node| node.text_data())
                    .map(|text| text.content.clone())
            };
            if let Some(mut text) = first_text {
                let start = index;
                index += 1;
                while index < children.len() {
                    let next = {
                        let doc = ctx.doc.borrow();
                        doc.get_node(children[index])
                            .and_then(|node| node.text_data())
                            .map(|text| text.content.clone())
                    };
                    let Some(next) = next else {
                        break;
                    };
                    text.push_str(&next);
                    index += 1;
                }
                // The spec drops empty Text nodes first, so the node that keeps
                // the merged data is the first non-empty one in the run.
                let keeper = children[start..index].iter().copied().find(|child| {
                    let doc = ctx.doc.borrow();
                    doc.get_node(*child)
                        .and_then(|node| node.text_data())
                        .is_some_and(|text| !text.content.is_empty())
                });
                let original = keeper.map(|keeper| {
                    let doc = ctx.doc.borrow();
                    doc.get_node(keeper)
                        .and_then(|node| node.text_data())
                        .map(|text| text.content.clone())
                        .unwrap_or_default()
                });
                if let (Some(keeper), Some(original)) = (keeper, original.as_ref())
                    && original != &text
                {
                    ctx.mutate_doc()
                        .mutate()
                        .replace_character_data(
                            keeper,
                            original.encode_utf16().count(),
                            0,
                            &text[original.len()..],
                        )
                        .map_err(|name| {
                            dom_error(name, "Cannot normalize CharacterData", context)
                        })?;
                }
                let mut prefix = 0;
                let mut reached_keeper = false;
                for child in &children[start..index] {
                    let length = ctx.doc.borrow().range_node_length(*child).unwrap_or(0);
                    if Some(*child) == keeper {
                        reached_keeper = true;
                        prefix = original
                            .as_ref()
                            .map_or(0, |text| text.encode_utf16().count());
                    } else {
                        if reached_keeper && let Some(keeper) = keeper {
                            ctx.doc
                                .borrow_mut()
                                .range_merge_text(keeper, *child, prefix);
                            prefix += length;
                        }
                        crate::dom::remove_and_free_node(&ctx, *child, context);
                    }
                }
            } else {
                stack.push(id);
                index += 1;
            }
        }
    }
    Ok(JsValue::undefined())
}

fn lookup_namespace_uri(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(object) = attr::attr_object(this) {
        return match attr::owner(&object) {
            Some(owner) => lookup_namespace_uri(&owner.into(), args, context),
            None => Ok(JsValue::null()),
        };
    }
    let prefix = match args.first() {
        Some(value) if !value.is_null_or_undefined() => to_rust_string(value, context)?,
        _ => String::new(),
    };
    let ctx = dom_ctx(context)?;
    let mut current = Some(this_node_id(this)?);
    let doc = ctx.doc.borrow();
    while let Some(id) = current {
        let Some(node) = doc.get_node(id) else {
            break;
        };
        match &node.data {
            NodeData::Document(_) => {
                current = node
                    .children
                    .iter()
                    .copied()
                    .find(|id| doc.get_node(*id).is_some_and(|node| node.is_element()));
                continue;
            }
            NodeData::DocumentFragment | NodeData::ShadowRoot(_) => return Ok(JsValue::null()),
            _ => {}
        }
        if let Some(element) = node.element_data() {
            if !element.name.ns.is_empty() && element.name.prefix.as_deref().unwrap_or("") == prefix
            {
                return Ok(js_str(&element.name.ns));
            }
            for attr in element.attrs() {
                if &*attr.name.ns == "http://www.w3.org/2000/xmlns/"
                    && ((prefix.is_empty()
                        && attr.name.prefix.is_none()
                        && &*attr.name.local == "xmlns")
                        || (attr.name.prefix.as_deref() == Some("xmlns")
                            && &*attr.name.local == prefix))
                {
                    return Ok(if attr.value.is_empty() {
                        JsValue::null()
                    } else {
                        js_str(&attr.value)
                    });
                }
            }
        }
        current = node.parent;
    }
    Ok(JsValue::null())
}

fn convert(ctx: &DomCtx, args: &[JsValue], context: &mut Context) -> JsResult<Vec<NodeId>> {
    let mut nodes = Vec::new();
    for value in args {
        let id = match node_id_of_value(value) {
            Some(id) => id,
            None => {
                if attr::attr_object(value).is_some() {
                    return Err(dom_error(
                        "HierarchyRequestError",
                        "An Attr cannot be inserted",
                        context,
                    ));
                }
                let text = to_rust_string(value, context)?;
                ctx.doc.borrow_mut().mutate().create_text_node(&text)
            }
        };
        if let Some(position) = nodes.iter().position(|candidate| *candidate == id) {
            nodes.remove(position);
        }
        nodes.push(id);
    }
    Ok(nodes)
}

fn expand(ctx: &DomCtx, nodes: &[NodeId]) -> Vec<NodeId> {
    let doc = ctx.doc.borrow();
    let mut expanded = Vec::new();
    for id in nodes {
        if let Some(node) = doc.get_node(*id) {
            if matches!(node.data, NodeData::DocumentFragment) {
                for child in &node.children {
                    expanded.retain(|candidate| candidate != child);
                    expanded.push(*child);
                }
            } else {
                expanded.retain(|candidate| candidate != id);
                expanded.push(*id);
            }
        }
    }
    expanded
}

pub(crate) fn validate(
    ctx: &DomCtx,
    parent: NodeId,
    source: &[NodeId],
    nodes: &[NodeId],
    reference: Option<NodeId>,
    removed: &[NodeId],
    context: &mut Context,
) -> JsResult<()> {
    let doc = ctx.doc.borrow();
    let parent_node = doc
        .get_node(parent)
        .ok_or_else(|| JsNativeError::typ().with_message("parent no longer exists"))?;
    if !matches!(
        parent_node.data,
        NodeData::Element(_)
            | NodeData::Document(_)
            | NodeData::DocumentFragment
            | NodeData::ShadowRoot(_)
    ) {
        return Err(dom_error(
            "HierarchyRequestError",
            "Node cannot have children",
            context,
        ));
    }
    if reference.is_some_and(|id| !parent_node.children.contains(&id)) {
        return Err(dom_error(
            "NotFoundError",
            "Reference is not a child of this parent",
            context,
        ));
    }
    for id in source.iter().chain(nodes) {
        let mut ancestor = Some(parent);
        while let Some(current) = ancestor {
            if current == *id {
                return Err(dom_error(
                    "HierarchyRequestError",
                    "Insertion would create a cycle",
                    context,
                ));
            }
            let Some(node) = doc.get_node(current) else {
                break;
            };
            ancestor = match &node.data {
                NodeData::ShadowRoot(root) if node.parent.is_none() => Some(root.host),
                _ => node.parent,
            };
        }
        let node = doc
            .get_node(*id)
            .ok_or_else(|| JsNativeError::typ().with_message("inserted node no longer exists"))?;
        if !matches!(
            node.data,
            NodeData::Element(_)
                | NodeData::Text(_)
                | NodeData::Comment { .. }
                | NodeData::DocumentFragment
        ) {
            return Err(dom_error(
                "HierarchyRequestError",
                "Node cannot be inserted",
                context,
            ));
        }
    }
    if matches!(parent_node.data, NodeData::Document(_)) {
        let mut elements = 0;
        for id in parent_node
            .children
            .iter()
            .filter(|id| !nodes.contains(id) && !removed.contains(id))
            .chain(nodes)
        {
            match &doc.get_node(*id).expect("existing child").data {
                NodeData::Element(_) => elements += 1,
                NodeData::Text(_) => {
                    return Err(dom_error(
                        "HierarchyRequestError",
                        "Documents cannot contain text children",
                        context,
                    ));
                }
                _ => {}
            }
        }
        if elements > 1 {
            return Err(dom_error(
                "HierarchyRequestError",
                "Document already has a document element",
                context,
            ));
        }
    }
    Ok(())
}

pub(crate) fn insert(
    ctx: &DomCtx,
    parent: NodeId,
    source: &[NodeId],
    reference: Option<NodeId>,
    removed: &[NodeId],
    context: &mut Context,
) -> JsResult<()> {
    let nodes = expand(ctx, source);
    validate(ctx, parent, source, &nodes, reference, removed, context)?;
    let reference = if reference.is_some_and(|id| nodes.contains(&id)) {
        let children = child_ids(ctx, parent);
        let position = children
            .iter()
            .position(|id| Some(*id) == reference)
            .expect("validated reference");
        children[position + 1..]
            .iter()
            .copied()
            .find(|id| !nodes.contains(id))
    } else {
        reference
    };
    // A reference that is itself being replaced (replaceWith, outerHTML) is
    // gone before the insertion: anchor on the next sibling that survives.
    let reference = match reference {
        Some(anchor) if removed.contains(&anchor) => {
            let children = child_ids(ctx, parent);
            let position = children.iter().position(|id| *id == anchor);
            position.and_then(|position| {
                children[position + 1..]
                    .iter()
                    .copied()
                    .find(|id| !nodes.contains(id) && !removed.contains(id))
            })
        }
        other => other,
    };
    for id in &nodes {
        crate::dom::node::unroot_detached_listener_subtree(ctx, *id, context);
        let mut doc = ctx.mutate_doc();
        if doc.get_node(*id).is_some_and(|node| node.parent.is_some()) {
            doc.mutate().remove_node(*id);
        }
    }
    for id in removed {
        if !nodes.contains(id) {
            crate::dom::remove_and_free_node(ctx, *id, context);
        }
    }
    {
        let mut doc = ctx.mutate_doc();
        let mut mutr = doc.mutate();
        match reference {
            Some(reference) => mutr.insert_nodes_before(reference, &nodes),
            None => mutr.append_children(parent, &nodes),
        }
    }
    for id in nodes {
        crate::dom::mark_node_reattached(ctx, id);
        crate::dom::node::root_inline_event_handlers(ctx, id, context);
    }
    // Custom element reactions for the inserted subtrees (upgrades,
    // connectedCallback), from the native mutation records.
    crate::dom::custom_elements::checkpoint(ctx, context);
    Ok(())
}

fn append(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let parent = this_node_id(this)?;
    let nodes = convert(&ctx, args, context)?;
    insert(&ctx, parent, &nodes, None, &[], context)?;
    Ok(JsValue::undefined())
}

fn prepend(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let parent = this_node_id(this)?;
    let nodes = convert(&ctx, args, context)?;
    insert(
        &ctx,
        parent,
        &nodes,
        child_ids(&ctx, parent).first().copied(),
        &[],
        context,
    )?;
    Ok(JsValue::undefined())
}

fn replace_children(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let parent = this_node_id(this)?;
    let nodes = convert(&ctx, args, context)?;
    insert(
        &ctx,
        parent,
        &nodes,
        None,
        &child_ids(&ctx, parent),
        context,
    )?;
    Ok(JsValue::undefined())
}

fn required_node(args: &[JsValue], index: usize) -> JsResult<NodeId> {
    args.get(index).and_then(node_id_of_value).ok_or_else(|| {
        JsNativeError::typ()
            .with_message("argument is not a Node")
            .into()
    })
}

fn append_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    insert(
        &ctx,
        this_node_id(this)?,
        &[required_node(args, 0)?],
        None,
        &[],
        context,
    )?;
    Ok(args[0].clone())
}

fn insert_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let reference = match args.get(1) {
        Some(value) if !value.is_null_or_undefined() => Some(required_node(args, 1)?),
        _ => None,
    };
    insert(
        &ctx,
        this_node_id(this)?,
        &[required_node(args, 0)?],
        reference,
        &[],
        context,
    )?;
    Ok(args[0].clone())
}

fn replace_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let parent = this_node_id(this)?;
    let old = required_node(args, 1)?;
    let new = required_node(args, 0)?;
    insert(&ctx, parent, &[new], Some(old), &[old], context)?;
    Ok(args[1].clone())
}

fn child_operation(
    this: &JsValue,
    args: &[JsValue],
    operation: u8,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let parent = ctx.doc.borrow().get_node(id).and_then(|node| node.parent);
    let Some(parent) = parent else {
        return Ok(JsValue::undefined());
    };
    let source = convert(&ctx, args, context)?;
    let nodes = expand(&ctx, &source);
    let children = child_ids(&ctx, parent);
    let position = children
        .iter()
        .position(|child| *child == id)
        .expect("child belongs to parent");
    let reference = if operation == 0 {
        children[position..]
            .iter()
            .copied()
            .find(|child| !nodes.contains(child))
    } else {
        children[position + 1..]
            .iter()
            .copied()
            .find(|child| !nodes.contains(child))
    };
    let removed = if operation == 2 { vec![id] } else { Vec::new() };
    insert(&ctx, parent, &source, reference, &removed, context)?;
    Ok(JsValue::undefined())
}

fn before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    child_operation(this, args, 0, context)
}

fn after(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    child_operation(this, args, 1, context)
}

fn replace_with(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    child_operation(this, args, 2, context)
}

fn adjacent(
    this: &JsValue,
    args: &[JsValue],
    text: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let position = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    if !["beforebegin", "afterbegin", "beforeend", "afterend"].contains(&position.as_str()) {
        return Err(dom_error(
            "SyntaxError",
            "Invalid insertion position",
            context,
        ));
    }
    let node = if text {
        let value = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
        ctx.doc.borrow_mut().mutate().create_text_node(&value)
    } else {
        let node = required_node(args, 1)?;
        if !ctx
            .doc
            .borrow()
            .get_node(node)
            .is_some_and(|node| node.is_element())
        {
            return Err(JsNativeError::typ()
                .with_message("argument is not an Element")
                .into());
        }
        node
    };
    let (parent, reference) = match position.as_str() {
        "afterbegin" => (Some(id), child_ids(&ctx, id).first().copied()),
        "beforeend" => (Some(id), None),
        "beforebegin" => (
            ctx.doc.borrow().get_node(id).and_then(|node| node.parent),
            Some(id),
        ),
        _ => {
            let parent = ctx.doc.borrow().get_node(id).and_then(|node| node.parent);
            let next = parent.and_then(|parent| {
                let children = child_ids(&ctx, parent);
                children
                    .iter()
                    .position(|child| *child == id)
                    .and_then(|position| children.get(position + 1).copied())
            });
            (parent, next)
        }
    };
    let Some(parent) = parent else {
        return Ok(if text {
            JsValue::undefined()
        } else {
            JsValue::null()
        });
    };
    if !text
        && ["beforebegin", "afterend"].contains(&position.as_str())
        && !ctx
            .doc
            .borrow()
            .get_node(parent)
            .is_some_and(|node| node.is_element())
    {
        return Ok(JsValue::null());
    }
    insert(&ctx, parent, &[node], reference, &[], context)?;
    Ok(if text {
        JsValue::undefined()
    } else {
        node_wrapper(&ctx, node, context).into()
    })
}

pub(crate) fn insert_adjacent_element(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    adjacent(this, args, false, context)
}

pub(crate) fn insert_adjacent_text(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    adjacent(this, args, true, context)
}

pub(crate) fn set_outer_html(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let html = match args.first() {
        Some(value) if value.is_null() => String::new(),
        value => to_rust_string(value.unwrap_or(&JsValue::undefined()), context)?,
    };
    let parent = ctx.doc.borrow().get_node(id).and_then(|node| node.parent);
    let Some(parent) = parent else {
        return Ok(JsValue::undefined());
    };
    let context_name: QualName = {
        let doc = ctx.doc.borrow();
        let parent = doc.get_node(parent).expect("existing parent");
        if matches!(parent.data, NodeData::Document(_)) {
            return Err(dom_error(
                "NoModificationAllowedError",
                "Cannot replace a document element with outerHTML",
                context,
            ));
        }
        parent
            .element_data()
            .map(|element| element.name.clone())
            .unwrap_or_else(|| crate::dom::qual_name("body"))
    };
    let staging = {
        let mut doc = ctx.doc.borrow_mut();
        let mut mutr = doc.mutate();
        let staging = mutr.create_element(context_name, Vec::new());
        mutr.set_inner_html(staging, &html);
        staging
    };
    let target = ctx
        .doc
        .borrow()
        .get_node(staging)
        .and_then(|node| node.element_data())
        .and_then(|element| element.template_contents)
        .unwrap_or(staging);
    let nodes = child_ids(&ctx, target);
    let result = insert(&ctx, parent, &nodes, Some(id), &[id], context);
    ctx.doc.borrow_mut().mutate().remove_and_drop_node(staging);
    result?;
    Ok(JsValue::undefined())
}

//! The `Node` (and `CharacterData`) prototypes: tree structure, tree mutation,
//! text content and event listener registration.

use blitz_dom::node::NodeData;
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsNativeError, JsResult};

use super::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_id_of_value, node_or_null,
    node_wrapper, this_node_id, to_rust_string,
};
use crate::dom::event::{EventRef, set_event_path};
use crate::state::Listener;

pub(crate) fn init_node_proto(proto: &JsObject, context: &mut Context) {
    define_accessor(proto, "nodeType", Some(node_type), None, context);
    define_accessor(proto, "nodeName", Some(node_name), None, context);
    define_accessor(proto, "parentNode", Some(parent_node), None, context);
    define_accessor(proto, "parentElement", Some(parent_node), None, context);
    define_accessor(proto, "childNodes", Some(child_nodes), None, context);
    define_accessor(proto, "firstChild", Some(first_child), None, context);
    define_accessor(proto, "lastChild", Some(last_child), None, context);
    define_accessor(
        proto,
        "previousSibling",
        Some(previous_sibling),
        None,
        context,
    );
    define_accessor(proto, "nextSibling", Some(next_sibling), None, context);
    define_accessor(proto, "isConnected", Some(is_connected), None, context);
    define_accessor(proto, "ownerDocument", Some(owner_document), None, context);
    define_accessor(
        proto,
        "textContent",
        Some(text_content),
        Some(set_text_content),
        context,
    );
    define_accessor(
        proto,
        "nodeValue",
        Some(node_value),
        Some(set_node_value),
        context,
    );

    define_method(proto, "appendChild", 1, append_child, context);
    define_method(proto, "insertBefore", 2, insert_before, context);
    define_method(proto, "removeChild", 1, remove_child, context);
    define_method(proto, "replaceChild", 2, replace_child, context);
    define_method(proto, "remove", 0, remove, context);
    define_method(proto, "hasChildNodes", 0, has_child_nodes, context);
    define_method(proto, "contains", 1, contains, context);
    define_method(
        proto,
        "compareDocumentPosition",
        1,
        compare_document_position,
        context,
    );
    define_method(proto, "cloneNode", 1, clone_node, context);
    define_method(proto, "addEventListener", 2, add_event_listener, context);
    define_method(
        proto,
        "removeEventListener",
        2,
        remove_event_listener,
        context,
    );
    define_method(proto, "dispatchEvent", 1, dispatch_event, context);
}

pub(crate) fn init_character_data_proto(proto: &JsObject, context: &mut Context) {
    define_accessor(
        proto,
        "data",
        Some(node_value),
        Some(set_node_value),
        context,
    );
}

// === Read-only tree structure ===

fn node_type(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let node_type = match doc.get_node(node_id).map(|node| &node.data) {
        Some(NodeData::Document) => 9,
        Some(NodeData::Element(_)) | Some(NodeData::AnonymousBlock(_)) => 1,
        Some(NodeData::Text(_)) => 3,
        Some(NodeData::Comment) => 8,
        None => 0,
    };
    Ok(JsValue::from(node_type))
}

fn node_name(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let name = match doc.get_node(node_id).map(|node| &node.data) {
        Some(NodeData::Document) => "#document".to_string(),
        Some(NodeData::Element(data)) | Some(NodeData::AnonymousBlock(data)) => {
            data.name.local.to_uppercase()
        }
        Some(NodeData::Text(_)) => "#text".to_string(),
        Some(NodeData::Comment) => "#comment".to_string(),
        None => String::new(),
    };
    Ok(js_str(&name))
}

fn parent_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let parent_id = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .and_then(|node| node.parent);
    Ok(node_or_null(&ctx, parent_id, context))
}

fn child_ids(ctx: &crate::state::DomCtx, node_id: usize) -> Vec<usize> {
    ctx.doc
        .borrow()
        .get_node(node_id)
        .map(|node| node.children.clone())
        .unwrap_or_default()
}

fn child_nodes(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let children = child_ids(&ctx, node_id);
    let wrappers: Vec<JsValue> = children
        .into_iter()
        .map(|child_id| node_wrapper(&ctx, child_id, context).into())
        .collect();
    Ok(JsArray::from_iter(wrappers, context).into())
}

fn first_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let child_id = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .and_then(|node| node.children.first().copied());
    Ok(node_or_null(&ctx, child_id, context))
}

fn last_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let child_id = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .and_then(|node| node.children.last().copied());
    Ok(node_or_null(&ctx, child_id, context))
}

fn sibling(ctx: &crate::state::DomCtx, node_id: usize, offset: isize) -> Option<usize> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(node_id)?;
    let parent = doc.get_node(node.parent?)?;
    let index = parent.index_of_child(node_id)?;
    let sibling_index = index.checked_add_signed(offset)?;
    parent.children.get(sibling_index).copied()
}

fn previous_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let sibling_id = sibling(&ctx, node_id, -1);
    Ok(node_or_null(&ctx, sibling_id, context))
}

fn next_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let sibling_id = sibling(&ctx, node_id, 1);
    Ok(node_or_null(&ctx, sibling_id, context))
}

fn is_connected(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let connected = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .is_some_and(|node| node.flags.is_in_document());
    Ok(JsValue::from(connected))
}

fn owner_document(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let root_id = ctx.doc.borrow().root_node().id;
    Ok(node_or_null(&ctx, Some(root_id), context))
}

// === Text content ===

fn text_content(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let text = doc
        .get_node(node_id)
        .map(|node| node.text_content())
        .unwrap_or_default();
    Ok(js_str(&text))
}

fn set_text_content(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _t = crate::script_stats::Timed::new("dom:textContent=");
    let ctx = dom_ctx(context)?;
    // Layout is now behind the tree. The next geometry read flushes.
    ctx.mark_layout_dirty();
    let node_id = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    let is_text_like = {
        let doc = ctx.doc.borrow();
        matches!(
            doc.get_node(node_id).map(|node| &node.data),
            Some(NodeData::Text(_)) | Some(NodeData::Comment)
        )
    };

    let mut doc = ctx.doc.borrow_mut();
    let mut mutr = doc.mutate();
    if is_text_like {
        mutr.set_node_text(node_id, &text);
    } else {
        // Detach (rather than drop) any existing children so that JS wrappers
        // referencing them remain valid.
        for child_id in mutr.child_ids(node_id) {
            mutr.remove_node(child_id);
        }
        if !text.is_empty() {
            let text_id = mutr.create_text_node(&text);
            mutr.append_children(node_id, &[text_id]);
        }
    }
    Ok(JsValue::undefined())
}

fn node_value(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    match doc.get_node(node_id).map(|node| &node.data) {
        Some(NodeData::Text(data)) => Ok(js_str(&data.content)),
        Some(NodeData::Comment) => Ok(js_str("")),
        _ => Ok(JsValue::null()),
    }
}

fn set_node_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let mut doc = ctx.doc.borrow_mut();
    doc.mutate().set_node_text(node_id, &text);
    Ok(JsValue::undefined())
}

// === Tree mutation ===

fn arg_node_id(args: &[JsValue], index: usize) -> JsResult<usize> {
    args.get(index).and_then(node_id_of_value).ok_or_else(|| {
        JsNativeError::typ()
            .with_message("argument is not a DOM node")
            .into()
    })
}

fn append_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _t = crate::script_stats::Timed::new("dom:appendChild");
    let ctx = dom_ctx(context)?;
    // Layout is now behind the tree. The next geometry read flushes.
    ctx.mark_layout_dirty();
    let parent_id = this_node_id(this)?;
    let child_id = arg_node_id(args, 0)?;

    let mut doc = ctx.doc.borrow_mut();
    let mut mutr = doc.mutate();
    // Detach from any current parent first. This also makes "move to end of
    // same parent" operations behave correctly.
    if mutr.node_has_parent(child_id) {
        mutr.remove_node(child_id);
    }
    mutr.append_children(parent_id, &[child_id]);
    drop(mutr);
    drop(doc);

    Ok(args[0].clone())
}

fn insert_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _t = crate::script_stats::Timed::new("dom:insertBefore");
    let ctx = dom_ctx(context)?;
    // Layout is now behind the tree. The next geometry read flushes.
    ctx.mark_layout_dirty();
    let parent_id = this_node_id(this)?;
    let new_id = arg_node_id(args, 0)?;

    // A null/undefined reference node means "append"
    let ref_arg = args.get(1).cloned().unwrap_or_default();
    let ref_id = if ref_arg.is_null_or_undefined() {
        None
    } else {
        Some(arg_node_id(args, 1)?)
    };

    // Inserting a node before itself is a no-op
    if ref_id == Some(new_id) {
        return Ok(args[0].clone());
    }

    let mut doc = ctx.doc.borrow_mut();
    let mut mutr = doc.mutate();
    if mutr.node_has_parent(new_id) {
        mutr.remove_node(new_id);
    }
    match ref_id {
        Some(ref_id) if mutr.node_has_parent(ref_id) => {
            mutr.insert_nodes_before(ref_id, &[new_id]);
        }
        _ => mutr.append_children(parent_id, &[new_id]),
    }
    Ok(args[0].clone())
}

fn remove_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    // Layout is now behind the tree. The next geometry read flushes.
    ctx.mark_layout_dirty();
    let _parent_id = this_node_id(this)?;
    let child_id = arg_node_id(args, 0)?;

    let mut doc = ctx.doc.borrow_mut();
    // Note: the node is detached rather than dropped so that JS wrappers
    // referencing it (or its descendants) remain valid.
    doc.mutate().remove_node(child_id);
    Ok(args[0].clone())
}

fn replace_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    // Layout is now behind the tree. The next geometry read flushes.
    ctx.mark_layout_dirty();
    let _parent_id = this_node_id(this)?;
    let new_id = arg_node_id(args, 0)?;
    let old_id = arg_node_id(args, 1)?;

    if new_id != old_id {
        let mut doc = ctx.doc.borrow_mut();
        let mut mutr = doc.mutate();
        if mutr.node_has_parent(new_id) {
            mutr.remove_node(new_id);
        }
        mutr.insert_nodes_before(old_id, &[new_id]);
        mutr.remove_node(old_id);
    }
    Ok(args[1].clone())
}

fn remove(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let mut doc = ctx.doc.borrow_mut();
    let mut mutr = doc.mutate();
    if mutr.node_has_parent(node_id) {
        mutr.remove_node(node_id);
    }
    Ok(JsValue::undefined())
}

fn has_child_nodes(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let has_children = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .is_some_and(|node| !node.children.is_empty());
    Ok(JsValue::from(has_children))
}

fn contains(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let Some(mut current) = args.first().and_then(node_id_of_value) else {
        return Ok(JsValue::from(false));
    };

    let doc = ctx.doc.borrow();
    loop {
        if current == node_id {
            return Ok(JsValue::from(true));
        }
        match doc.get_node(current).and_then(|node| node.parent) {
            Some(parent_id) => current = parent_id,
            None => return Ok(JsValue::from(false)),
        }
    }
}

fn compare_document_position(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    const DISCONNECTED: u16 = 0x01;
    const PRECEDING: u16 = 0x02;
    const FOLLOWING: u16 = 0x04;
    const CONTAINS: u16 = 0x08;
    const CONTAINED_BY: u16 = 0x10;
    const IMPLEMENTATION_SPECIFIC: u16 = 0x20;

    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let other_id = args
        .first()
        .and_then(node_id_of_value)
        .ok_or_else(|| JsNativeError::typ().with_message("argument is not a Node"))?;
    if node_id == other_id {
        return Ok(JsValue::from(0));
    }

    let doc = ctx.doc.borrow();
    let path = |mut id: usize| {
        let mut path = Vec::new();
        loop {
            path.push(id);
            let Some(parent) = doc.get_node(id).and_then(|node| node.parent) else {
                break;
            };
            id = parent;
        }
        path.reverse();
        path
    };
    let node_path = path(node_id);
    let other_path = path(other_id);
    if node_path.first() != other_path.first() {
        let order = if node_id < other_id {
            FOLLOWING
        } else {
            PRECEDING
        };
        return Ok(JsValue::from(
            DISCONNECTED | IMPLEMENTATION_SPECIFIC | order,
        ));
    }

    let common = node_path
        .iter()
        .zip(&other_path)
        .take_while(|(left, right)| left == right)
        .count();
    if common == node_path.len() {
        return Ok(JsValue::from(FOLLOWING | CONTAINED_BY));
    }
    if common == other_path.len() {
        return Ok(JsValue::from(PRECEDING | CONTAINS));
    }
    let parent_id = node_path[common - 1];
    let parent = doc
        .get_node(parent_id)
        .expect("common ancestor is missing from document");
    let node_index = parent
        .children
        .iter()
        .position(|child| *child == node_path[common])
        .expect("node missing from common ancestor");
    let other_index = parent
        .children
        .iter()
        .position(|child| *child == other_path[common])
        .expect("other node missing from common ancestor");
    Ok(JsValue::from(if node_index < other_index {
        FOLLOWING
    } else {
        PRECEDING
    }))
}

fn clone_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let deep = args.first().map(JsValue::to_boolean).unwrap_or(false);

    enum CloneSrc {
        Element(blitz_dom::QualName, Vec<blitz_dom::Attribute>),
        Text(String),
        Other,
    }

    let new_node_id = {
        let mut doc = ctx.doc.borrow_mut();
        if deep {
            doc.mutate().deep_clone_node(node_id)
        } else {
            let src = match doc.get_node(node_id).map(|node| &node.data) {
                Some(NodeData::Element(data)) => {
                    CloneSrc::Element(data.name.clone(), data.attrs().to_vec())
                }
                Some(NodeData::Text(data)) => CloneSrc::Text(data.content.clone()),
                _ => CloneSrc::Other,
            };
            let mut mutr = doc.mutate();
            match src {
                CloneSrc::Element(name, attrs) => mutr.create_element(name, attrs),
                CloneSrc::Text(content) => mutr.create_text_node(&content),
                CloneSrc::Other => mutr.create_comment_node(),
            }
        }
    };

    Ok(node_wrapper(&ctx, new_node_id, context).into())
}

// === Event listeners ===

fn add_event_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let event_type = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(callback) = args.get(1).and_then(|value| value.as_object()) else {
        return Ok(JsValue::undefined());
    };
    if !callback.is_callable() {
        return Ok(JsValue::undefined());
    }

    // Parse options (bool `capture` or `{ capture, once }`)
    let mut capture = false;
    let mut once = false;
    match args.get(2) {
        Some(options) if options.is_object() => {
            let options = options.as_object().unwrap();
            capture = options
                .get(boa_engine::js_string!("capture"), context)?
                .to_boolean();
            once = options
                .get(boa_engine::js_string!("once"), context)?
                .to_boolean();
        }
        Some(options) => capture = options.to_boolean(),
        None => {}
    }

    let mut state = ctx.state.borrow_mut();
    let listeners = state
        .node_listeners
        .entry(node_id)
        .or_default()
        .entry(event_type)
        .or_default();

    // Duplicate listeners (same callback + capture flag) are ignored
    if !listeners
        .iter()
        .any(|l| JsObject::equals(&l.callback, &callback) && l.capture == capture)
    {
        listeners.push(Listener {
            callback,
            capture,
            once,
        });
    }

    Ok(JsValue::undefined())
}

fn remove_event_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let event_type = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(callback) = args.get(1).and_then(|value| value.as_object()) else {
        return Ok(JsValue::undefined());
    };

    let capture = match args.get(2) {
        Some(options) if options.is_object() => options
            .as_object()
            .unwrap()
            .get(boa_engine::js_string!("capture"), context)?
            .to_boolean(),
        Some(options) => options.to_boolean(),
        None => false,
    };

    let mut state = ctx.state.borrow_mut();
    if let Some(listeners) = state
        .node_listeners
        .get_mut(&node_id)
        .and_then(|map| map.get_mut(&event_type))
    {
        listeners.retain(|l| !(JsObject::equals(&l.callback, &callback) && l.capture == capture));
    }

    Ok(JsValue::undefined())
}

fn dispatch_event(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let target_id = this_node_id(this)?;
    let event = args
        .first()
        .and_then(JsValue::as_object)
        .filter(|event| event.downcast_ref::<EventRef>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("dispatchEvent requires an Event"))?;
    let event_type = to_rust_string(
        &event.get(boa_engine::js_string!("type"), context)?,
        context,
    )?;
    let bubbles = event
        .get(boa_engine::js_string!("bubbles"), context)?
        .to_boolean();
    let chain = ctx.doc.borrow().node_chain(target_id);
    let target: JsValue = node_wrapper(&ctx, target_id, context).into();
    define_value(&event, "target", target.clone(), context);
    define_value(&event, "srcElement", target, context);
    define_value(&event, "eventPhase", JsValue::from(2), context);
    let mut event_path: Vec<JsObject> = chain
        .iter()
        .map(|&node_id| node_wrapper(&ctx, node_id, context))
        .collect();
    event_path.push(context.global_object().clone());
    set_event_path(&event, event_path);
    let on_name = boa_engine::JsString::from(format!("on{event_type}"));

    'chain: for node_id in chain {
        let mut callbacks = Vec::new();
        {
            let mut state = ctx.state.borrow_mut();
            if let Some(listeners) = state
                .node_listeners
                .get_mut(&node_id)
                .and_then(|listeners| listeners.get_mut(&event_type))
            {
                callbacks.extend(listeners.iter().map(|listener| listener.callback.clone()));
                listeners.retain(|listener| !listener.once);
            }
        }
        if let Some(wrapper) = ctx.state.borrow().node_wrappers.get(&node_id).cloned() {
            if let Some(handler) = wrapper.get(on_name.clone(), context)?.as_object()
                && handler.is_callable()
            {
                callbacks.push(handler.clone());
            }
        }

        let current_target: JsValue = node_wrapper(&ctx, node_id, context).into();
        define_value(&event, "currentTarget", current_target.clone(), context);
        for callback in callbacks {
            callback.call(&current_target, &[event.clone().into()], context)?;
            if event
                .downcast_ref::<EventRef>()
                .is_some_and(|event| event.stopped_immediate.get())
            {
                break 'chain;
            }
        }
        let stopped = event
            .downcast_ref::<EventRef>()
            .is_some_and(|event| event.stopped.get());
        if !bubbles || stopped {
            break;
        }
    }

    define_value(&event, "currentTarget", JsValue::null(), context);
    define_value(&event, "eventPhase", JsValue::from(0), context);
    let prevented = event
        .downcast_ref::<EventRef>()
        .is_some_and(|event| event.prevented.get());
    Ok(JsValue::from(!prevented))
}

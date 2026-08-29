//! The `Node` (and `CharacterData`) prototypes: tree structure, tree mutation,
//! text content and event listener registration.

use blitz_dom::NodeId;
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
use crate::state::NodeListener;

const LISTENER_CALLBACKS_PROPERTY: &str = "__blitz_internal_listener_callbacks__";
const DETACHED_SUBTREE_PROPERTY: &str = "__blitz_internal_detached_subtree__";

fn node_is_connected(ctx: &crate::state::DomCtx, node_id: NodeId) -> bool {
    ctx.doc
        .borrow()
        .get_node(node_id)
        .is_some_and(|node| node.flags.is_in_document())
}

/// Make the JS wrapper the strong owner of callbacks. The native map is only
/// a dispatch index and stores weak handles, otherwise a callback closing over
/// its own node is rooted forever by Rust.
pub(crate) fn sync_node_listener_callbacks(
    ctx: &crate::state::DomCtx,
    node_id: NodeId,
    context: &mut Context,
) {
    let callbacks: Vec<JsValue> = ctx
        .state
        .borrow()
        .node_listeners
        .get(&node_id)
        .into_iter()
        .flat_map(|by_type| by_type.values())
        .flatten()
        .filter_map(|listener| listener.callback.upgrade())
        .map(Into::into)
        .collect();
    let wrapper = ctx
        .state
        .borrow()
        .node_wrappers
        .get(&node_id)
        .and_then(|wrapper| wrapper.upgrade());
    if let Some(wrapper) = wrapper {
        super::define_value(
            &wrapper,
            LISTENER_CALLBACKS_PROPERTY,
            JsArray::from_iter(callbacks, context).into(),
            context,
        );
        let connected = node_is_connected(ctx, node_id);
        let mut state = ctx.state.borrow_mut();
        if connected {
            state.listener_wrappers.insert(node_id, wrapper);
        } else {
            state.listener_wrappers.remove(&node_id);
        }
    }
}

/// Move listener ownership wholly into Boa before native removal. Every extant
/// wrapper in the subtree points at one wrapper group, so holding any node
/// preserves all descendant listeners while an unreachable subtree remains a
/// collectable JavaScript cycle.
pub(crate) fn unroot_detached_listener_subtree(
    ctx: &crate::state::DomCtx,
    node_id: NodeId,
    context: &mut Context,
) {
    let ids = {
        let doc = ctx.doc.borrow();
        let mut ids = Vec::new();
        let mut stack = vec![node_id];
        while let Some(id) = stack.pop() {
            ids.push(id);
            if let Some(node) = doc.get_node(id) {
                stack.extend(node.children.iter().copied());
            }
        }
        ids
    };
    let wrappers: Vec<JsObject> = {
        let state = ctx.state.borrow();
        ids.iter()
            .filter_map(|id| {
                state
                    .node_wrappers
                    .get(id)
                    .and_then(|wrapper| wrapper.upgrade())
            })
            .collect()
    };
    if !wrappers.is_empty() {
        let group = JsArray::from_iter(wrappers.iter().cloned().map(Into::into), context);
        for wrapper in &wrappers {
            super::define_value(
                wrapper,
                DETACHED_SUBTREE_PROPERTY,
                group.clone().into(),
                context,
            );
        }
    }
    let mut state = ctx.state.borrow_mut();
    for id in ids {
        state.listener_wrappers.remove(&id);
    }
}

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
    // The `ParentNode` trio. `append` is the one modern code actually reaches
    // for, and its absence is invisible in the worst way: the call throws a
    // TypeError from inside whatever effect or lifecycle hook made it, that
    // frame unwinds, and the surrounding UI is left half-built with nothing
    // logged at the DOM layer. A dialog that relocates its own subtree under
    // `body` to escape a containing block simply stays where it was, and its
    // full-screen backdrop paints inside that ancestor instead.
    define_method(proto, "append", 1, append, context);
    define_method(proto, "prepend", 1, prepend, context);
    define_method(proto, "replaceChildren", 0, replace_children, context);
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
        Some(NodeData::Document(_)) => 9,
        Some(NodeData::Element(_)) | Some(NodeData::AnonymousBlock(_)) => 1,
        Some(NodeData::Text(_)) => 3,
        Some(NodeData::Comment { .. }) => 8,
        // A shadow root is a DocumentFragment as far as the DOM is concerned.
        Some(NodeData::ShadowRoot(_)) => 11,
        None => 0,
    };
    Ok(JsValue::from(node_type))
}

fn node_name(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let name = match doc.get_node(node_id).map(|node| &node.data) {
        Some(NodeData::Document(_)) => "#document".to_string(),
        Some(NodeData::Element(data)) | Some(NodeData::AnonymousBlock(data)) => {
            data.name.local.to_uppercase()
        }
        Some(NodeData::Text(_)) => "#text".to_string(),
        Some(NodeData::Comment { .. }) => "#comment".to_string(),
        Some(NodeData::ShadowRoot(_)) => "#document-fragment".to_string(),
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

fn child_ids(ctx: &crate::state::DomCtx, node_id: NodeId) -> Vec<NodeId> {
    ctx.doc
        .borrow()
        .get_node(node_id)
        .map(|node| node.children.to_vec())
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

fn sibling(ctx: &crate::state::DomCtx, node_id: NodeId, offset: isize) -> Option<NodeId> {
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
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:textContent=");
    let node_id = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    let is_text_like = {
        let doc = ctx.doc.borrow();
        matches!(
            doc.get_node(node_id).map(|node| &node.data),
            Some(NodeData::Text(_)) | Some(NodeData::Comment { .. })
        )
    };

    if is_text_like {
        let mut doc = ctx.mutate_doc();
        let mut mutr = doc.mutate();
        mutr.set_node_text(node_id, &text);
    } else {
        // Detach (rather than drop) any existing children so that JS wrappers
        // referencing them remain valid.
        let children = ctx
            .doc
            .borrow()
            .get_node(node_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        for child_id in children {
            super::remove_and_free_node(&ctx, child_id, context);
        }
        if !text.is_empty() {
            let mut doc = ctx.mutate_doc();
            let mut mutr = doc.mutate();
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
        // A comment is CharacterData, so `comment.data` and `comment.nodeValue`
        // are its contents. This returned "" until the contents were reachable:
        // `cloneNode` copied them, so a script could clone a comment and read
        // back text that the original refused to report.
        Some(NodeData::Comment { contents }) => Ok(js_str(contents)),
        _ => Ok(JsValue::null()),
    }
}

fn set_node_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let mut doc = ctx.mutate_doc();
    doc.mutate().set_node_text(node_id, &text);
    Ok(JsValue::undefined())
}

// === Tree mutation ===

fn arg_node_id(args: &[JsValue], index: usize) -> JsResult<NodeId> {
    args.get(index).and_then(node_id_of_value).ok_or_else(|| {
        JsNativeError::typ()
            .with_message("argument is not a DOM node")
            .into()
    })
}

fn append_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:appendChild");
    let parent_id = this_node_id(this)?;
    let child_id = arg_node_id(args, 0)?;

    let mut doc = ctx.mutate_doc();
    let mut mutr = doc.mutate();
    // Detach from any current parent first. This also makes "move to end of
    // same parent" operations behave correctly.
    if mutr.node_has_parent(child_id) {
        mutr.remove_node(child_id);
    }
    mutr.append_children(parent_id, &[child_id]);
    drop(mutr);
    drop(doc);
    super::mark_node_reattached(&ctx, child_id);

    // An element created after its class was defined is upgraded on insertion,
    // which is also when `connectedCallback` is due. Without this only the
    // elements present when `define` ran would ever get the class.
    super::custom_elements::upgrade_if_defined(&ctx, child_id, context)?;

    Ok(args[0].clone())
}

/// Resolve one `append`/`prepend` argument to a node id, creating a text node
/// for a bare string as the spec requires.
fn arg_node_or_text(
    ctx: &crate::state::DomCtx,
    value: &JsValue,
    context: &mut Context,
) -> JsResult<NodeId> {
    if let Some(node_id) = node_id_of_value(value) {
        return Ok(node_id);
    }
    let text = value.to_string(context)?.to_std_string_lossy();
    let mut doc = ctx.mutate_doc();
    let id = doc.mutate().create_text_node(&text);
    Ok(id)
}

/// `ParentNode.append(...nodes)`: nodes or strings, appended in order.
fn append(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let profiling_ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&profiling_ctx, "dom:append");
    let parent_id = this_node_id(this)?;
    for arg in args {
        let ctx = dom_ctx(context)?;
        let child_id = arg_node_or_text(&ctx, arg, context)?;
        let ctx = dom_ctx(context)?;
        let mut doc = ctx.mutate_doc();
        let mut mutr = doc.mutate();
        // Same detach-first rule as `appendChild`: appending an attached node
        // is a move, not a second parent.
        if mutr.node_has_parent(child_id) {
            mutr.remove_node(child_id);
        }
        mutr.append_children(parent_id, &[child_id]);
        drop(mutr);
        drop(doc);
        super::mark_node_reattached(&ctx, child_id);
        super::custom_elements::upgrade_if_defined(&ctx, child_id, context)?;
    }
    Ok(JsValue::undefined())
}

/// `ParentNode.prepend(...nodes)`: the same, inserted before the first child.
fn prepend(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let profiling_ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&profiling_ctx, "dom:prepend");
    let parent_id = this_node_id(this)?;
    for (offset, arg) in args.iter().enumerate() {
        let ctx = dom_ctx(context)?;
        let child_id = arg_node_or_text(&ctx, arg, context)?;
        let ctx = dom_ctx(context)?;
        let mut doc = ctx.mutate_doc();
        let mut mutr = doc.mutate();
        if mutr.node_has_parent(child_id) {
            mutr.remove_node(child_id);
        }
        // Keep the arguments in their given order: the first goes to the front,
        // each later one directly after the one before it.
        let reference = mutr.child_ids(parent_id).get(offset).copied();
        match reference {
            Some(reference) => mutr.insert_nodes_before(reference, &[child_id]),
            None => mutr.append_children(parent_id, &[child_id]),
        }
        drop(mutr);
        drop(doc);
        super::mark_node_reattached(&ctx, child_id);
    }
    Ok(JsValue::undefined())
}

/// `ParentNode.replaceChildren(...nodes)`: empty the parent, then append.
fn replace_children(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let profiling_ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&profiling_ctx, "dom:replaceChildren");
    let parent_id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    let children = ctx
        .doc
        .borrow()
        .get_node(parent_id)
        .map(|node| node.children.clone())
        .unwrap_or_default();
    for child in children {
        super::remove_and_free_node(&ctx, child, context);
    }
    append(this, args, context)?;
    let _ = parent_id;
    Ok(JsValue::undefined())
}

fn insert_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:insertBefore");
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

    let mut doc = ctx.mutate_doc();
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
    drop(mutr);
    drop(doc);
    super::mark_node_reattached(&ctx, new_id);
    Ok(args[0].clone())
}

fn remove_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _parent_id = this_node_id(this)?;
    let child_id = arg_node_id(args, 0)?;

    // Detached when a wrapper still holds it, freed when none does. The node is
    // returned either way, as the spec requires, and holding that return value
    // is itself what keeps it alive.
    super::remove_and_free_node(&ctx, child_id, context);
    Ok(args[0].clone())
}

fn replace_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _parent_id = this_node_id(this)?;
    let new_id = arg_node_id(args, 0)?;
    let old_id = arg_node_id(args, 1)?;

    if new_id != old_id {
        let mut doc = ctx.mutate_doc();
        let mut mutr = doc.mutate();
        if mutr.node_has_parent(new_id) {
            mutr.remove_node(new_id);
        }
        mutr.insert_nodes_before(old_id, &[new_id]);
        drop(mutr);
        drop(doc);
        super::mark_node_reattached(&ctx, new_id);
        super::remove_and_free_node(&ctx, old_id, context);
    }
    Ok(args[1].clone())
}

fn remove(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let has_parent = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .is_some_and(|node| node.parent.is_some());
    if has_parent {
        super::remove_and_free_node(&ctx, node_id, context);
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
    let path = |mut id: NodeId| {
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

pub(super) fn clone_node(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let deep = args.first().map(JsValue::to_boolean).unwrap_or(false);

    enum CloneSrc {
        Element(blitz_dom::QualName, Vec<blitz_dom::Attribute>),
        Text(String),
        /// Comments carry their contents upstream, so a clone copies them
        /// rather than producing an empty comment.
        Comment(String),
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
                Some(NodeData::Comment { contents }) => CloneSrc::Comment(contents.clone()),
                _ => CloneSrc::Other,
            };
            let mut mutr = doc.mutate();
            match src {
                CloneSrc::Element(name, attrs) => mutr.create_element(name, attrs),
                CloneSrc::Text(content) => mutr.create_text_node(&content),
                CloneSrc::Comment(contents) => mutr.create_comment_node(&contents),
                CloneSrc::Other => mutr.create_comment_node(""),
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
    // Registration is a DOM operation like any other, and a mount attaching a
    // thousand listeners spent all of it outside the breakdown, so the profile
    // read as though it had touched fewer bindings than it had.
    //
    // Its counterparts are still uncounted: `removeEventListener`,
    // `dispatchEvent`, `removeChild`, `replaceChild`, `remove` and
    // `nodeValue=`. Worth knowing before reading a total here as the cost of
    // every DOM call this file serves.
    let _t = crate::script_stats::Timed::new(&ctx, "dom:addEventListener");
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
    if !listeners.iter().any(|listener| {
        listener
            .callback
            .upgrade()
            .is_some_and(|registered| JsObject::equals(&registered, &callback))
            && listener.capture == capture
    }) {
        listeners.push(NodeListener {
            callback: callback.downgrade(),
            capture,
            once,
        });
    }
    drop(state);
    sync_node_listener_callbacks(&ctx, node_id, context);

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
        listeners.retain(|listener| {
            !listener
                .callback
                .upgrade()
                .is_some_and(|registered| JsObject::equals(&registered, &callback))
                || listener.capture != capture
        });
    }
    drop(state);
    sync_node_listener_callbacks(&ctx, node_id, context);

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
                callbacks.extend(
                    listeners
                        .iter()
                        .filter_map(|listener| listener.callback.upgrade()),
                );
                listeners.retain(|listener| !listener.once);
            }
        }
        sync_node_listener_callbacks(&ctx, node_id, context);
        // Upgraded: the cache is weak, and a node whose wrapper has been
        // collected cannot be carrying an `on<event>` handler, because holding
        // one would have kept the wrapper alive.
        if let Some(wrapper) = ctx
            .state
            .borrow()
            .node_wrappers
            .get(&node_id)
            .and_then(boa_engine::object::WeakJsObject::upgrade)
        {
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

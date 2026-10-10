//! Live DOM traversal over native child lists.
//!
//! Cursors own their root and position wrappers in Boa's traced heap. The
//! removal index holds only weak cursor handles. No subtree snapshots or
//! JavaScript mutator wrappers are involved.

use std::cell::{Cell, RefCell};

use blitz_dom::NodeId;
use blitz_dom::node::NodeData;
use boa_engine::object::{JsObject, WeakJsObject};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace, js_string,
};
use boa_gc::GcRefCell;

use super::{dom_error, interface};
use crate::dom::{
    define_accessor, define_method, dom_ctx, node_id_of_value, node_wrapper, this_node_id,
};
use crate::state::DomCtx;

const ACCEPT: u16 = 1;
const REJECT: u16 = 2;
const SKIP: u16 = 3;

#[derive(Trace, Finalize, JsData)]
struct TraversalState {
    walker: JsObject,
    iterator: JsObject,
    #[unsafe_ignore_trace]
    iterators: RefCell<Vec<WeakJsObject>>,
}

#[derive(Clone, Trace, Finalize)]
struct Position {
    node: JsObject,
    #[unsafe_ignore_trace]
    before: bool,
}

#[derive(Trace, Finalize, JsData)]
struct Cursor {
    root: JsObject,
    position: GcRefCell<Position>,
    // A filter can remove the candidate currently being considered. Its
    // temporary position participates in the same pre-removing steps.
    pending: GcRefCell<Option<Position>>,
    filter: JsValue,
    #[unsafe_ignore_trace]
    what_to_show: u32,
    #[unsafe_ignore_trace]
    iterator: bool,
    #[unsafe_ignore_trace]
    active: Cell<bool>,
}

pub(crate) fn install(document: &JsObject, context: &mut Context) {
    let walker = JsObject::with_object_proto(context.intrinsics());
    let iterator = JsObject::with_object_proto(context.intrinsics());
    let filter = JsObject::with_object_proto(context.intrinsics());

    for proto in [&walker, &iterator] {
        define_accessor(proto, "root", Some(root), None, context);
        define_accessor(proto, "whatToShow", Some(what_to_show), None, context);
        define_accessor(proto, "filter", Some(filter_value), None, context);
        define_method(proto, "nextNode", 0, next_node, context);
        define_method(proto, "previousNode", 0, previous_node, context);
    }
    define_accessor(
        &walker,
        "currentNode",
        Some(current_node),
        Some(set_current_node),
        context,
    );
    define_method(&walker, "parentNode", 0, parent_node, context);
    define_method(&walker, "firstChild", 0, first_child, context);
    define_method(&walker, "lastChild", 0, last_child, context);
    define_method(&walker, "previousSibling", 0, previous_sibling, context);
    define_method(&walker, "nextSibling", 0, next_sibling, context);

    define_accessor(
        &iterator,
        "referenceNode",
        Some(current_node),
        None,
        context,
    );
    define_accessor(
        &iterator,
        "pointerBeforeReferenceNode",
        Some(pointer_before),
        None,
        context,
    );
    define_method(&iterator, "detach", 0, detach, context);

    interface("TreeWalker", &walker, context);
    interface("NodeIterator", &iterator, context);
    interface("NodeFilter", &filter, context);
    let constructor = context
        .global_object()
        .clone()
        .get(js_string!("NodeFilter"), context)
        .expect("NodeFilter constructor missing")
        .as_object()
        .expect("NodeFilter constructor is not an object");
    for (name, value) in [
        ("FILTER_ACCEPT", 1u32),
        ("FILTER_REJECT", 2),
        ("FILTER_SKIP", 3),
        ("SHOW_ALL", u32::MAX),
        ("SHOW_ELEMENT", 0x1),
        ("SHOW_ATTRIBUTE", 0x2),
        ("SHOW_TEXT", 0x4),
        ("SHOW_CDATA_SECTION", 0x8),
        ("SHOW_ENTITY_REFERENCE", 0x10),
        ("SHOW_ENTITY", 0x20),
        ("SHOW_PROCESSING_INSTRUCTION", 0x40),
        ("SHOW_COMMENT", 0x80),
        ("SHOW_DOCUMENT", 0x100),
        ("SHOW_DOCUMENT_TYPE", 0x200),
        ("SHOW_DOCUMENT_FRAGMENT", 0x400),
        ("SHOW_NOTATION", 0x800),
    ] {
        for object in [&constructor, &filter] {
            object
                .define_property_or_throw(
                    JsString::from(name),
                    PropertyDescriptor::builder()
                        .value(JsValue::from(value))
                        .writable(false)
                        .enumerable(true)
                        .configurable(false)
                        .build(),
                    context,
                )
                .expect("failed to define NodeFilter constant");
        }
    }
    context.insert_data(TraversalState {
        walker,
        iterator,
        iterators: RefCell::new(Vec::new()),
    });
    define_method(document, "createTreeWalker", 1, create_walker, context);
    define_method(document, "createNodeIterator", 1, create_iterator, context);
}

fn cursor(this: &JsValue) -> JsResult<JsObject> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid traversal receiver"))?;
    if object.downcast_ref::<Cursor>().is_none() {
        return Err(JsNativeError::typ()
            .with_message("Invalid traversal receiver")
            .into());
    }
    Ok(object)
}

fn id(object: &JsObject) -> NodeId {
    node_id_of_value(&object.clone().into()).expect("cursor contains a native node")
}

fn snapshot(object: &JsObject) -> (NodeId, Position, bool) {
    let data = object.downcast_ref::<Cursor>().expect("checked cursor");
    let position = data.position.borrow().clone();
    (id(&data.root), position, data.iterator)
}

fn create(
    this: &JsValue,
    args: &[JsValue],
    iterator: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    this_node_id(this)?;
    let root_id = args
        .first()
        .and_then(node_id_of_value)
        .ok_or_else(|| JsNativeError::typ().with_message("Traversal root must be a Node"))?;
    let what_to_show = match args.get(1) {
        Some(value) if !value.is_undefined() => value.to_u32(context)?,
        _ => u32::MAX,
    };
    let filter = match args.get(2) {
        Some(value) if !value.is_null_or_undefined() => {
            if value.as_object().is_none() {
                return Err(JsNativeError::typ()
                    .with_message("Filter must be an object")
                    .into());
            }
            value.clone()
        }
        _ => JsValue::null(),
    };
    let ctx = dom_ctx(context)?;
    let root = node_wrapper(&ctx, root_id, context);
    let proto = {
        let state = context
            .get_data::<TraversalState>()
            .expect("traversal not initialised");
        if iterator {
            state.iterator.clone()
        } else {
            state.walker.clone()
        }
    };
    let object = JsObject::from_proto_and_data(
        Some(proto),
        Cursor {
            root: root.clone(),
            position: GcRefCell::new(Position {
                node: root,
                before: true,
            }),
            pending: GcRefCell::new(None),
            filter,
            what_to_show,
            iterator,
            active: Cell::new(false),
        },
    );
    if iterator {
        let state = context
            .get_data::<TraversalState>()
            .expect("traversal not initialised");
        let mut iterators = state.iterators.borrow_mut();
        iterators.retain(|entry| entry.upgrade().is_some());
        iterators.push(object.downgrade());
    }
    Ok(object.into())
}

fn create_walker(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    create(this, args, false, context)
}

fn create_iterator(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    create(this, args, true, context)
}

fn root(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let data = object.downcast_ref::<Cursor>().expect("checked cursor");
    Ok(data.root.clone().into())
}

fn what_to_show(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let data = object.downcast_ref::<Cursor>().expect("checked cursor");
    Ok(data.what_to_show.into())
}

fn filter_value(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let data = object.downcast_ref::<Cursor>().expect("checked cursor");
    Ok(data.filter.clone())
}

fn current_node(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    Ok(snapshot(&object).1.node.clone().into())
}

fn set_current_node(this: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let node = args
        .first()
        .and_then(|value| node_id_of_value(value).and_then(|_| value.as_object()))
        .ok_or_else(|| JsNativeError::typ().with_message("currentNode must be a Node"))?;
    let data = object.downcast_ref::<Cursor>().expect("checked cursor");
    if data.iterator {
        return Err(JsNativeError::typ()
            .with_message("Expected a TreeWalker")
            .into());
    }
    data.position.borrow_mut().node = node;
    Ok(JsValue::undefined())
}

fn pointer_before(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    Ok(snapshot(&object).1.before.into())
}

fn detach(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    cursor(this)?;
    // Obsolete in the DOM standard; it does not disable traversal.
    Ok(JsValue::undefined())
}

fn parent(ctx: &DomCtx, node: NodeId) -> Option<NodeId> {
    ctx.doc.borrow().get_node(node)?.parent
}

fn child(ctx: &DomCtx, node: NodeId, forward: bool) -> Option<NodeId> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(node)?;
    let index = if forward {
        0
    } else {
        node.children.len().checked_sub(1)?
    };
    node.dom_child_at(index)
}

fn sibling(ctx: &DomCtx, node: NodeId, forward: bool) -> Option<NodeId> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(node)?;
    if forward {
        node.forward(1)
    } else {
        node.backward(1)
    }
    .map(|node| node.id)
}

fn following(ctx: &DomCtx, mut node: NodeId, root: NodeId) -> Option<NodeId> {
    while node != root {
        if let Some(next) = sibling(ctx, node, true) {
            return Some(next);
        }
        node = parent(ctx, node)?;
    }
    None
}

fn next(ctx: &DomCtx, node: NodeId, root: NodeId) -> Option<NodeId> {
    child(ctx, node, true).or_else(|| following(ctx, node, root))
}

fn previous(ctx: &DomCtx, node: NodeId, root: NodeId) -> Option<NodeId> {
    if node == root {
        return None;
    }
    if let Some(mut node) = sibling(ctx, node, false) {
        while let Some(last) = child(ctx, node, false) {
            node = last;
        }
        Some(node)
    } else {
        parent(ctx, node)
    }
}

fn inclusive_ancestor(ctx: &DomCtx, ancestor: NodeId, mut node: NodeId) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        let Some(next) = parent(ctx, node) else {
            return false;
        };
        node = next;
    }
}

fn accept(object: &JsObject, node: NodeId, ctx: &DomCtx, context: &mut Context) -> JsResult<u16> {
    let (active, mask, filter) = {
        let data = object.downcast_ref::<Cursor>().expect("checked cursor");
        (data.active.get(), data.what_to_show, data.filter.clone())
    };
    if active {
        return Err(dom_error(
            "InvalidStateError",
            "Recursive filter invocation",
            context,
        ));
    }
    let bit = {
        let doc = ctx.doc.borrow();
        let native = doc.get_node(node);
        match native.and_then(|node| node.markup.as_deref()) {
            Some(blitz_dom::node::MarkupNode::ProcessingInstruction { .. }) => 0x40,
            Some(blitz_dom::node::MarkupNode::Doctype { .. }) => 0x200,
            None => match native.map(|node| &node.data) {
                Some(NodeData::Element(_)) | Some(NodeData::AnonymousBlock(_)) => 0x1,
                Some(NodeData::Text(_)) => 0x4,
                Some(NodeData::Comment { .. }) => 0x80,
                Some(NodeData::Document(_)) => 0x100,
                Some(NodeData::DocumentFragment) | Some(NodeData::ShadowRoot(_)) => 0x400,
                None => 0,
            },
        }
    };
    if mask & bit == 0 {
        return Ok(SKIP);
    }
    let Some(filter) = filter.as_object() else {
        return Ok(ACCEPT);
    };
    object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .active
        .set(true);
    // All Boa and document borrows are released before user code is called.
    let result = (|| {
        let wrapper: JsValue = node_wrapper(ctx, node, context).into();
        let value = if filter.is_callable() {
            filter.call(&JsValue::undefined(), &[wrapper], context)?
        } else {
            let callback = filter
                .get(js_string!("acceptNode"), context)?
                .as_object()
                .filter(|callback| callback.is_callable())
                .ok_or_else(|| JsNativeError::typ().with_message("acceptNode is not callable"))?;
            callback.call(&filter.clone().into(), &[wrapper], context)?
        };
        // NodeFilter returns an unsigned short.
        Ok(value.to_u32(context)? as u16)
    })();
    object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .active
        .set(false);
    result
}

fn commit(object: &JsObject, node: NodeId, ctx: &DomCtx, context: &mut Context) -> JsValue {
    let wrapper = node_wrapper(ctx, node, context);
    object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .position
        .borrow_mut()
        .node = wrapper.clone();
    wrapper.into()
}

fn walker_next(object: &JsObject, ctx: &DomCtx, context: &mut Context) -> JsResult<JsValue> {
    let (root, position, _) = snapshot(object);
    let mut candidate = next(ctx, id(&position.node), root);
    while let Some(node) = candidate {
        match accept(object, node, ctx, context)? {
            ACCEPT => return Ok(commit(object, node, ctx, context)),
            REJECT => candidate = following(ctx, node, root),
            _ => candidate = next(ctx, node, root),
        }
    }
    Ok(JsValue::null())
}

fn walker_previous(object: &JsObject, ctx: &DomCtx, context: &mut Context) -> JsResult<JsValue> {
    let (root, position, _) = snapshot(object);
    let mut node = id(&position.node);
    while node != root {
        if let Some(previous) = sibling(ctx, node, false) {
            node = previous;
            let mut result = accept(object, node, ctx, context)?;
            while result != REJECT {
                let Some(last) = child(ctx, node, false) else {
                    break;
                };
                node = last;
                result = accept(object, node, ctx, context)?;
            }
            if result == ACCEPT {
                return Ok(commit(object, node, ctx, context));
            }
        } else {
            let Some(up) = parent(ctx, node) else {
                break;
            };
            node = up;
            if accept(object, node, ctx, context)? == ACCEPT {
                return Ok(commit(object, node, ctx, context));
            }
        }
    }
    Ok(JsValue::null())
}

fn next_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let ctx = dom_ctx(context)?;
    if snapshot(&object).2 {
        iterator_move(&object, true, &ctx, context)
    } else {
        walker_next(&object, &ctx, context)
    }
}

fn previous_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let ctx = dom_ctx(context)?;
    if snapshot(&object).2 {
        iterator_move(&object, false, &ctx, context)
    } else {
        walker_previous(&object, &ctx, context)
    }
}

fn parent_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let ctx = dom_ctx(context)?;
    let (root, position, _) = snapshot(&object);
    let mut node = id(&position.node);
    while node != root {
        let Some(up) = parent(&ctx, node) else {
            break;
        };
        node = up;
        if accept(&object, node, &ctx, context)? == ACCEPT {
            return Ok(commit(&object, node, &ctx, context));
        }
    }
    Ok(JsValue::null())
}

fn walker_child(this: &JsValue, forward: bool, context: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let ctx = dom_ctx(context)?;
    let (root, position, _) = snapshot(&object);
    let current = id(&position.node);
    let Some(mut node) = child(&ctx, current, forward) else {
        return Ok(JsValue::null());
    };
    loop {
        let result = accept(&object, node, &ctx, context)?;
        if result == ACCEPT {
            return Ok(commit(&object, node, &ctx, context));
        }
        if result == SKIP
            && let Some(down) = child(&ctx, node, forward)
        {
            node = down;
            continue;
        }
        loop {
            if let Some(next) = sibling(&ctx, node, forward) {
                node = next;
                break;
            }
            let Some(up) = parent(&ctx, node) else {
                return Ok(JsValue::null());
            };
            if up == current || up == root {
                return Ok(JsValue::null());
            }
            node = up;
        }
    }
}

fn first_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_child(this, true, context)
}

fn last_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_child(this, false, context)
}

fn walker_sibling(this: &JsValue, forward: bool, context: &mut Context) -> JsResult<JsValue> {
    let object = cursor(this)?;
    let ctx = dom_ctx(context)?;
    let (root, position, _) = snapshot(&object);
    let mut node = id(&position.node);
    if node == root {
        return Ok(JsValue::null());
    }
    loop {
        if let Some(next) = sibling(&ctx, node, forward) {
            node = next;
            loop {
                let result = accept(&object, node, &ctx, context)?;
                if result == ACCEPT {
                    return Ok(commit(&object, node, &ctx, context));
                }
                if result == REJECT {
                    break;
                }
                let Some(down) = child(&ctx, node, forward) else {
                    break;
                };
                node = down;
            }
        } else {
            let Some(up) = parent(&ctx, node) else {
                return Ok(JsValue::null());
            };
            node = up;
            if node == root || accept(&object, node, &ctx, context)? == ACCEPT {
                return Ok(JsValue::null());
            }
        }
    }
}

fn next_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_sibling(this, true, context)
}

fn previous_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_sibling(this, false, context)
}

fn iterator_move(
    object: &JsObject,
    forward: bool,
    ctx: &DomCtx,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (root, position, _) = snapshot(object);
    if object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .active
        .get()
    {
        return Err(dom_error(
            "InvalidStateError",
            "Recursive filter invocation",
            context,
        ));
    }
    *object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .pending
        .borrow_mut() = Some(position);
    let result = (|| {
        loop {
            let mut candidate = object
                .downcast_ref::<Cursor>()
                .expect("checked cursor")
                .pending
                .borrow()
                .clone()
                .expect("active iterator candidate");
            let mut node = id(&candidate.node);
            if candidate.before == forward {
                candidate.before = !forward;
            } else {
                let following = if forward {
                    next(ctx, node, root)
                } else {
                    previous(ctx, node, root)
                };
                let Some(following) = following else {
                    return Ok(JsValue::null());
                };
                node = following;
                candidate.node = node_wrapper(ctx, node, context);
                candidate.before = !forward;
            }
            *object
                .downcast_ref::<Cursor>()
                .expect("checked cursor")
                .pending
                .borrow_mut() = Some(candidate);
            // Both REJECT and SKIP leave descendants eligible for iterators.
            if accept(object, node, ctx, context)? == ACCEPT {
                let position = object
                    .downcast_ref::<Cursor>()
                    .expect("checked cursor")
                    .pending
                    .borrow()
                    .clone()
                    .expect("active iterator candidate");
                let value = position.node.clone().into();
                *object
                    .downcast_ref::<Cursor>()
                    .expect("checked cursor")
                    .position
                    .borrow_mut() = position;
                return Ok(value);
            }
        }
    })();
    *object
        .downcast_ref::<Cursor>()
        .expect("checked cursor")
        .pending
        .borrow_mut() = None;
    result
}

fn adjusted(
    ctx: &DomCtx,
    root: NodeId,
    removed: NodeId,
    position: &Position,
) -> Option<(NodeId, bool)> {
    let reference = id(&position.node);
    if removed == root
        || !inclusive_ancestor(ctx, root, removed)
        || !inclusive_ancestor(ctx, removed, reference)
    {
        return None;
    }
    if position.before
        && let Some(next) = following(ctx, removed, root)
    {
        return Some((next, true));
    }
    // The predecessor of the removed subtree is its parent's previous child
    // and that child's last descendant, or the parent itself.
    previous(ctx, removed, root).map(|previous| (previous, false))
}

/// Run before native parent/child links are changed.
///
/// The shared script detach helper calls this for removal and for DOMA moves.
/// TreeWalker positions deliberately remain on their original nodes.
pub(crate) fn pre_remove(ctx: &DomCtx, removed: NodeId, context: &mut Context) {
    let iterators = {
        let Some(state) = context.get_data::<TraversalState>() else {
            return;
        };
        let mut live = Vec::new();
        state.iterators.borrow_mut().retain(|entry| {
            if let Some(object) = entry.upgrade() {
                live.push(object);
                true
            } else {
                false
            }
        });
        live
    };
    for object in iterators {
        let (root, position, _) = snapshot(&object);
        let pending = object
            .downcast_ref::<Cursor>()
            .expect("registered iterator")
            .pending
            .borrow()
            .clone();
        let position_change = adjusted(ctx, root, removed, &position);
        let pending_change = pending
            .as_ref()
            .and_then(|position| adjusted(ctx, root, removed, position));
        if let Some((node, before)) = position_change {
            let node = node_wrapper(ctx, node, context);
            *object
                .downcast_ref::<Cursor>()
                .expect("registered iterator")
                .position
                .borrow_mut() = Position { node, before };
        }
        if let Some((node, before)) = pending_change {
            let node = node_wrapper(ctx, node, context);
            *object
                .downcast_ref::<Cursor>()
                .expect("registered iterator")
                .pending
                .borrow_mut() = Some(Position { node, before });
        }
    }
}

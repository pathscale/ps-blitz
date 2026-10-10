//! Range and Selection bindings. All traversal and boundary maintenance is
//! native; content mutations use the existing native DOM insertion path.

use std::cmp::Ordering;

use blitz_dom::{LiveRange, NodeData, NodeId, RangeBoundary, RangeBounds, RangeContent};
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsValue, NativeFunction, Trace,
};
use boa_gc::GcRefCell;

use super::{
    define_accessor, define_method, define_value, dom_ctx, interfaces, js_str, node_id_of_value,
    node_or_null, node_wrapper, this_node_id, to_rust_string,
};
use crate::state::DomCtx;

#[derive(Trace, Finalize, JsData)]
struct RangeRef {
    #[unsafe_ignore_trace]
    range: LiveRange,
    owners: GcRefCell<Vec<JsObject>>,
}

#[derive(Trace, Finalize, JsData)]
struct SelectionRef {
    range: GcRefCell<Option<JsObject>>,
}

#[derive(Trace, Finalize, JsData)]
struct SelectionState {
    object: JsObject,
}

fn error(name: &str, context: &mut Context) -> boa_engine::JsError {
    interfaces::exception(name, "Invalid range operation", context)
}

fn argument_node(args: &[JsValue], index: usize) -> JsResult<NodeId> {
    args.get(index)
        .and_then(node_id_of_value)
        .ok_or_else(|| JsNativeError::typ().with_message("Expected a Node").into())
}

fn offset(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<usize> {
    Ok(args
        .get(index)
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)? as usize)
}

fn range(this: &JsValue) -> JsResult<LiveRange> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<RangeRef>()
                .map(|data| data.range.clone())
        })
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid Range receiver")
                .into()
        })
}

fn boundary(ctx: &DomCtx, args: &[JsValue], context: &mut Context) -> JsResult<RangeBoundary> {
    let node = argument_node(args, 0)?;
    let offset = offset(args, 1, context)?;
    let length = ctx
        .doc
        .borrow()
        .range_node_length(node)
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    if offset > length {
        return Err(error("IndexSizeError", context));
    }
    Ok(RangeBoundary { node, offset })
}

fn refresh_owners(object: &JsObject, ctx: &DomCtx, context: &mut Context) {
    let Some(bounds) = object
        .downcast_ref::<RangeRef>()
        .map(|data| data.range.bounds())
    else {
        return;
    };
    let roots = {
        let doc = ctx.doc.borrow();
        [
            doc.range_root(bounds.start.node),
            doc.range_root(bounds.end.node),
        ]
    };
    let mut ids = vec![bounds.start.node, bounds.end.node, roots[0], roots[1]];
    ids.sort();
    ids.dedup();
    let owners = ids
        .into_iter()
        .map(|id| node_wrapper(ctx, id, context))
        .collect();
    if let Some(data) = object.downcast_ref::<RangeRef>() {
        *data.owners.borrow_mut() = owners;
    }
}

fn commit(
    this: &JsValue,
    bounds: RangeBounds,
    ctx: &DomCtx,
    context: &mut Context,
) -> JsResult<JsValue> {
    range(this)?.set_bounds(bounds);
    if let Some(object) = this.as_object() {
        refresh_owners(&object, ctx, context);
    }
    ctx.doc.borrow().shell_provider.request_redraw();
    Ok(JsValue::undefined())
}

fn wrap(bounds: RangeBounds, proto: JsObject, ctx: &DomCtx, context: &mut Context) -> JsObject {
    let live = ctx.doc.borrow_mut().create_live_range(bounds);
    let object = JsObject::from_proto_and_data(
        Some(proto),
        RangeRef {
            range: live,
            owners: GcRefCell::new(Vec::new()),
        },
    );
    refresh_owners(&object, ctx, context);
    object
}

pub(crate) fn create_range(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node = this_node_id(this)?;
    if !ctx
        .doc
        .borrow()
        .get_node(node)
        .is_some_and(|node| matches!(node.data, NodeData::Document(_)))
    {
        return Err(JsNativeError::typ()
            .with_message("Expected a Document")
            .into());
    }
    let point = RangeBoundary { node, offset: 0 };
    Ok(wrap(
        RangeBounds {
            start: point,
            end: point,
        },
        interfaces::prototype("Range", context),
        &ctx,
        context,
    )
    .into())
}

fn construct(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let proto = interfaces::construction_prototype(new_target, "Range", context)?;
    let ctx = dom_ctx(context)?;
    let point = RangeBoundary {
        node: ctx.doc.borrow().root_node().id,
        offset: 0,
    };
    Ok(wrap(
        RangeBounds {
            start: point,
            end: point,
        },
        proto,
        &ctx,
        context,
    )
    .into())
}

fn endpoint(this: &JsValue, end: bool, node: bool, context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let bounds = range(this)?.bounds();
    if let Some(object) = this.as_object() {
        refresh_owners(&object, &ctx, context);
    }
    let point = if end { bounds.end } else { bounds.start };
    Ok(if node {
        node_wrapper(&ctx, point.node, context).into()
    } else {
        JsValue::from(point.offset as f64)
    })
}

fn start_container(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    endpoint(this, false, true, context)
}

fn end_container(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    endpoint(this, true, true, context)
}

fn start_offset(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    endpoint(this, false, false, context)
}

fn end_offset(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    endpoint(this, true, false, context)
}

fn collapsed(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(range(this)?.bounds().collapsed().into())
}

fn common_ancestor(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = ctx
        .doc
        .borrow()
        .range_common_ancestor(range(this)?.bounds());
    Ok(node_or_null(&ctx, id, context))
}

fn set_endpoint(
    this: &JsValue,
    point: RangeBoundary,
    end: bool,
    ctx: &DomCtx,
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut bounds = range(this)?.bounds();
    let order = ctx.doc.borrow().compare_range_boundaries(
        if end { bounds.start } else { point },
        if end { point } else { bounds.end },
    );
    if end {
        bounds.end = point;
        if order.is_none() || order == Some(Ordering::Greater) {
            bounds.start = point;
        }
    } else {
        bounds.start = point;
        if order.is_none() || order == Some(Ordering::Greater) {
            bounds.end = point;
        }
    }
    commit(this, bounds, ctx, context)
}

fn set_start(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let point = boundary(&ctx, args, context)?;
    set_endpoint(this, point, false, &ctx, context)
}

fn set_end(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let point = boundary(&ctx, args, context)?;
    set_endpoint(this, point, true, &ctx, context)
}

fn adjacent_boundary(
    ctx: &DomCtx,
    id: NodeId,
    after: bool,
    context: &mut Context,
) -> JsResult<RangeBoundary> {
    let doc = ctx.doc.borrow();
    let parent = doc
        .get_node(id)
        .and_then(|node| node.parent)
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    let index = doc
        .get_node(parent)
        .and_then(|node| node.index_of_child(id))
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    Ok(RangeBoundary {
        node: parent,
        offset: index + usize::from(after),
    })
}

fn adjacent(
    this: &JsValue,
    args: &[JsValue],
    end: bool,
    after: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let point = adjacent_boundary(&ctx, argument_node(args, 0)?, after, context)?;
    set_endpoint(this, point, end, &ctx, context)
}

fn start_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    adjacent(this, args, false, false, context)
}

fn start_after(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    adjacent(this, args, false, true, context)
}

fn end_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    adjacent(this, args, true, false, context)
}

fn end_after(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    adjacent(this, args, true, true, context)
}

fn select_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = argument_node(args, 0)?;
    let start = adjacent_boundary(&ctx, id, false, context)?;
    let end = RangeBoundary {
        node: start.node,
        offset: start.offset + 1,
    };
    commit(this, RangeBounds { start, end }, &ctx, context)
}

fn select_contents(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node = argument_node(args, 0)?;
    let length = ctx
        .doc
        .borrow()
        .range_node_length(node)
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    commit(
        this,
        RangeBounds {
            start: RangeBoundary { node, offset: 0 },
            end: RangeBoundary {
                node,
                offset: length,
            },
        },
        &ctx,
        context,
    )
}

fn collapse(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let bounds = range(this)?.bounds();
    let point = if args.first().is_some_and(JsValue::to_boolean) {
        bounds.start
    } else {
        bounds.end
    };
    commit(
        this,
        RangeBounds {
            start: point,
            end: point,
        },
        &ctx,
        context,
    )
}

fn clone_range(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(wrap(
        range(this)?.bounds(),
        interfaces::prototype("Range", context),
        &ctx,
        context,
    )
    .into())
}

fn detach(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    range(this)?;
    Ok(JsValue::undefined())
}

fn to_string(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(js_str(
        &ctx.doc.borrow().range_string(range(this)?.bounds()),
    ))
}

fn compare(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let how = offset(args, 0, context)?;
    let ours = range(this)?.bounds();
    let theirs = range(args.get(1).unwrap_or(&JsValue::undefined()))?.bounds();
    let (a, b) = match how {
        0 => (ours.start, theirs.start),
        1 => (ours.end, theirs.start),
        2 => (ours.end, theirs.end),
        3 => (ours.start, theirs.end),
        _ => return Err(error("NotSupportedError", context)),
    };
    let order = ctx
        .doc
        .borrow()
        .compare_range_boundaries(a, b)
        .ok_or_else(|| error("WrongDocumentError", context))?;
    Ok(JsValue::from(match order {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentOperation {
    Clone,
    Extract,
    Delete,
}

fn clone_node(ctx: &DomCtx, id: NodeId, deep: bool, context: &mut Context) -> JsResult<NodeId> {
    let node: JsValue = node_wrapper(ctx, id, context).into();
    let cloned = super::node::clone_node(&node, &[deep.into()], context)?;
    this_node_id(&cloned)
}

fn apply_content(
    ctx: &DomCtx,
    content: &RangeContent,
    destination: Option<NodeId>,
    operation: ContentOperation,
    context: &mut Context,
) -> JsResult<()> {
    let mut output = None;
    match content {
        RangeContent::Full(id) => match operation {
            ContentOperation::Clone => output = Some(clone_node(ctx, *id, true, context)?),
            ContentOperation::Extract => output = Some(*id),
            ContentOperation::Delete => super::remove_and_free_node(ctx, *id, context),
        },
        RangeContent::Data(id, start, end) => {
            if operation != ContentOperation::Delete {
                let text = ctx
                    .doc
                    .borrow()
                    .range_character_data(*id)
                    .map(|text| blitz_dom::range::utf16_slice(text, *start, *end))
                    .unwrap_or_default();
                let new = clone_node(ctx, *id, false, context)?;
                ctx.doc.borrow_mut().mutate().set_node_text(new, &text);
                output = Some(new);
            }
            if operation != ContentOperation::Clone {
                let result =
                    ctx.mutate_doc()
                        .mutate()
                        .replace_character_data(*id, *start, end - start, "");
                result.map_err(|name| error(name, context))?;
            }
        }
        RangeContent::Partial(id, children) => {
            let new = if operation == ContentOperation::Delete {
                None
            } else {
                Some(clone_node(ctx, *id, false, context)?)
            };
            for child in children {
                apply_content(ctx, child, new, operation, context)?;
            }
            output = new;
        }
    }
    if let (Some(destination), Some(output)) = (destination, output) {
        super::doma::tree::insert(ctx, destination, &[output], None, &[], context)?;
    }
    Ok(())
}

fn contents(
    this: &JsValue,
    operation: ContentOperation,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let live = range(this)?;
    let bounds = live.bounds();
    let plan = ctx.doc.borrow().range_contents(bounds);
    let fragment = if operation == ContentOperation::Delete {
        None
    } else {
        let id = ctx.doc.borrow_mut().mutate().create_document_fragment();
        // This wrapper is part of the ownership group built below.
        Some(node_wrapper(&ctx, id, context))
    };
    let destination = fragment
        .as_ref()
        .and_then(|object| node_id_of_value(&object.clone().into()));
    let collapse_point = if operation != ContentOperation::Clone && !bounds.collapsed() {
        let point = ctx.doc.borrow().range_collapse_after_deletion(bounds);
        Some(ctx.doc.borrow_mut().create_live_range(RangeBounds {
            start: point,
            end: point,
        }))
    } else {
        None
    };
    for content in &plan {
        apply_content(&ctx, content, destination, operation, context)?;
    }
    if let Some(collapse_point) = collapse_point {
        let point = collapse_point.bounds().start;
        commit(
            this,
            RangeBounds {
                start: point,
                end: point,
            },
            &ctx,
            context,
        )?;
    }
    if let Some(id) = destination {
        super::node::unroot_detached_listener_subtree(&ctx, id, context);
    }
    Ok(fragment.map_or_else(JsValue::undefined, Into::into))
}

fn clone_contents(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    contents(this, ContentOperation::Clone, context)
}

fn extract_contents(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    contents(this, ContentOperation::Extract, context)
}

fn delete_contents(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    contents(this, ContentOperation::Delete, context)
}

fn insert_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let live = range(this)?;
    let bounds = live.bounds();
    let id = argument_node(args, 0)?;
    let (parent, mut reference, split, expanded) = {
        let doc = ctx.doc.borrow();
        let start = doc
            .get_node(bounds.start.node)
            .ok_or_else(|| error("InvalidNodeTypeError", context))?;
        let split = matches!(start.data, NodeData::Text(_));
        if matches!(start.data, NodeData::Comment { .. }) || id == bounds.start.node {
            return Err(error("HierarchyRequestError", context));
        }
        let (parent, reference) = if split {
            (
                start
                    .parent
                    .ok_or_else(|| error("HierarchyRequestError", context))?,
                Some(start.id),
            )
        } else {
            (start.id, start.children.get(bounds.start.offset).copied())
        };
        let inserted = doc
            .get_node(id)
            .ok_or_else(|| error("InvalidNodeTypeError", context))?;
        let expanded = if matches!(inserted.data, NodeData::DocumentFragment) {
            inserted.children.to_vec()
        } else {
            vec![id]
        };
        (parent, reference, split, expanded)
    };
    super::doma::tree::validate(&ctx, parent, &[id], &expanded, reference, &[], context)?;
    if split {
        let result = ctx
            .mutate_doc()
            .mutate()
            .split_text(bounds.start.node, bounds.start.offset);
        reference = Some(result.map_err(|name| error(name, context))?);
    }
    super::doma::tree::insert(&ctx, parent, &[id], reference, &[], context)?;
    if bounds.collapsed() {
        let end = {
            let doc = ctx.doc.borrow();
            let parent_node = doc.get_node(parent).expect("insertion parent");
            let offset = if let Some(last) = expanded.last() {
                parent_node.index_of_child(*last).expect("inserted child") + 1
            } else {
                reference
                    .and_then(|id| parent_node.index_of_child(id))
                    .unwrap_or(parent_node.children.len())
            };
            RangeBoundary {
                node: parent,
                offset,
            }
        };
        commit(
            this,
            RangeBounds {
                start: live.bounds().start,
                end,
            },
            &ctx,
            context,
        )?;
    }
    Ok(JsValue::undefined())
}

fn surround(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = argument_node(args, 0)?;
    let bounds = range(this)?.bounds();
    let invalid_partial = {
        fn partial(
            doc: &blitz_dom::BaseDocument,
            item: &RangeContent,
            bounds: RangeBounds,
        ) -> bool {
            match item {
                RangeContent::Partial(_, _) => true,
                RangeContent::Data(id, _, _) => {
                    bounds.start.node != bounds.end.node
                        && !doc.get_node(*id).is_some_and(|node| node.is_text_node())
                }
                RangeContent::Full(_) => false,
            }
        }
        let doc = ctx.doc.borrow();
        if !doc.get_node(id).is_some_and(|node| node.is_element()) {
            return Err(error("InvalidNodeTypeError", context));
        }
        if doc.range_contains(id, bounds.start.node) || doc.range_contains(id, bounds.end.node) {
            return Err(error("HierarchyRequestError", context));
        }
        doc.range_contents(bounds)
            .iter()
            .any(|item| partial(&doc, item, bounds))
    };
    if invalid_partial {
        return Err(error("InvalidStateError", context));
    }
    // Validate the insertion location before extracting anything.
    {
        let doc = ctx.doc.borrow();
        let point = doc.range_collapse_after_deletion(bounds);
        let node = doc.get_node(point.node).expect("range boundary");
        let (parent, reference) = if node.is_text_node() {
            (
                node.parent
                    .ok_or_else(|| error("HierarchyRequestError", context))?,
                Some(node.id),
            )
        } else {
            (node.id, node.children.get(point.offset).copied())
        };
        drop(doc);
        super::doma::tree::validate(&ctx, parent, &[id], &[id], reference, &[], context)?;
    }
    let fragment = extract_contents(this, &[], context)?;
    let children = ctx
        .doc
        .borrow()
        .get_node(id)
        .map(|node| node.children.to_vec())
        .unwrap_or_default();
    for child in children {
        super::remove_and_free_node(&ctx, child, context);
    }
    insert_node(this, args, context)?;
    super::doma::tree::insert(&ctx, id, &[this_node_id(&fragment)?], None, &[], context)?;
    select_node(this, args, context)
}

fn rect_object(rect: blitz_dom::kurbo::Rect, context: &mut Context) -> JsResult<JsValue> {
    let constructor = interfaces::constructor("DOMRect", context);
    Ok(constructor
        .construct(
            &[
                rect.x0.into(),
                rect.y0.into(),
                rect.width().into(),
                rect.height().into(),
            ],
            None,
            context,
        )?
        .into())
}

fn client_rects(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let rects = ctx.doc.borrow().range_client_rects(range(this)?.bounds());
    let mut values = Vec::with_capacity(rects.len());
    for rect in rects {
        values.push(rect_object(rect, context)?);
    }
    Ok(JsArray::from_iter(values, context).into())
}

fn bounding_rect(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let rects = ctx.doc.borrow().range_client_rects(range(this)?.bounds());
    let rect = rects
        .into_iter()
        .reduce(|a, b| a.union(b))
        .unwrap_or_else(|| blitz_dom::kurbo::Rect::new(0.0, 0.0, 0.0, 0.0));
    rect_object(rect, context)
}

fn character_length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let length = ctx
        .doc
        .borrow()
        .range_character_data(id)
        .map(|text| text.encode_utf16().count())
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    Ok((length as f64).into())
}

fn substring_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let start = offset(args, 0, context)?;
    let count = offset(args, 1, context)?;
    let doc = ctx.doc.borrow();
    let text = doc
        .range_character_data(id)
        .ok_or_else(|| error("InvalidNodeTypeError", context))?;
    let length = text.encode_utf16().count();
    if start > length {
        return Err(error("IndexSizeError", context));
    }
    Ok(js_str(&blitz_dom::range::utf16_slice(
        text,
        start,
        start + count.min(length - start),
    )))
}

fn replace_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let start = offset(args, 0, context)?;
    let count = offset(args, 1, context)?;
    let value = to_rust_string(args.get(2).unwrap_or(&JsValue::undefined()), context)?;
    let result = ctx
        .mutate_doc()
        .mutate()
        .replace_character_data(id, start, count, &value);
    result.map_err(|name| error(name, context))?;
    Ok(JsValue::undefined())
}

fn insert_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    replace_data(
        this,
        &[
            args.first().cloned().unwrap_or_default(),
            0.into(),
            args.get(1).cloned().unwrap_or_default(),
        ],
        context,
    )
}

fn delete_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    replace_data(
        this,
        &[
            args.first().cloned().unwrap_or_default(),
            args.get(1).cloned().unwrap_or_default(),
            js_str(""),
        ],
        context,
    )
}

fn append_data(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let length = character_length(this, &[], context)?;
    replace_data(
        this,
        &[length, 0.into(), args.first().cloned().unwrap_or_default()],
        context,
    )
}

fn split_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let start = offset(args, 0, context)?;
    let result = ctx.mutate_doc().mutate().split_text(id, start);
    let new = result.map_err(|name| error(name, context))?;
    super::mark_node_reattached(&ctx, new);
    Ok(node_wrapper(&ctx, new, context).into())
}

fn selection_object(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|object| object.downcast_ref::<SelectionRef>().is_some())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid Selection receiver")
                .into()
        })
}

fn current_selection(
    object: &JsObject,
    ctx: &DomCtx,
    context: &mut Context,
) -> JsResult<Option<JsObject>> {
    let cached = object
        .downcast_ref::<SelectionRef>()
        .expect("Selection")
        .range
        .borrow()
        .clone();
    let native = ctx.doc.borrow().dom_selection.clone();
    if let (Some(cached), Some(native)) = (&cached, &native)
        && range(&cached.clone().into())?.same_range(native)
    {
        return Ok(Some(cached.clone()));
    }
    *object
        .downcast_ref::<SelectionRef>()
        .expect("Selection")
        .range
        .borrow_mut() = None;
    if native.is_some() {
        return Ok(None);
    }
    // A pointer selection is represented in inline layout coordinates. Adopt
    // it only when script asks for a Selection, preserving the existing paint.
    let bounds = {
        let doc = ctx.doc.borrow();
        let ranges = doc.get_text_selection_ranges();
        ranges.first().zip(ranges.last()).and_then(|(first, last)| {
            Some(RangeBounds {
                start: doc.range_boundary_from_layout(first.0, first.1, false)?,
                end: doc.range_boundary_from_layout(last.0, last.2, true)?,
            })
        })
    };
    let Some(bounds) = bounds else {
        return Ok(None);
    };
    let wrapper = wrap(
        bounds,
        interfaces::prototype("Range", context),
        ctx,
        context,
    );
    ctx.doc.borrow_mut().dom_selection = Some(range(&wrapper.clone().into())?);
    *object
        .downcast_ref::<SelectionRef>()
        .expect("Selection")
        .range
        .borrow_mut() = Some(wrapper.clone());
    Ok(Some(wrapper))
}

pub(crate) fn document_selection(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    if id != ctx.doc.borrow().root_node().id {
        return Ok(JsValue::null());
    }
    window_selection(this, &[], context)
}

fn window_selection(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(context
        .get_data::<SelectionState>()
        .expect("Selection not initialised")
        .object
        .clone()
        .into())
}

fn selection_count(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    Ok(i32::from(current_selection(&object, &ctx, context)?.is_some()).into())
}

fn selection_collapsed(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    let selected = current_selection(&object, &ctx, context)?;
    Ok(match selected {
        Some(object) => range(&object.into())?.bounds().collapsed(),
        None => true,
    }
    .into())
}

fn selection_endpoint(
    this: &JsValue,
    end: bool,
    node: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    match current_selection(&object, &ctx, context)? {
        Some(object) => endpoint(&object.into(), end, node, context),
        None => Ok(if node { JsValue::null() } else { 0.into() }),
    }
}

fn anchor_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    selection_endpoint(this, false, true, context)
}

fn anchor_offset(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    selection_endpoint(this, false, false, context)
}

fn focus_node(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    selection_endpoint(this, true, true, context)
}

fn focus_offset(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    selection_endpoint(this, true, false, context)
}

fn selection_string(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    match current_selection(&object, &ctx, context)? {
        Some(object) => to_string(&object.into(), &[], context),
        None => Ok(js_str("")),
    }
}

fn get_range(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    let index = offset(args, 0, context)?;
    let selected = current_selection(&object, &ctx, context)?;
    if index != 0 || selected.is_none() {
        return Err(error("IndexSizeError", context));
    }
    Ok(selected.expect("checked range").into())
}

fn remove_all(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    *object
        .downcast_ref::<SelectionRef>()
        .expect("Selection")
        .range
        .borrow_mut() = None;
    let mut doc = ctx.doc.borrow_mut();
    doc.clear_text_selection();
    doc.shell_provider.request_redraw();
    Ok(JsValue::undefined())
}

fn add_range(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let selected = &args.first().cloned().unwrap_or_default();
    let live = range(selected)?;
    let ctx = dom_ctx(context)?;
    if current_selection(&object, &ctx, context)?.is_some() {
        return Ok(JsValue::undefined());
    }
    let in_document = {
        let doc = ctx.doc.borrow();
        doc.range_root(live.bounds().start.node) == doc.root_node().id
    };
    if !in_document {
        return Ok(JsValue::undefined());
    }
    {
        let mut doc = ctx.doc.borrow_mut();
        doc.clear_text_selection();
        doc.dom_selection = Some(live);
        doc.shell_provider.request_redraw();
    }
    *object
        .downcast_ref::<SelectionRef>()
        .expect("Selection")
        .range
        .borrow_mut() = selected.as_object();
    Ok(JsValue::undefined())
}

fn selection_collapse(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    selection_object(this)?;
    if args.first().is_some_and(JsValue::is_null) {
        return remove_all(this, &[], context);
    }
    let ctx = dom_ctx(context)?;
    let point = boundary(&ctx, args, context)?;
    if ctx.doc.borrow().range_root(point.node) != ctx.doc.borrow().root_node().id {
        return Ok(JsValue::undefined());
    }
    let wrapper = wrap(
        RangeBounds {
            start: point,
            end: point,
        },
        interfaces::prototype("Range", context),
        &ctx,
        context,
    );
    remove_all(this, &[], context)?;
    add_range(this, &[wrapper.into()], context)
}

fn collapse_selection_end(this: &JsValue, end: bool, context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    let selected = current_selection(&object, &ctx, context)?
        .ok_or_else(|| error("InvalidStateError", context))?;
    let bounds = range(&selected.into())?.bounds();
    let point = if end { bounds.end } else { bounds.start };
    let node = node_wrapper(&ctx, point.node, context);
    selection_collapse(this, &[node.into(), (point.offset as f64).into()], context)
}

fn collapse_to_start(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    collapse_selection_end(this, false, context)
}

fn collapse_to_end(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    collapse_selection_end(this, true, context)
}

fn selection_delete(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = selection_object(this)?;
    let ctx = dom_ctx(context)?;
    if let Some(object) = current_selection(&object, &ctx, context)? {
        delete_contents(&object.into(), &[], context)?;
    }
    Ok(JsValue::undefined())
}

pub(crate) fn init(context: &mut Context) {
    let proto = JsObject::with_object_proto(context.intrinsics());
    for (name, getter) in [
        ("startContainer", start_container as super::NativeFnPtr),
        ("endContainer", end_container),
        ("startOffset", start_offset),
        ("endOffset", end_offset),
        ("collapsed", collapsed),
        ("commonAncestorContainer", common_ancestor),
    ] {
        define_accessor(&proto, name, Some(getter), None, context);
    }
    for (name, length, method) in [
        ("setStart", 2, set_start as super::NativeFnPtr),
        ("setEnd", 2, set_end),
        ("setStartBefore", 1, start_before),
        ("setStartAfter", 1, start_after),
        ("setEndBefore", 1, end_before),
        ("setEndAfter", 1, end_after),
        ("selectNode", 1, select_node),
        ("selectNodeContents", 1, select_contents),
        ("collapse", 0, collapse),
        ("cloneRange", 0, clone_range),
        ("detach", 0, detach),
        ("toString", 0, to_string),
        ("compareBoundaryPoints", 2, compare),
        ("cloneContents", 0, clone_contents),
        ("extractContents", 0, extract_contents),
        ("deleteContents", 0, delete_contents),
        ("insertNode", 1, insert_node),
        ("surroundContents", 1, surround),
        ("getClientRects", 0, client_rects),
        ("getBoundingClientRect", 0, bounding_rect),
    ] {
        define_method(&proto, name, length, method, context);
    }
    interfaces::register(
        "Range",
        None,
        proto.clone(),
        0,
        NativeFunction::from_fn_ptr(construct),
        context,
    );
    let constructor = interfaces::constructor("Range", context);
    for (name, value) in [
        ("START_TO_START", 0),
        ("START_TO_END", 1),
        ("END_TO_END", 2),
        ("END_TO_START", 3),
    ] {
        define_value(&proto, name, value.into(), context);
        define_value(&constructor, name, value.into(), context);
    }

    let character = interfaces::prototype("CharacterData", context);
    define_accessor(&character, "length", Some(character_length), None, context);
    for (name, length, method) in [
        ("substringData", 2, substring_data as super::NativeFnPtr),
        ("appendData", 1, append_data),
        ("insertData", 2, insert_data),
        ("deleteData", 2, delete_data),
        ("replaceData", 3, replace_data),
    ] {
        define_method(&character, name, length, method, context);
    }
    define_method(
        &interfaces::prototype("Text", context),
        "splitText",
        1,
        split_text,
        context,
    );

    let proto = JsObject::with_object_proto(context.intrinsics());
    for (name, getter) in [
        ("rangeCount", selection_count as super::NativeFnPtr),
        ("isCollapsed", selection_collapsed),
        ("anchorNode", anchor_node),
        ("anchorOffset", anchor_offset),
        ("focusNode", focus_node),
        ("focusOffset", focus_offset),
    ] {
        define_accessor(&proto, name, Some(getter), None, context);
    }
    for (name, length, method) in [
        ("toString", 0, selection_string as super::NativeFnPtr),
        ("getRangeAt", 1, get_range),
        ("addRange", 1, add_range),
        ("removeAllRanges", 0, remove_all),
        ("empty", 0, remove_all),
        ("collapse", 1, selection_collapse),
        ("setPosition", 1, selection_collapse),
        ("collapseToStart", 0, collapse_to_start),
        ("collapseToEnd", 0, collapse_to_end),
        ("deleteFromDocument", 0, selection_delete),
    ] {
        define_method(&proto, name, length, method, context);
    }
    interfaces::register(
        "Selection",
        None,
        proto.clone(),
        0,
        NativeFunction::from_fn_ptr(interfaces::illegal),
        context,
    );
    let object = JsObject::from_proto_and_data(
        Some(proto),
        SelectionRef {
            range: GcRefCell::new(None),
        },
    );
    context.insert_data(SelectionState { object });
    let global = context.global_object().clone();
    define_method(&global, "getSelection", 0, window_selection, context);
}

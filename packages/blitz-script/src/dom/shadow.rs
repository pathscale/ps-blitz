//! ShadowRoot and slot bindings. Distribution remains in blitz-dom.

use blitz_dom::BaseDocument;
use blitz_dom::node::ShadowRootMode;
use blitz_html::DocumentHtmlParser;
use boa_engine::object::{JsObject, builtins::JsArray};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsValue, NativeFunction, Trace, js_string,
};

use super::{
    define_accessor, define_method, dom_ctx, element, js_str, node_or_null, node_wrapper,
    this_node_id, to_rust_string,
};
use crate::state::DomCtx;

#[derive(Clone, Trace, Finalize, JsData)]
struct ShadowProtos {
    root: JsObject,
}

pub(crate) fn root_proto(context: &Context) -> JsObject {
    context
        .get_data::<ShadowProtos>()
        .expect("shadow prototypes missing")
        .root
        .clone()
}

pub(crate) fn init(ctx: &DomCtx, context: &mut Context) {
    let (node, element, document) = {
        let state = ctx.state.borrow();
        let protos = state.protos();
        (
            protos.node.clone(),
            protos.element.clone(),
            protos.document.clone(),
        )
    };

    let root = JsObject::with_object_proto(context.intrinsics());
    root.set_prototype(Some(super::interfaces::prototype(
        "DocumentFragment",
        context,
    )));
    define_accessor(&root, "host", Some(host), None, context);
    define_accessor(&root, "mode", Some(mode), None, context);
    define_accessor(&root, "clonable", Some(clonable), None, context);
    define_accessor(&root, "serializable", Some(serializable), None, context);
    define_accessor(&root, "delegatesFocus", Some(delegates_focus), None, context);
    define_method(&root, "setHTMLUnsafe", 1, set_html_unsafe, context);
    define_accessor(
        &root,
        "innerHTML",
        Some(element::get_inner_html),
        Some(element::set_inner_html),
        context,
    );
    define_method(&root, "querySelector", 1, element::query_selector, context);
    define_method(
        &root,
        "querySelectorAll",
        1,
        element::query_selector_all,
        context,
    );
    define_method(&root, "getElementById", 1, get_element_by_id, context);

    define_accessor(&node, "assignedSlot", Some(assigned_slot), None, context);
    define_method(&element, "attachShadow", 1, attach_shadow, context);
    define_method(&element, "setHTMLUnsafe", 1, set_html_unsafe, context);
    define_accessor(&element, "shadowRoot", Some(shadow_root), None, context);
    define_accessor(&element, "slot", Some(get_slot), Some(set_slot), context);
    define_method(&element, "assignedNodes", 0, assigned_nodes, context);
    define_method(&element, "assignedElements", 0, assigned_elements, context);

    define_method(
        &super::interfaces::constructor("Document", context),
        "parseHTMLUnsafe",
        1,
        parse_html_unsafe,
        context,
    );

    context.insert_data(ShadowProtos { root: root.clone() });
    super::interfaces::register(
        "ShadowRoot",
        Some("DocumentFragment"),
        root.clone(),
        0,
        NativeFunction::from_fn_ptr(illegal_constructor),
        context,
    );
    super::sheets::init(&document, &root, context);
}

fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("Illegal ShadowRoot constructor")
        .into())
}

fn attach_shadow(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let host_id = this_node_id(this)?;
    let options = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("attachShadow requires options"))?;
    let mode = to_rust_string(&options.get(js_string!("mode"), context)?, context)?;
    let mode = match mode.as_str() {
        "open" => ShadowRootMode::Open,
        "closed" => ShadowRootMode::Closed,
        _ => {
            return Err(JsNativeError::typ()
                .with_message("Invalid ShadowRoot mode")
                .into());
        }
    };
    let (allowed, shadow_id) = {
        let doc = ctx.doc.borrow();
        let Some(host) = doc
            .get_node(host_id)
            .and_then(|node| node.element_data())
        else {
            return Err(JsNativeError::typ()
                .with_message("attachShadow requires an element")
                .into());
        };
        (
            BaseDocument::element_can_be_shadow_host(host),
            host.shadow_root,
        )
    };
    if !allowed {
        return Err(super::doma::dom_error(
            "NotSupportedError",
            "attachShadow",
            context,
        ));
    }
    let existing = {
        let doc = ctx.doc.borrow();
        shadow_id.and_then(|root_id| {
            let root = doc.get_node(root_id)?.shadow_root_data()?;
            Some((root_id, root.declarative, root.mode))
        })
    };
    if let Some((root_id, declarative, existing_mode)) = existing {
        // A declarative root is reusable when the requested mode matches.
        // The same root is emptied and returned. Any other existing root,
        // including a different mode, is NotSupportedError.
        if declarative && existing_mode == mode {
            ctx.mutate_doc()
                .mutate()
                .remove_and_drop_all_children(root_id);
            return Ok(node_wrapper(&ctx, root_id, context).into());
        }
        return Err(super::doma::dom_error(
            "NotSupportedError",
            "attachShadow",
            context,
        ));
    }
    let root_id = ctx.mutate_doc().mutate().attach_shadow(host_id, mode);
    Ok(node_wrapper(&ctx, root_id, context).into())
}

fn shadow_bool(
    this: &JsValue,
    context: &mut Context,
    read: fn(&blitz_dom::node::ShadowRootData) -> bool,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.shadow_root_data())
        .map(read)
        .ok_or_else(|| JsNativeError::typ().with_message("ShadowRoot receiver required"))?;
    Ok(JsValue::from(value))
}

fn clonable(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    shadow_bool(this, context, |root| root.clonable)
}

fn serializable(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    shadow_bool(this, context, |root| root.serializable)
}

fn delegates_focus(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    shadow_bool(this, context, |root| root.delegates_focus)
}

fn set_html_unsafe(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let html = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let target = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .and_then(|node| node.element_data())
        .and_then(|element| element.template_contents)
        .unwrap_or(node_id);
    let children = ctx
        .doc
        .borrow()
        .get_node(target)
        .map(|node| node.children.to_vec())
        .unwrap_or_default();
    for child_id in children {
        super::remove_and_free_node(&ctx, child_id, context);
    }
    ctx.mutate_doc().mutate().set_html_unsafe(node_id, &html);
    Ok(JsValue::undefined())
}

fn parse_html_unsafe(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let source = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let document_id = {
        let mut doc = ctx.doc.borrow_mut();
        let mut mutr = doc.mutate();
        let document_id = mutr.create_document("text/html");
        DocumentHtmlParser::parse_html_unsafe_into_mutator(&mut mutr, document_id, &source);
        document_id
    };
    ctx.state.borrow_mut().detached_nodes.push(document_id);
    Ok(node_wrapper(&ctx, document_id, context).into())
}

fn shadow_root(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let host_id = this_node_id(this)?;
    let root_id = {
        let doc = ctx.doc.borrow();
        doc.shadow_root_id(host_id).filter(|&id| {
            doc.get_node(id)
                .and_then(|node| node.shadow_root_data())
                .is_some_and(|root| root.mode == ShadowRootMode::Open)
        })
    };
    Ok(node_or_null(&ctx, root_id, context))
}

fn host(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let host_id = ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.shadow_root_data())
        .map(|root| root.host)
        .ok_or_else(|| JsNativeError::typ().with_message("ShadowRoot receiver required"))?;
    Ok(node_wrapper(&ctx, host_id, context).into())
}

fn mode(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let mode = ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.shadow_root_data())
        .map(|root| root.mode)
        .ok_or_else(|| JsNativeError::typ().with_message("ShadowRoot receiver required"))?;
    Ok(js_str(if mode == ShadowRootMode::Open {
        "open"
    } else {
        "closed"
    }))
}

fn get_element_by_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let root_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let found = {
        let doc = ctx.doc.borrow();
        let root = doc
            .get_node(root_id)
            .filter(|node| node.is_shadow_root())
            .ok_or_else(|| JsNativeError::typ().with_message("ShadowRoot receiver required"))?;
        let mut stack: Vec<_> = root.children.iter().rev().copied().collect();
        let mut found = None;
        while let Some(id) = stack.pop() {
            let node = doc.get_node(id).unwrap();
            if !name.is_empty() && node.attr(markup5ever::local_name!("id")) == Some(name.as_str())
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

fn get_slot(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.attr(markup5ever::local_name!("slot")))
        .unwrap_or("")
        .to_owned();
    Ok(js_str(&value))
}

fn set_slot(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let value = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    ctx.mutate_doc()
        .mutate()
        .set_attribute(id, element::attr_name("slot"), &value);
    Ok(JsValue::undefined())
}

fn assigned_slot(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let assigned = {
        let mut doc = ctx.doc.borrow_mut();
        doc.compute_flattened_trees();
        doc.get_node(id)
            .and_then(|node| node.assigned_slot)
            .filter(|&slot_id| {
                doc.containing_shadow_root(slot_id)
                    .and_then(|root_id| doc.get_node(root_id))
                    .and_then(|node| node.shadow_root_data())
                    .is_some_and(|root| root.mode == ShadowRootMode::Open)
            })
    };
    Ok(node_or_null(&ctx, assigned, context))
}

fn assignment(
    this: &JsValue,
    args: &[JsValue],
    elements_only: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let flatten = match args.first().and_then(JsValue::as_object) {
        Some(options) => options.get(js_string!("flatten"), context)?.to_boolean(),
        None => false,
    };
    let ids = {
        let mut doc = ctx.doc.borrow_mut();
        if !doc.get_node(id).is_some_and(|node| {
            node.data
                .is_element_with_tag_name(&markup5ever::local_name!("slot"))
        }) {
            return Err(JsNativeError::typ()
                .with_message("HTMLSlotElement receiver required")
                .into());
        }
        let mut ids = doc.slot_assigned_nodes(id, flatten);
        if elements_only {
            ids.retain(|&id| doc.get_node(id).is_some_and(|node| node.is_element()));
        }
        ids
    };
    let values: Vec<JsValue> = ids
        .into_iter()
        .map(|id| node_wrapper(&ctx, id, context).into())
        .collect();
    Ok(JsArray::from_iter(values, context).into())
}

fn assigned_nodes(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    assignment(this, args, false, context)
}

fn assigned_elements(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    assignment(this, args, true, context)
}

pub(crate) fn deliver_slot_changes(ctx: &DomCtx, context: &mut Context) -> bool {
    let slots = ctx.doc.borrow_mut().take_slot_changes();
    let delivered = !slots.is_empty();
    for id in slots {
        let target: JsValue = node_wrapper(ctx, id, context).into();
        let event = super::event::create_event(ctx, "slotchange", true, false, &target, context);
        let _ = super::shadow_event::dispatch(ctx, id, &event, false, context);
    }
    delivered
}

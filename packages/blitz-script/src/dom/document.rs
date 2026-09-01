//! The `Document` prototype: node creation and lookup.

use blitz_dom::NodeId;
use blitz_dom::local_name;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsResult, JsString, NativeFunction, js_string};

use super::{
    define_accessor, define_method, dom_ctx, node_or_null, node_wrapper, qual_name, qual_name_ns,
    this_node_id, to_rust_string,
};
use crate::state::DomCtx;

pub(crate) fn init_document_proto(proto: &JsObject, context: &mut Context) {
    define_accessor(
        proto,
        "documentElement",
        Some(document_element),
        None,
        context,
    );
    define_accessor(proto, "body", Some(body), None, context);
    define_accessor(proto, "head", Some(head), None, context);
    define_accessor(proto, "activeElement", Some(active_element), None, context);
    define_accessor(proto, "defaultView", Some(default_view), None, context);

    define_method(proto, "createElement", 1, create_element, context);
    define_method(proto, "createElementNS", 2, create_element_ns, context);
    define_method(proto, "createTextNode", 1, create_text_node, context);
    define_method(proto, "importNode", 2, import_node, context);
    define_method(proto, "getSelection", 0, get_selection, context);
    define_method(proto, "createComment", 1, create_comment, context);
    define_method(
        proto,
        "createDocumentFragment",
        0,
        create_document_fragment,
        context,
    );
    define_method(proto, "getElementById", 1, get_element_by_id, context);
    define_method(proto, "querySelector", 1, query_selector, context);
    define_method(proto, "querySelectorAll", 1, query_selector_all, context);
}

fn find_tag(ctx: &DomCtx, tag: blitz_dom::LocalName) -> Option<NodeId> {
    let doc = ctx.doc.borrow();
    let root = doc.try_root_element()?;
    if root.data.is_element_with_tag_name(&tag) {
        return Some(root.id);
    }
    root.children
        .iter()
        .copied()
        .find(|child_id| {
            doc.get_node(*child_id)
                .is_some_and(|child| child.data.is_element_with_tag_name(&tag))
        })
        .or_else(|| {
            // Fall back to a full tree search
            let mut stack = vec![doc.root_node().id];
            while let Some(node_id) = stack.pop() {
                let node = doc.get_node(node_id)?;
                if node.data.is_element_with_tag_name(&tag) {
                    return Some(node_id);
                }
                stack.extend(node.children.iter().rev().copied());
            }
            None
        })
}

fn document_element(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let root_id = ctx.doc.borrow().try_root_element().map(|root| root.id);
    Ok(node_or_null(&ctx, root_id, context))
}

fn body(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let body_id = find_tag(&ctx, local_name!("body"));
    Ok(node_or_null(&ctx, body_id, context))
}

fn head(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let head_id = find_tag(&ctx, local_name!("head"));
    Ok(node_or_null(&ctx, head_id, context))
}

fn active_element(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let focus_id = ctx.doc.borrow().get_focussed_node_id();
    Ok(node_or_null(&ctx, focus_id, context))
}

fn default_view(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    Ok(context.global_object().into())
}

// === Node creation ===

fn create_element(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:createElement");
    let _ = this_node_id(this)?;
    let tag = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_element(qual_name(&tag), Vec::new())
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

fn create_element_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let ns = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let tag = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate()
            .create_element(qual_name_ns(&tag, &ns), Vec::new())
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

/// Clone a node for insertion into this document.
///
/// Every script-visible node currently belongs to the document represented by
/// this Boa realm, so importing is the same structural operation as
/// `cloneNode`. Keeping it on `Document` still matters: Solid deliberately
/// takes this path for custom-element template roots such as Chuzz's
/// `<web-view>` page host.
fn import_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    let node = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let deep = args.get(1).cloned().unwrap_or_else(JsValue::undefined);
    super::node::clone_node(&node, &[deep], context)
}

/// `document.getSelection()`.
///
/// Enough of `Selection` for the idioms that reach for it: reading the
/// highlighted string, and the save/restore-the-range dance around a temporary
/// off-screen textarea. Without it the property is `undefined` and the *call*
/// throws, so `document.getSelection()?.toString()` blows up rather than
/// yielding `undefined` — which is how a missing method takes an entire keydown
/// or copy handler down with it.
///
/// Ranges are not modelled, so `rangeCount` is 0 whenever the selection is
/// empty and `getRangeAt` returns null. Callers guard on `rangeCount` (all of
/// them, in practice) and then skip the restore rather than mis-restoring.
fn get_selection(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let text = ctx.doc.borrow().get_selected_text().unwrap_or_default();
    let range_count = i32::from(!text.is_empty());
    let selection = ObjectInitializer::new(context)
        .function(
            NativeFunction::from_fn_ptr(selection_to_string),
            js_string!("toString"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(selection_get_range_at),
            js_string!("getRangeAt"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(selection_remove_all_ranges),
            js_string!("removeAllRanges"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(selection_add_range),
            js_string!("addRange"),
            1,
        )
        .property(js_string!("rangeCount"), range_count, Attribute::all())
        .property(js_string!("isCollapsed"), text.is_empty(), Attribute::all())
        .build();
    Ok(selection.into())
}

fn selection_to_string(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let text = ctx.doc.borrow().get_selected_text().unwrap_or_default();
    Ok(JsValue::from(JsString::from(text)))
}

fn selection_get_range_at(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::null())
}

fn selection_remove_all_ranges(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    dom_ctx(context)?.doc.borrow_mut().clear_text_selection();
    Ok(JsValue::undefined())
}

/// Ranges are not modelled, so there is nothing to put back. Present and inert
/// rather than absent, because absent is what throws.
fn selection_add_range(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn create_text_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:createTextNode");
    let _ = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_text_node(&text)
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

/// `new Text("...")`, the constructor form of `document.createTextNode`.
///
/// Rarely called directly, but frameworks reference the global to test node
/// types, and a missing global is a `ReferenceError` at module scope: the whole
/// bundle dies before it renders, which looks exactly like a blank page rather
/// than like a missing API.
/// The prototype has to be attached, not just the callable. `register_global_callable`
/// alone produces a function with no `prototype` property, and `class X extends
/// Text` then fails with "superclass prototype must be an object or null",
/// which is how pathscale.com's bundle died after `Text` itself existed.
pub(crate) fn register_text_constructor(proto: &JsObject, context: &mut Context) {
    context
        .register_global_callable(
            boa_engine::js_string!("Text"),
            1,
            boa_engine::NativeFunction::from_fn_ptr(text_constructor),
        )
        .expect("failed to register Text constructor");
    let global = context.global_object().clone();
    let constructor = global
        .get(boa_engine::js_string!("Text"), context)
        .expect("Text constructor missing")
        .as_object()
        .expect("Text is not an object");
    constructor
        .set(
            boa_engine::js_string!("prototype"),
            proto.clone(),
            true,
            context,
        )
        .expect("failed to set Text prototype");
}

fn text_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    // An omitted argument makes an empty text node, per the DOM spec, rather
    // than the string "undefined".
    let text = match args.first() {
        Some(value) if !value.is_undefined() => to_rust_string(value, context)?,
        _ => String::new(),
    };
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_text_node(&text)
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

fn create_comment(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_comment_node(&text)
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

/// `document.createDocumentFragment()`.
///
/// jQuery calls this during initialisation:
///
/// ```js
/// xe = C.createDocumentFragment().appendChild(C.createElement("div"))
/// ```
///
/// Without it the call threw `TypeError: not a callable function` and the
/// library died before defining `jQuery`, so every page depending on it lost
/// its scripting. Eight sites in a hundred-site corpus failed exactly there,
/// across four jQuery versions on four CDNs, and every one of them reported it
/// downstream as `jQuery is not defined` — a missing global that was never
/// missing.
fn create_document_fragment(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_document_fragment()
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

// === Lookup ===

fn get_element_by_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let id = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node_id = ctx.doc.borrow().get_element_by_id(&id);
    Ok(node_or_null(&ctx, node_id, context))
}

fn query_selector(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node_id = ctx.doc.borrow().query_selector(&selector).ok().flatten();
    Ok(node_or_null(&ctx, node_id, context))
}

fn query_selector_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let matches: Vec<NodeId> = ctx
        .doc
        .borrow()
        .query_selector_all(&selector)
        .map(|matches| matches.into_iter().collect())
        .unwrap_or_default();
    let wrappers: Vec<JsValue> = matches
        .into_iter()
        .map(|match_id| node_wrapper(&ctx, match_id, context).into())
        .collect();
    Ok(JsArray::from_iter(wrappers, context).into())
}

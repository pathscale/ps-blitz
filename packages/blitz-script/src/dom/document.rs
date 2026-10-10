//! The `Document` prototype: node creation and lookup.

use blitz_dom::NodeId;
use blitz_dom::local_name;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{JsObject, ObjectInitializer};
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
    define_accessor(proto, "implementation", Some(implementation), None, context);
    define_accessor(proto, "title", Some(title), None, context);
    define_accessor(proto, "defaultView", Some(default_view), None, context);
    define_accessor(
        proto,
        "location",
        Some(get_location),
        Some(set_location),
        context,
    );
    define_accessor(proto, "domain", Some(get_domain), Some(set_domain), context);

    define_method(proto, "createElement", 1, create_element, context);
    define_method(proto, "createElementNS", 2, create_element_ns, context);
    define_method(proto, "createTextNode", 1, create_text_node, context);
    define_method(proto, "importNode", 2, import_node, context);
    define_method(proto, "createRange", 0, super::range::create_range, context);
    define_method(
        proto,
        "getSelection",
        0,
        super::range::document_selection,
        context,
    );
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

/// The first element with `tag` inside `root_id`'s subtree.
///
/// Scoped to `root_id` rather than the page, because `createHTMLDocument`
/// makes a second document in the same arena. A page-wide search answered
/// `newDoc.body` with the live page's `body`, which is worse than answering
/// `null`: jQuery writes two `<form>` elements into whatever it gets back.
fn find_tag_within(ctx: &DomCtx, root_id: NodeId, tag: blitz_dom::LocalName) -> Option<NodeId> {
    let doc = ctx.doc.borrow();

    // `document > html > body` is the shape in nearly every case, so try the
    // root element and its children before walking anything. Without this,
    // reading `document.body` scans the whole of `head` first, every time.
    let root_element = doc
        .get_node(root_id)?
        .children
        .iter()
        .copied()
        .find(|id| doc.get_node(*id).is_some_and(|child| child.is_element()));
    if let Some(element_id) = root_element {
        let element = doc.get_node(element_id)?;
        if element.data.is_element_with_tag_name(&tag) {
            return Some(element_id);
        }
        if let Some(found) = element.children.iter().copied().find(|child_id| {
            doc.get_node(*child_id)
                .is_some_and(|child| child.data.is_element_with_tag_name(&tag))
        }) {
            return Some(found);
        }
    }

    // Otherwise walk. A missing node is skipped rather than ending the search:
    // returning `None` from inside the loop would abandon siblings that are
    // still there.
    let mut stack = vec![root_id];
    while let Some(node_id) = stack.pop() {
        let Some(node) = doc.get_node(node_id) else {
            continue;
        };
        if node.data.is_element_with_tag_name(&tag) {
            return Some(node_id);
        }
        stack.extend(node.children.iter().rev().copied());
    }
    None
}

fn document_element(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let this_id = this_node_id(this)?;
    let root_id = ctx.doc.borrow().try_root_element().map(|root| root.id);
    // A created document answers with its own root element; the page document
    // keeps the cached root it already had.
    let root_id = if Some(this_id) == ctx.doc.borrow().root_node().id.into() {
        root_id
    } else {
        let doc = ctx.doc.borrow();
        doc.get_node(this_id).and_then(|node| {
            node.children
                .iter()
                .copied()
                .find(|id| doc.get_node(*id).is_some_and(|c| c.is_element()))
        })
    };
    Ok(node_or_null(&ctx, root_id, context))
}

fn body(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let this_id = this_node_id(this)?;
    let body_id = find_tag_within(&ctx, this_id, local_name!("body"));
    Ok(node_or_null(&ctx, body_id, context))
}

fn head(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let this_id = this_node_id(this)?;
    let head_id = find_tag_within(&ctx, this_id, local_name!("head"));
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

/// `document.location`: the window's `Location`, the same object.
fn get_location(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    context.global_object().get(js_string!("location"), context)
}

/// `document.location = url` navigates, as `location.href = url` does.
fn set_location(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    let location = context
        .global_object()
        .get(js_string!("location"), context)?;
    if let Some(location) = location.as_object() {
        location.set(
            js_string!("href"),
            args.first().cloned().unwrap_or_default(),
            true,
            context,
        )?;
    }
    Ok(JsValue::undefined())
}

/// `document.domain`: the host of the document's URL.
fn get_domain(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    let location = context
        .global_object()
        .get(js_string!("location"), context)?;
    match location.as_object() {
        Some(location) => location.get(js_string!("hostname"), context),
        None => Ok(js_string!("").into()),
    }
}

/// Setting `document.domain` to the current host or one of its parent
/// domains is accepted and changes nothing, which is what Chromium does now
/// that documents are origin-keyed; anything else is a SecurityError.
fn set_domain(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let requested = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    let current = get_domain(this, &[], context)?
        .to_string(context)?
        .to_std_string_escaped()
        .to_ascii_lowercase();
    let allowed = !requested.is_empty()
        && (current == requested
            || (current.ends_with(&requested)
                && current[..current.len() - requested.len()].ends_with('.')
                && requested.contains('.')));
    if !allowed {
        return Err(boa_engine::JsNativeError::error()
            .with_message(format!(
                "SecurityError: '{requested}' is not a suffix of '{current}'"
            ))
            .into());
    }
    Ok(JsValue::undefined())
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
/// library died before defining `jQuery` and every page depending on it lost
/// its scripting. Eight sites in a hundred-site corpus failed that way, and
/// they reported it as `jQuery is not defined`: a missing global that was never
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
    let node_id = ctx
        .doc
        .borrow()
        .query_selector(&selector)
        .map_err(|_| super::element::invalid_selector(&selector))?;
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
        .map_err(|_| super::element::invalid_selector(&selector))?;
    let wrappers: Vec<JsValue> = matches
        .into_iter()
        .map(|match_id| node_wrapper(&ctx, match_id, context).into())
        .collect();
    Ok(JsArray::from_iter(wrappers, context).into())
}

/// `document.implementation`.
///
/// Only `createHTMLDocument` is implemented. `hasFeature` was deprecated to a
/// constant `true` long ago and nothing reads `createDocument` in the corpus,
/// so neither is here; adding a stub that answers plausibly would be worse
/// than the absent property, which at least fails where the caller can see it.
fn implementation(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    let object = ObjectInitializer::new(context)
        .function(
            NativeFunction::from_fn_ptr(create_html_document),
            js_string!("createHTMLDocument"),
            1,
        )
        .build();
    Ok(object.into())
}

/// The text of this document's `<title>`, or the empty string.
fn title(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let this_id = this_node_id(this)?;
    let text = find_tag_within(&ctx, this_id, local_name!("title"))
        .and_then(|title_id| {
            let doc = ctx.doc.borrow();
            let title = doc.get_node(title_id)?;
            Some(
                title
                    .children
                    .iter()
                    .filter_map(|child_id| doc.get_node(*child_id))
                    .filter_map(|child| child.text_data().map(|text| text.content.clone()))
                    .collect::<String>(),
            )
        })
        .unwrap_or_default();
    Ok(JsString::from(text.as_str()).into())
}

/// `document.implementation.createHTMLDocument(title)`.
///
/// The new document is detached, so it never lays out or paints, and it lives
/// in this document's arena rather than a second `BaseDocument`.
fn create_html_document(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let title = match args.first() {
        // `createHTMLDocument()` with no argument is not the same call as
        // `createHTMLDocument(undefined)`: the first has no title at all, the
        // second stringifies to "undefined". jQuery passes "".
        None => String::new(),
        Some(value) => to_rust_string(value, context)?,
    };
    let node_id = {
        let mut doc = ctx.doc.borrow_mut();
        doc.mutate().create_html_document(&title)
    };
    Ok(node_wrapper(&ctx, node_id, context).into())
}

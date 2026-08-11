//! `window.customElements`: the JavaScript half of custom elements.
//!
//! `blitz-dom` already carries a custom element registry, but it is a Rust one:
//! definitions are `Box<dyn CustomElement>` supplied by the embedder. A page
//! cannot reach it, and `customElements` was simply absent, so
//! `customElements.define(...)` threw a `ReferenceError` out of whatever module
//! ran it. A framework that registers its components at import time loses the
//! whole module, and the page renders as unstyled markup or not at all.
//!
//! What this implements is upgrade by prototype: a defined element gets the
//! class's prototype and its `connectedCallback`. The constructor body does not
//! run, because Boa offers no way to construct into an object that already
//! exists, and most component code puts its work in `connectedCallback`
//! anyway — the constructor cannot touch attributes or children per spec.

use boa_engine::object::JsObject;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsNativeError, JsResult, js_string};

use super::{dom_ctx, js_str, node_wrapper, to_rust_string};

/// A valid custom element name contains a dash and starts with a lowercase
/// ASCII letter. The dash is what keeps the namespace disjoint from HTML's.
fn is_valid_name(name: &str) -> bool {
    name.contains('-')
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
}

/// `customElements.define(name, constructor)`.
pub(crate) fn define(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(constructor) = args.get(1).and_then(|value| value.as_object()) else {
        return Err(JsNativeError::typ()
            .with_message("customElements.define: constructor must be an object")
            .into());
    };
    if !is_valid_name(&name) {
        return Err(JsNativeError::typ()
            .with_message(format!(
                "customElements.define: '{name}' is not a valid name"
            ))
            .into());
    }

    let ctx = dom_ctx(context)?;
    let already_defined = ctx
        .state
        .borrow()
        .custom_element_definitions
        .contains_key(&name);
    if already_defined {
        return Err(JsNativeError::typ()
            .with_message(format!(
                "customElements.define: '{name}' is already defined"
            ))
            .into());
    }
    ctx.state
        .borrow_mut()
        .custom_element_definitions
        .insert(name.clone(), constructor.clone());

    // Upgrade what is already in the document.
    //
    // Definition usually happens after parsing, so the elements this applies to
    // exist by the time it runs. Without this pass they keep the plain Element
    // prototype for their whole life and every method the class defines is
    // missing.
    let existing = ctx
        .doc
        .borrow()
        .query_selector_all(&name)
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    for node_id in existing {
        let wrapper = node_wrapper(&ctx, node_id, context);
        upgrade(&wrapper, &constructor, context)?;
    }

    Ok(JsValue::undefined())
}

/// `customElements.get(name)`: the constructor, or undefined.
pub(crate) fn get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let definition = ctx
        .state
        .borrow()
        .custom_element_definitions
        .get(&name)
        .cloned();
    Ok(definition.map_or(JsValue::undefined(), JsValue::from))
}

/// `customElements.getName(constructor)`: the inverse of `get`.
pub(crate) fn get_name(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(constructor) = args.first().and_then(|value| value.as_object()) else {
        return Ok(JsValue::null());
    };
    let ctx = dom_ctx(context)?;
    let state = ctx.state.borrow();
    let found = state
        .custom_element_definitions
        .iter()
        .find(|(_, defined)| JsObject::equals(defined, &constructor))
        .map(|(name, _)| name.clone());
    Ok(found.map_or(JsValue::null(), |name| js_str(&name)))
}

/// Give `element` the class's prototype and run `connectedCallback`.
///
/// Boa cannot construct into an object that already exists, so the constructor
/// body is not run. Per spec a custom element constructor may not inspect
/// attributes or children or add any, which is why component work belongs in
/// `connectedCallback` — so what is missing is narrower than it sounds, and the
/// element does gain every method and accessor the class declares.
fn upgrade(element: &JsObject, constructor: &JsObject, context: &mut Context) -> JsResult<()> {
    let prototype = constructor.get(js_string!("prototype"), context)?;
    let Some(prototype) = prototype.as_object() else {
        return Ok(());
    };

    // Splice the DOM prototype into the class's chain before adopting it.
    //
    // `HTMLElement` here is a shim that exists so `instanceof` works: it is a
    // constructor whose `Symbol.hasInstance` checks `nodeType`, and its
    // `prototype` is an empty object with no relationship to the object
    // `node_wrapper` hands out. So `class C extends HTMLElement` produces a
    // chain ending in that empty object, and adopting it as-is would take
    // `setAttribute` and every other DOM method away from the element.
    //
    // Walking to the end of the chain rather than re-pointing
    // `constructor.prototype` directly, so that `class B extends A extends
    // HTMLElement` keeps A's methods instead of losing them.
    let dom_prototype = element.prototype();
    if let Some(dom_prototype) = dom_prototype {
        let object_prototype = context.intrinsics().constructors().object().prototype();
        let mut tail = prototype.clone();
        loop {
            let Some(next) = tail.prototype() else { break };
            if JsObject::equals(&next, &object_prototype) || JsObject::equals(&next, &dom_prototype)
            {
                break;
            }
            tail = next;
        }
        if !JsObject::equals(&tail, &dom_prototype) {
            tail.set_prototype(Some(dom_prototype));
        }
    }

    element.set_prototype(Some(prototype.clone()));

    let connected = element.get(js_string!("connectedCallback"), context)?;
    if let Some(callback) = connected.as_object()
        && callback.is_callable()
    {
        callback.call(&JsValue::from(element.clone()), &[], context)?;
    }
    Ok(())
}

/// Upgrade `node_id` if its tag name has a definition.
///
/// Called when an element is inserted, so elements created after `define` are
/// upgraded too rather than only those present when the definition ran.
pub(crate) fn upgrade_if_defined(
    ctx: &crate::state::DomCtx,
    node_id: blitz_dom::NodeId,
    context: &mut Context,
) -> JsResult<()> {
    let tag = {
        let doc = ctx.doc.borrow();
        let Some(node) = doc.get_node(node_id) else {
            return Ok(());
        };
        let Some(element) = node.element_data() else {
            return Ok(());
        };
        element.name.local.to_string()
    };
    if !tag.contains('-') {
        return Ok(());
    }
    let definition = ctx
        .state
        .borrow()
        .custom_element_definitions
        .get(&tag)
        .cloned();
    let Some(constructor) = definition else {
        return Ok(());
    };
    let wrapper = node_wrapper(ctx, node_id, context);
    upgrade(&wrapper, &constructor, context)
}

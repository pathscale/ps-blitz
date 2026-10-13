//! Passive listener options and callback-scoped event cancellation.

use blitz_dom::node::NodeData;
use boa_engine::object::JsObject;
use boa_engine::{Context, JsNativeError, JsResult, JsValue};

use super::event::EventRef;
use super::node_id_of_value;
use crate::state::DomCtx;

/// An omitted or undefined member uses the target's default. Explicit false
/// opts out, including when inherited from the options object's prototype.
pub(super) fn option(args: &[JsValue], context: &mut Context) -> JsResult<Option<bool>> {
    let Some(options) = args.get(2).and_then(JsValue::as_object) else {
        return Ok(None);
    };
    let value = options.get(boa_engine::js_string!("passive"), context)?;
    Ok((!value.is_undefined()).then(|| value.to_boolean()))
}

fn default_value(this: &JsValue, kind: &str, context: &Context) -> bool {
    if !matches!(kind, "wheel" | "mousewheel" | "touchstart" | "touchmove") {
        return false;
    }
    if this
        .as_object()
        .is_some_and(|object| JsObject::equals(&object, &context.global_object()))
    {
        return true;
    }
    let Some(node_id) = node_id_of_value(this) else {
        return false;
    };
    let Some(ctx) = context.get_data::<DomCtx>() else {
        return false;
    };
    let doc = ctx.doc.borrow();
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    if matches!(&node.data, NodeData::Document(_)) {
        return true;
    }
    let Some(document) = node.owner_document.and_then(|id| doc.get_node(id)) else {
        return false;
    };
    let Some(root) = document
        .children
        .iter()
        .filter_map(|&id| doc.get_node(id))
        .find(|child| child.is_element())
    else {
        return false;
    };
    if root.id == node_id {
        return true;
    }
    // document.body is the first HTML body or frameset child of the HTML
    // document element, not any element whose tag happens to be body.
    let Some(element) = root.data.downcast_element() else {
        return false;
    };
    if element.name.ns != markup5ever::ns!(html)
        || element.name.local != markup5ever::local_name!("html")
    {
        return false;
    }
    root.children
        .iter()
        .filter_map(|&id| doc.get_node(id))
        .find(|child| {
            child.data.downcast_element().is_some_and(|element| {
                element.name.ns == markup5ever::ns!(html)
                    && (element.name.local == markup5ever::local_name!("body")
                        || element.name.local == markup5ever::local_name!("frameset"))
            })
        })
        .is_some_and(|body| body.id == node_id)
}

pub(crate) fn resolve(
    this: &JsValue,
    args: &[JsValue],
    kind: &str,
    context: &mut Context,
) -> JsResult<bool> {
    Ok(option(args, context)?.unwrap_or_else(|| default_value(this, kind, context)))
}

struct CallbackGuard {
    event: JsObject,
    previous: bool,
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        if let Some(event) = self.event.downcast_ref::<EventRef>() {
            event.in_passive_listener.set(self.previous);
        }
    }
}

/// The guard restores the flag before the dispatcher inspects the result or
/// invokes another listener, including on thrown exceptions and nested calls.
pub(crate) fn invoke(
    callback: &JsObject,
    this: &JsValue,
    event: &JsObject,
    passive: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let previous = event
        .downcast_ref::<EventRef>()
        .expect("listener Event")
        .in_passive_listener
        .replace(passive);
    let _guard = CallbackGuard {
        event: event.clone(),
        previous,
    };
    if callback.is_callable() {
        callback.call(this, &[event.clone().into()], context)
    } else {
        let method = callback
            .get(boa_engine::js_string!("handleEvent"), context)?
            .as_object()
            .filter(|method| method.is_callable())
            .ok_or_else(|| JsNativeError::typ().with_message("handleEvent is not callable"))?;
        method.call(&callback.clone().into(), &[event.clone().into()], context)
    }
}

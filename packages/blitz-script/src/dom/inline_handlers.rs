//! Lazy compilation and invocation of event handler content attributes.

use blitz_dom::NodeId;
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::{Context, Finalize, JsData, JsResult, JsString, JsValue, Trace};

use super::{
    ON_EVENT_TYPES, ON_HANDLER_PREFIX, define_accessor, define_value, dom_ctx, js_str,
    node_id_of_value, node_or_null, node_wrapper, this_node_id,
};
use crate::state::DomCtx;

#[derive(Trace, Finalize, JsData)]
struct RawHandler {
    element: JsObject,
    source: JsString,
}

fn event_type(name: &str) -> Option<&'static str> {
    let event = name.strip_prefix("on")?;
    ON_EVENT_TYPES.iter().copied().find(|known| *known == event)
}

fn key(event: &str) -> JsString {
    JsString::from(format!("{ON_HANDLER_PREFIX}{event}"))
}

fn forwarded(ctx: &DomCtx, id: NodeId, event: &str) -> bool {
    if !matches!(event, "load" | "error") {
        return false;
    }
    let doc = ctx.doc.borrow();
    doc.get_node(id).is_some_and(|node| {
        node.owner_document == Some(doc.root_node().id)
            && node.element_data().is_some_and(|element| {
                element.name.ns == markup5ever::ns!(html)
                    && matches!(element.name.local.as_ref(), "body" | "frameset")
            })
    })
}

fn target(ctx: &DomCtx, object: &JsObject, event: &str, context: &Context) -> JsObject {
    if node_id_of_value(&object.clone().into()).is_some_and(|id| forwarded(ctx, id, event)) {
        context.global_object().clone()
    } else {
        object.clone()
    }
}

fn set_raw(
    ctx: &DomCtx,
    element: &JsObject,
    event: &str,
    source: Option<JsString>,
    context: &mut Context,
) {
    let target = target(ctx, element, event, context);
    let value = source.map_or_else(JsValue::null, |source| {
        JsObject::from_proto_and_data(
            None,
            RawHandler {
                element: element.clone(),
                source,
            },
        )
        .into()
    });
    define_value(
        &target,
        &format!("{ON_HANDLER_PREFIX}{event}"),
        value,
        context,
    );
    if let Some(id) = node_id_of_value(&element.clone().into()) {
        super::node::root_inline_event_handlers(ctx, id, context);
    }
}

/// Seed a newly created wrapper from native raw handler state. This never scans
/// the ordinary attribute list, and does not copy another wrapper's IDL value.
pub(crate) fn seed(ctx: &DomCtx, id: NodeId, wrapper: &JsObject, context: &mut Context) {
    let attributes = {
        let doc = ctx.doc.borrow();
        doc.get_node(id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.inline_event_attributes.as_deref())
            .cloned()
            .unwrap_or_default()
    };
    for attribute in attributes {
        if let Some(event) = event_type(attribute.name.local.as_ref())
            && !forwarded(ctx, id, event)
        {
            set_raw(ctx, wrapper, event, Some(JsString::from(&*attribute.value)), context);
        }
    }
}

/// Apply only recorded on* changes. No document walk or MutationObserver
/// registration is needed. Draining before an IDL write preserves write order.
pub(crate) fn sync(ctx: &DomCtx, context: &mut Context) {
    let changes = ctx.doc.borrow_mut().take_inline_handler_changes();
    for (id, name, source) in changes {
        let Some(event) = event_type(name.local.as_ref()) else {
            continue;
        };
        if ctx.doc.borrow().get_node(id).is_none() {
            continue;
        }
        let wrapper = node_wrapper(ctx, id, context);
        set_raw(
            ctx,
            &wrapper,
            event,
            source.map(|source| JsString::from(&*source)),
            context,
        );
    }
}

fn form_owner(ctx: &DomCtx, id: NodeId) -> Option<NodeId> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(id)?;
    let element = node.element_data()?;
    if element.name.ns != markup5ever::ns!(html)
        || !matches!(
            element.name.local.as_ref(),
            "button" | "input" | "select" | "textarea" | "fieldset" | "object" | "output" | "img"
        )
    {
        return None;
    }
    if let Some(form) = element.attr(markup5ever::local_name!("form")) {
        if !node.flags.is_in_document() {
            return None;
        }
        return doc.get_element_by_id(form).filter(|id| {
            doc.get_node(*id).is_some_and(|node| {
                node.element_data().is_some_and(|element| {
                    element.name.ns == markup5ever::ns!(html)
                        && element.name.local == markup5ever::local_name!("form")
                })
            })
        });
    }
    let mut parent = node.parent;
    while let Some(id) = parent {
        let node = doc.get_node(id)?;
        if node.element_data().is_some_and(|element| {
            element.name.ns == markup5ever::ns!(html)
                && element.name.local == markup5ever::local_name!("form")
        }) {
            return Some(id);
        }
        parent = node.parent;
    }
    None
}

fn form(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = form_owner(&ctx, this_node_id(this)?);
    Ok(node_or_null(&ctx, owner, context))
}

fn compile(
    ctx: &DomCtx,
    element: &JsObject,
    source: &JsString,
    event: &str,
    window_error: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = node_id_of_value(&element.clone().into()) else {
        return Ok(JsValue::null());
    };
    let owner = {
        let doc = ctx.doc.borrow();
        doc.get_node(id).and_then(|node| node.owner_document)
    };
    let root = ctx.doc.borrow().root_node().id;
    if owner != Some(root) {
        return Ok(JsValue::null());
    }

    let parameters = if window_error {
        "event, source, lineno, colno, error"
    } else {
        "event"
    };
    let constructor = context.intrinsics().constructors().function().constructor();

    // Validate the author's function body independently before embedding it in
    // the scope factory. A closing brace in an invalid body cannot escape into
    // the generated factory. The intrinsic constructor also guarantees sloppy
    // parsing independently of the script that first reads the IDL property.
    constructor.construct(&[js_str(parameters), source.clone().into()], None, context)?;

    let body = format!(
        "with (this[0]) {{ with (this[1]) {{ with (this[2]) {{\
         return function on{event}({parameters}) {{\n{}\n}};\
         }} }} }}",
        source.to_std_string_lossy()
    );
    let factory = constructor.construct(&[js_str(&body)], None, context)?;
    let document = node_wrapper(ctx, root, context);
    let form = form_owner(ctx, id)
        .map(|id| node_wrapper(ctx, id, context))
        .unwrap_or_else(|| JsObject::with_object_proto(context.intrinsics()));
    let scopes = JsArray::from_iter(
        [document.into(), form.into(), element.clone().into()],
        context,
    );
    factory.call(&scopes.into(), &[], context)
}

pub(crate) fn get(
    this: &JsValue,
    event: &'static str,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::null());
    };
    let ctx = dom_ctx(context)?;
    sync(&ctx, context);
    let target = target(&ctx, &object, event, context);
    let stored = target.get(key(event), context)?;
    let raw = stored.as_object().and_then(|object| {
        object
            .downcast_ref::<RawHandler>()
            .map(|raw| (raw.element.clone(), raw.source.clone()))
    });
    let Some((element, source)) = raw else {
        return Ok(if stored.is_callable() {
            stored
        } else {
            JsValue::null()
        });
    };

    // Both a failed compilation and a successful one consume the raw state.
    define_value(
        &target,
        &format!("{ON_HANDLER_PREFIX}{event}"),
        JsValue::null(),
        context,
    );
    let window_error = event == "error" && JsObject::equals(&target, &context.global_object());
    let value = match compile(&ctx, &element, &source, event, window_error, context) {
        Ok(value) => value,
        Err(error) => {
            crate::runtime::report_handler_error(
                context,
                &format!("on{event} content attribute"),
                &error,
            );
            JsValue::null()
        }
    };
    define_value(
        &target,
        &format!("{ON_HANDLER_PREFIX}{event}"),
        value.clone(),
        context,
    );
    Ok(value)
}

pub(crate) fn set(
    this: &JsValue,
    args: &[JsValue],
    event: &'static str,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let ctx = dom_ctx(context)?;
    sync(&ctx, context);
    let target = target(&ctx, &object, event, context);
    let value = args
        .first()
        .filter(|value| value.is_callable())
        .cloned()
        .unwrap_or_else(JsValue::null);
    define_value(
        &target,
        &format!("{ON_HANDLER_PREFIX}{event}"),
        value,
        context,
    );
    if let Some(id) = node_id_of_value(this) {
        super::node::root_inline_event_handlers(&ctx, id, context);
    }
    Ok(JsValue::undefined())
}

/// Invoke an IDL handler separately from addEventListener callbacks, which
/// have no return-value cancellation semantics. Resolve it after listeners so
/// a listener's replacement or removal takes effect in this dispatch.
pub(crate) fn invoke(
    ctx: &DomCtx,
    target: &JsObject,
    event: &JsObject,
    name: &str,
    context: &mut Context,
) -> bool {
    sync(ctx, context);
    if node_id_of_value(&target.clone().into()).is_some_and(|id| forwarded(ctx, id, name)) {
        // Forwarded handlers belong to Window's listener list.
        return false;
    }
    let handler = match target.get(JsString::from(format!("on{name}")), context) {
        Ok(value) => value.as_callable(),
        Err(error) => {
            crate::runtime::report_handler_error(context, &format!("on{name} getter"), &error);
            None
        }
    };
    let Some(handler) = handler else {
        return false;
    };
    let window_error = name == "error" && JsObject::equals(target, &context.global_object());
    let result = (|| -> JsResult<JsValue> {
        let mut arguments = vec![event.clone().into()];
        if window_error {
            let message = event.get(JsString::from("message"), context)?;
            if !message.is_undefined() {
                arguments = vec![
                    message,
                    event.get(JsString::from("filename"), context)?,
                    event.get(JsString::from("lineno"), context)?,
                    event.get(JsString::from("colno"), context)?,
                    event.get(JsString::from("error"), context)?,
                ];
            }
        }
        handler.call(&target.clone().into(), &arguments, context)
    })();
    match result {
        Ok(value) if value.as_boolean() == Some(window_error) => {
            if let Some(event) = event.downcast_ref::<super::event::EventRef>()
                && event.get("cancelable").to_boolean()
            {
                event.prevented.set(true);
            }
        }
        Ok(_) => {}
        Err(error) => {
            crate::runtime::report_handler_error(context, &format!("on{name} handler"), &error);
        }
    }
    true
}

pub(crate) fn install(context: &mut Context) {
    let window = context.global_object().clone();
    for event in ON_EVENT_TYPES {
        super::define_on_event_accessor(&window, event, context);
    }
    for interface in [
        "HTMLButtonElement",
        "HTMLInputElement",
        "HTMLSelectElement",
        "HTMLTextAreaElement",
        "HTMLFieldSetElement",
        "HTMLObjectElement",
        "HTMLOutputElement",
        "HTMLImageElement",
    ] {
        let prototype = super::interfaces::prototype(interface, context);
        define_accessor(&prototype, "form", Some(form), None, context);
    }
}


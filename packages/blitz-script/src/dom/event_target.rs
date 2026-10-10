//! EventTarget instances own their listeners in Boa's traced heap.

use std::cell::Cell;
use std::sync::Arc;

use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue,
    NativeFunction, Trace,
};
use boa_gc::GcRefCell;

use super::event::{EventRef, set_event_path};
use super::{define_value, js_str, node_id_of_value, to_rust_string};

#[derive(Clone, Trace, Finalize)]
struct Listener {
    callback: JsObject,
    signal: Option<JsObject>,
    #[unsafe_ignore_trace]
    kind: Arc<str>,
    #[unsafe_ignore_trace]
    capture: bool,
    #[unsafe_ignore_trace]
    once: bool,
    #[unsafe_ignore_trace]
    id: u64,
}

#[derive(Trace, Finalize, JsData)]
struct Target {
    listeners: GcRefCell<Vec<Listener>>,
    #[unsafe_ignore_trace]
    next_id: Cell<u64>,
}

pub(super) fn construct(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = super::interfaces::construction_prototype(new_target, "EventTarget", context)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Target {
            listeners: GcRefCell::new(Vec::new()),
            next_id: Cell::new(0),
        },
    ).into())
}

fn options(args: &[JsValue], context: &mut Context) -> JsResult<(bool, bool, Option<JsObject>)> {
    let Some(options) = args.get(2) else {
        return Ok((false, false, None));
    };
    if let Some(object) = options.as_object() {
        Ok((
            object.get(boa_engine::js_string!("capture"), context)?.to_boolean(),
            object.get(boa_engine::js_string!("once"), context)?.to_boolean(),
            object.get(boa_engine::js_string!("signal"), context)?.as_object(),
        ))
    } else {
        Ok((options.to_boolean(), false, None))
    }
}

#[derive(Clone, Copy)]
enum Method {
    Add,
    Remove,
    Dispatch,
}

/// EventTarget.prototype's methods work on every EventTarget, not only on
/// script-constructed ones: the window and DOM nodes keep their own native
/// listener lists, so a call on them (ShadyDOM keeps
/// `EventTarget.prototype.addEventListener` and calls it on `window`) goes to
/// that implementation.
fn delegate(
    object: &JsObject,
    method: Method,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> Option<JsResult<JsValue>> {
    if object.downcast_ref::<Target>().is_some() {
        return None;
    }
    if JsObject::equals(object, &context.global_object()) {
        return Some(match method {
            Method::Add => crate::runtime::window_add_event_listener(this, args, context),
            Method::Remove => crate::runtime::window_remove_event_listener(this, args, context),
            Method::Dispatch => crate::runtime::window_dispatch_event(this, args, context),
        });
    }
    if super::node_id_of_value(this).is_some() {
        return Some(match method {
            Method::Add => super::node::add_event_listener(this, args, context),
            Method::Remove => super::node::remove_event_listener(this, args, context),
            Method::Dispatch => super::node::dispatch_event(this, args, context),
        });
    }
    None
}

fn add(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this.as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid EventTarget receiver"))?;
    if let Some(result) = delegate(&object, Method::Add, this, args, context) {
        return result;
    }
    let target = object.downcast_ref::<Target>()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid EventTarget receiver"))?;
    let kind: Arc<str> = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?.into();
    let Some(callback) = args.get(1).and_then(JsValue::as_object) else {
        return Ok(JsValue::undefined());
    };
    let (capture, once, signal) = options(args, context)?;
    if let Some(signal) = &signal
        && signal.get(boa_engine::js_string!("aborted"), context)?.to_boolean()
    {
        return Ok(JsValue::undefined());
    }
    let mut listeners = target.listeners.borrow_mut();
    if !listeners.iter().any(|listener| {
        listener.kind == kind
            && listener.capture == capture
            && JsObject::equals(&listener.callback, &callback)
    }) {
        let id = target.next_id.get();
        target.next_id.set(id.wrapping_add(1));
        listeners.push(Listener { callback, signal, kind, capture, once, id });
    }
    Ok(JsValue::undefined())
}

fn remove(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this.as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid EventTarget receiver"))?;
    if let Some(result) = delegate(&object, Method::Remove, this, args, context) {
        return result;
    }
    let target = object.downcast_ref::<Target>()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid EventTarget receiver"))?;
    let kind = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(callback) = args.get(1).and_then(JsValue::as_object) else {
        return Ok(JsValue::undefined());
    };
    let (capture, _, _) = options(args, context)?;
    target.listeners.borrow_mut().retain(|listener| {
        listener.kind.as_ref() != kind
            || listener.capture != capture
            || !JsObject::equals(&listener.callback, &callback)
    });
    Ok(JsValue::undefined())
}

fn dispatch(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this.as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid EventTarget receiver"))?;
    if let Some(result) = delegate(&object, Method::Dispatch, this, args, context) {
        return result;
    }
    if object.downcast_ref::<Target>().is_none() {
        return Err(JsNativeError::typ().with_message("Invalid EventTarget receiver").into());
    }
    let event = args.first().and_then(JsValue::as_object)
        .filter(|object| object.downcast_ref::<EventRef>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("dispatchEvent requires an Event"))?;
    let kind = to_rust_string(&event.get(boa_engine::js_string!("type"), context)?, context)?;
    if kind.is_empty()
        || !event.get(boa_engine::js_string!("currentTarget"), context)?.is_null_or_undefined()
    {
        return Err(super::interfaces::exception(
            "InvalidStateError", "Event is uninitialised or already being dispatched", context,
        ));
    }
    {
        let data = event.downcast_ref::<EventRef>().expect("validated Event");
        data.stopped.set(false);
        data.stopped_immediate.set(false);
    }
    define_value(&event, "target", this.clone(), context);
    define_value(&event, "srcElement", this.clone(), context);
    define_value(&event, "currentTarget", this.clone(), context);
    define_value(&event, "isTrusted", JsValue::from(false), context);
    define_value(&event, "eventPhase", JsValue::from(2), context);
    set_event_path(&event, vec![object.clone()]);
    let mut snapshot = object.downcast_ref::<Target>()
        .expect("validated EventTarget").listeners.borrow().clone();
    snapshot.sort_by_key(|listener| !listener.capture);
    for listener in snapshot {
        if listener.kind.as_ref() != kind {
            continue;
        }
        if let Some(signal) = &listener.signal
            && signal.get(boa_engine::js_string!("aborted"), context)?.to_boolean()
        {
            object.downcast_ref::<Target>().expect("validated EventTarget")
                .listeners.borrow_mut().retain(|entry| entry.id != listener.id);
            continue;
        }
        let present = object.downcast_ref::<Target>().expect("validated EventTarget")
            .listeners.borrow().iter().any(|entry| entry.id == listener.id);
        if !present {
            continue;
        }
        if listener.once {
            object.downcast_ref::<Target>().expect("validated EventTarget")
                .listeners.borrow_mut().retain(|entry| entry.id != listener.id);
        }
        let result = if listener.callback.is_callable() {
            listener.callback.call(this, &[event.clone().into()], context)
        } else {
            let method = listener.callback.get(boa_engine::js_string!("handleEvent"), context);
            method.and_then(|method| {
                let method = method.as_object().filter(|method| method.is_callable())
                    .ok_or_else(|| JsNativeError::typ().with_message("handleEvent is not callable"))?;
                method.call(&listener.callback.clone().into(), &[event.clone().into()], context)
            })
        };
        if let Err(error) = result {
            eprintln!("EventTarget listener: {error}");
        }
        if event.downcast_ref::<EventRef>().expect("validated Event")
            .stopped_immediate.get()
        {
            break;
        }
    }
    define_value(&event, "currentTarget", JsValue::null(), context);
    define_value(&event, "eventPhase", JsValue::from(0), context);
    set_event_path(&event, Vec::new());
    let cancelable = event.get(boa_engine::js_string!("cancelable"), context)?.to_boolean();
    let prevented = event.downcast_ref::<EventRef>().expect("validated Event").prevented.get();
    Ok(JsValue::from(!(cancelable && prevented)))
}

pub(super) fn init(node: &JsObject, context: &mut Context) {
    let proto = super::interfaces::prototype("EventTarget", context);
    for (name, length, native) in [
        ("addEventListener", 2, add as super::NativeFnPtr),
        ("removeEventListener", 2, remove as super::NativeFnPtr),
        ("dispatchEvent", 1, dispatch as super::NativeFnPtr),
    ] {
        let original = node.get(JsString::from(name), context)
            .expect("missing node event method").as_object().expect("invalid node event method");
        let function = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |this, args, original, context| {
                    if node_id_of_value(this).is_some() {
                        original.call(this, args, context)
                    } else {
                        native(this, args, context)
                    }
                },
                original,
            ),
        ).name(JsString::from(name)).length(length).build();
        define_value(&proto, name, function.into(), context);
        node.delete_property_or_throw(JsString::from(name), context)
            .expect("failed to move EventTarget method");
    }
    let _ = js_str;
}


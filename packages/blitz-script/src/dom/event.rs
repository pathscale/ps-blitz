//! JS `Event` objects dispatched to script event listeners.

use std::cell::Cell;

use blitz_traits::events::{
    BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, DomEventData,
};
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::value::JsValue;
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsString, Trace};
use boa_gc::GcRefCell;
use keyboard_types::Modifiers;

use super::{define_accessor, define_method, define_value, js_str, node_or_null};
use crate::state::DomCtx;

/// Native data attached to JS `Event` objects. Tracks the flags set by
/// `preventDefault` / `stopPropagation` so they can be read back after dispatch.
#[derive(Default, Trace, Finalize, JsData)]
pub(crate) struct EventRef {
    pub path: GcRefCell<Vec<JsObject>>,
    pub fields: GcRefCell<Vec<(JsString, JsValue)>>,
    #[unsafe_ignore_trace]
    pub interface: Cell<&'static str>,
    #[unsafe_ignore_trace]
    pub initialized: Cell<bool>,
    #[unsafe_ignore_trace]
    pub dispatching: Cell<bool>,
    #[unsafe_ignore_trace]
    pub prevented: Cell<bool>,
    #[unsafe_ignore_trace]
    pub stopped: Cell<bool>,
    #[unsafe_ignore_trace]
    pub stopped_immediate: Cell<bool>,
}

impl EventRef {
    pub(crate) fn get(&self, name: &str) -> JsValue {
        let key = JsString::from(name);
        self.fields
            .borrow()
            .iter()
            .find(|(stored, _)| stored == &key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    pub(crate) fn put(&self, name: &str, value: JsValue) {
        let key = JsString::from(name);
        let mut fields = self.fields.borrow_mut();
        if let Some((_, stored)) = fields.iter_mut().find(|(stored, _)| stored == &key) {
            *stored = value;
        } else {
            fields.push((key, value));
        }
    }
}

/// Host writes update native slots rather than creating writable JS properties.
pub(crate) fn set_event_field(object: &JsObject, name: &str, value: &JsValue) -> bool {
    let Some(event) = object.downcast_ref::<EventRef>() else {
        return false;
    };
    event.put(name, value.clone());
    true
}

pub(crate) fn init_event_proto(proto: &JsObject, context: &mut Context) {
    super::event_interfaces::init_base_accessors(proto, context);
    define_method(proto, "composedPath", 0, composed_path, context);
    define_method(proto, "preventDefault", 0, prevent_default, context);
    define_method(proto, "stopPropagation", 0, stop_propagation, context);
    define_method(
        proto,
        "stopImmediatePropagation",
        0,
        stop_immediate_propagation,
        context,
    );
    define_accessor(
        proto,
        "defaultPrevented",
        Some(default_prevented),
        None,
        context,
    );
    // `cancelBubble` is the legacy alias for the stop-propagation flag, and
    // delegated dispatchers still read and write it. Without it a framework's
    // own `stopPropagation` shim had nothing to set and nothing to check, so
    // propagation did not actually stop: pressing a category button inside a
    // cookie-preferences panel also ran the dismiss handler on the backdrop
    // that contains the panel, and the whole dialog closed.
    define_accessor(
        proto,
        "cancelBubble",
        Some(get_cancel_bubble),
        Some(set_cancel_bubble),
        context,
    );
    define_accessor(
        proto,
        "returnValue",
        Some(get_return_value),
        Some(set_return_value),
        context,
    );
    define_method(proto, "initEvent", 1, init_event, context);
}

pub(crate) fn set_event_path(event: &JsObject, path: Vec<JsObject>) {
    if let Some(event) = event.downcast_ref::<EventRef>() {
        *event.path.borrow_mut() = path;
    }
}

pub(crate) fn register_event_constructor(proto: &JsObject, context: &mut Context) {
    super::event_interfaces::register(proto, context);
}

fn event_ref<T>(this: &JsValue, f: impl FnOnce(&EventRef) -> T) -> JsResult<T> {
    this.as_object()
        .and_then(|obj| obj.downcast_ref::<EventRef>().map(|event| f(&event)))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Illegal invocation")
                .into()
        })
}

/// Clearing dispatch state also happens when a listener throws.
pub(crate) struct DispatchGuard {
    event: JsObject,
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        if let Some(event) = self.event.downcast_ref::<EventRef>() {
            event.dispatching.set(false);
            event.stopped.set(false);
            event.stopped_immediate.set(false);
            event.path.borrow_mut().clear();
            event.put("currentTarget", JsValue::null());
            event.put("eventPhase", JsValue::from(0));
        }
    }
}

pub(crate) fn begin_dispatch(
    event: &JsObject,
    scripted: bool,
    context: &mut Context,
) -> JsResult<DispatchGuard> {
    let invalid = event
        .downcast_ref::<EventRef>()
        .is_none_or(|event| !event.initialized.get() || event.dispatching.get());
    if invalid {
        return Err(super::event_interfaces::named_error(
            "InvalidStateError",
            "The event is uninitialized or is already being dispatched",
            context,
        ));
    }
    {
        let data = event.downcast_ref::<EventRef>().expect("validated Event");
        data.dispatching.set(true);
        if scripted {
            data.put("isTrusted", JsValue::from(false));
        }
    }
    Ok(DispatchGuard {
        event: event.clone(),
    })
}

fn composed_path(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = event_ref(this, |event| event.path.borrow().clone())?;
    Ok(JsArray::from_iter(path.into_iter().map(Into::into), context).into())
}

fn prevent_default(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    event_ref(this, |event| {
        if event.get("cancelable").to_boolean() {
            event.prevented.set(true);
        }
    })?;
    Ok(JsValue::undefined())
}

fn stop_propagation(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    event_ref(this, |event| event.stopped.set(true))?;
    Ok(JsValue::undefined())
}

fn stop_immediate_propagation(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    event_ref(this, |event| {
        event.stopped.set(true);
        event.stopped_immediate.set(true);
    })?;
    Ok(JsValue::undefined())
}

fn get_cancel_bubble(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(event_ref(this, |event| event.stopped.get())?))
}

/// Setting it to true sets the stop-propagation flag. Setting it to false does
/// nothing, per the DOM standard: the flag cannot be unset once raised.
fn set_cancel_bubble(this: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let value = args.first().unwrap_or(&JsValue::undefined()).to_boolean();
    event_ref(this, |event| {
        if value {
            event.stopped.set(true);
        }
    })?;
    Ok(JsValue::undefined())
}

fn default_prevented(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(event_ref(this, |event| {
        event.prevented.get()
    })?))
}

fn get_return_value(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(!event_ref(this, |event| {
        event.prevented.get()
    })?))
}

fn set_return_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !args.first().unwrap_or(&JsValue::undefined()).to_boolean() {
        prevent_default(this, &[], context)?;
    } else {
        event_ref(this, |_| ())?;
    }
    Ok(JsValue::undefined())
}

fn init_event(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    super::event_interfaces::legacy_init("Event", this, args, context)
}

/// Create a JS event object with the standard `Event` fields.
pub(crate) fn create_event(
    ctx: &DomCtx,
    event_type: &str,
    bubbles: bool,
    cancelable: bool,
    target: &JsValue,
    context: &mut Context,
) -> JsObject {
    let proto = ctx.state.borrow().protos().event.clone();
    let data = EventRef::default();
    data.interface.set("Event");
    data.initialized.set(true);
    let event = JsObject::from_proto_and_data(Some(proto), data);
    define_value(&event, "type", js_str(event_type), context);
    define_value(&event, "target", target.clone(), context);
    define_value(&event, "srcElement", target.clone(), context);
    define_value(&event, "currentTarget", JsValue::null(), context);
    define_value(&event, "bubbles", JsValue::from(bubbles), context);
    define_value(&event, "cancelable", JsValue::from(cancelable), context);
    define_value(&event, "composed", JsValue::from(false), context);
    define_value(&event, "isTrusted", JsValue::from(true), context);
    define_value(&event, "eventPhase", JsValue::from(0), context);
    let timestamp = super::event_interfaces::timestamp(context);
    define_value(&event, "timeStamp", JsValue::from(timestamp), context);
    super::event_interfaces::install_trust_accessor(&event, context);
    event
}

fn add_modifiers(event: &JsObject, mods: Modifiers, context: &mut Context) {
    for (name, modifier) in [
        ("ctrlKey", Modifiers::CONTROL),
        ("shiftKey", Modifiers::SHIFT),
        ("altKey", Modifiers::ALT),
        ("metaKey", Modifiers::META),
        ("modifierAltGraph", Modifiers::ALT_GRAPH),
        ("modifierCapsLock", Modifiers::CAPS_LOCK),
        ("modifierNumLock", Modifiers::NUM_LOCK),
        ("modifierScrollLock", Modifiers::SCROLL_LOCK),
    ] {
        define_value(event, name, JsValue::from(mods.contains(modifier)), context);
    }
}

fn add_pointer_fields(event: &JsObject, data: &BlitzPointerEvent, context: &mut Context) {
    let (pointer_id, pointer_type) = match data.id {
        BlitzPointerId::Mouse => (1, "mouse"),
        BlitzPointerId::Pen => (2, "pen"),
        BlitzPointerId::Finger(id) => (id.saturating_add(3), "touch"),
    };
    define_value(
        event,
        "pointerId",
        JsValue::from(pointer_id as i32),
        context,
    );
    define_value(event, "pointerType", js_str(pointer_type), context);
    define_value(event, "isPrimary", JsValue::from(data.is_primary), context);
    define_value(
        event,
        "pressure",
        JsValue::from(data.details.pressure),
        context,
    );
    define_value(
        event,
        "tangentialPressure",
        JsValue::from(data.details.tangential_pressure),
        context,
    );
    define_value(
        event,
        "tiltX",
        JsValue::from(data.details.tilt_x as i32),
        context,
    );
    define_value(
        event,
        "tiltY",
        JsValue::from(data.details.tilt_y as i32),
        context,
    );
    define_value(
        event,
        "twist",
        JsValue::from(data.details.twist as i32),
        context,
    );
    define_value(
        event,
        "altitudeAngle",
        JsValue::from(data.details.altitude),
        context,
    );
    define_value(
        event,
        "azimuthAngle",
        JsValue::from(data.details.azimuth),
        context,
    );
    define_value(
        event,
        "clientX",
        JsValue::from(data.client_x() as f64),
        context,
    );
    define_value(
        event,
        "clientY",
        JsValue::from(data.client_y() as f64),
        context,
    );
    define_value(event, "pageX", JsValue::from(data.page_x() as f64), context);
    define_value(event, "pageY", JsValue::from(data.page_y() as f64), context);
    define_value(
        event,
        "screenX",
        JsValue::from(data.screen_x() as f64),
        context,
    );
    define_value(
        event,
        "screenY",
        JsValue::from(data.screen_y() as f64),
        context,
    );
    define_value(
        event,
        "offsetX",
        JsValue::from(data.element_x() as f64),
        context,
    );
    define_value(
        event,
        "offsetY",
        JsValue::from(data.element_y() as f64),
        context,
    );
    define_value(event, "button", JsValue::from(data.button as u8), context);
    define_value(
        event,
        "buttons",
        JsValue::from(data.buttons.bits()),
        context,
    );
    let event_type = event.downcast_ref::<EventRef>().expect("Event").get("type");
    let event_type = event_type
        .as_string()
        .map(|s| s.to_std_string_lossy())
        .unwrap_or_default();
    let detail = match event_type.as_str() {
        "dblclick" => 2,
        "click" | "mousedown" | "mouseup" => 1,
        _ => 0,
    };
    define_value(event, "detail", JsValue::from(detail), context);
    define_value(
        event,
        "__which",
        JsValue::from(data.button as u8 + 1),
        context,
    );
    super::event_interfaces::add_movement(event, data, &event_type, context);
    add_modifiers(event, data.mods, context);
}

fn add_key_fields(event: &JsObject, data: &BlitzKeyEvent, context: &mut Context) {
    define_value(event, "key", js_str(&data.key.to_string()), context);
    define_value(event, "code", js_str(&data.code.to_string()), context);
    define_value(
        event,
        "location",
        JsValue::from(data.location as u32),
        context,
    );
    define_value(
        event,
        "repeat",
        JsValue::from(data.is_auto_repeating),
        context,
    );
    define_value(
        event,
        "isComposing",
        JsValue::from(data.is_composing),
        context,
    );
    super::event_interfaces::add_legacy_key_fields(event, data, context);
    add_modifiers(event, data.modifiers, context);
}

/// Create a JS event object for a Blitz [`DomEventData`], populating
/// type-specific fields (mouse coordinates, key names, etc).
pub(crate) fn create_event_for_dom_event(
    ctx: &DomCtx,
    data: &DomEventData,
    bubbles: bool,
    cancelable: bool,
    target: &JsValue,
    context: &mut Context,
) -> JsObject {
    let event = create_event(ctx, data.name(), bubbles, cancelable, target, context);
    let class = super::event_interfaces::class_for_dom_event(data);
    super::event_interfaces::initialize_native(&event, class, context);
    if super::event_interfaces::inherits(class, "UIEvent") {
        let view: JsValue = context.global_object().into();
        define_value(&event, "view", view, context);
        define_value(&event, "composed", JsValue::from(true), context);
    }

    match data {
        DomEventData::PointerMove(pointer)
        | DomEventData::PointerDown(pointer)
        | DomEventData::PointerUp(pointer)
        | DomEventData::PointerCancel(pointer)
        | DomEventData::PointerEnter(pointer)
        | DomEventData::PointerLeave(pointer)
        | DomEventData::PointerOver(pointer)
        | DomEventData::PointerOut(pointer)
        | DomEventData::MouseMove(pointer)
        | DomEventData::MouseDown(pointer)
        | DomEventData::MouseUp(pointer)
        | DomEventData::MouseEnter(pointer)
        | DomEventData::MouseLeave(pointer)
        | DomEventData::MouseOver(pointer)
        | DomEventData::MouseOut(pointer)
        | DomEventData::Click(pointer)
        | DomEventData::ContextMenu(pointer)
        | DomEventData::DoubleClick(pointer) => {
            add_pointer_fields(&event, pointer, context);
        }

        DomEventData::TouchStart(pointer)
        | DomEventData::TouchMove(pointer)
        | DomEventData::TouchEnd(pointer)
        | DomEventData::TouchCancel(pointer) => {
            super::event_interfaces::add_touch_fields(
                &event,
                pointer,
                data.name(),
                target,
                context,
            );
            add_modifiers(&event, pointer.mods, context);
        }

        DomEventData::KeyPress(key) | DomEventData::KeyDown(key) | DomEventData::KeyUp(key) => {
            add_key_fields(&event, key, context);
        }

        DomEventData::Wheel(wheel) => {
            let (delta_x, delta_y, delta_mode) = match wheel.delta {
                BlitzWheelDelta::Lines(x, y) => (x, y, 1),
                BlitzWheelDelta::Pixels(x, y) => (x, y, 0),
            };
            define_value(&event, "deltaX", JsValue::from(delta_x), context);
            define_value(&event, "deltaY", JsValue::from(delta_y), context);
            define_value(&event, "deltaZ", JsValue::from(0.0), context);
            define_value(&event, "deltaMode", JsValue::from(delta_mode), context);
            define_value(
                &event,
                "screenX",
                JsValue::from(wheel.coords.screen_x),
                context,
            );
            define_value(
                &event,
                "screenY",
                JsValue::from(wheel.coords.screen_y),
                context,
            );
            define_value(
                &event,
                "clientX",
                JsValue::from(wheel.coords.client_x),
                context,
            );
            define_value(
                &event,
                "clientY",
                JsValue::from(wheel.coords.client_y),
                context,
            );
            define_value(&event, "pageX", JsValue::from(wheel.coords.page_x), context);
            define_value(&event, "pageY", JsValue::from(wheel.coords.page_y), context);
            define_value(&event, "offsetX", JsValue::from(wheel.element.x), context);
            define_value(&event, "offsetY", JsValue::from(wheel.element.y), context);
            define_value(
                &event,
                "buttons",
                JsValue::from(wheel.buttons.bits()),
                context,
            );
            add_modifiers(&event, wheel.mods, context);
        }

        DomEventData::Focus(focus)
        | DomEventData::Blur(focus)
        | DomEventData::FocusIn(focus)
        | DomEventData::FocusOut(focus) => {
            let related = node_or_null(ctx, focus.related_target, context);
            define_value(&event, "relatedTarget", related, context);
        }

        DomEventData::Submit(submit) => {
            let submitter = if submit.submitter == submit.form {
                None
            } else {
                Some(blitz_dom::NodeId::from_u64(submit.submitter))
            };
            let submitter = node_or_null(ctx, submitter, context);
            define_value(&event, "submitter", submitter, context);
        }

        // The producer currently supplies the resulting value, not the edit.
        // Do not misreport the entire value as newly inserted text.
        DomEventData::Input(_) => {}

        _ => {}
    }

    event
}

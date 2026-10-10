//! Event interface objects and Web IDL dictionary conversion.

use std::cell::{Cell, RefCell};

use blitz_traits::events::{BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, DomEventData};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, IntegrityLevel, JsObject, ObjectInitializer};
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace, js_string,
};
use boa_gc::GcRefCell;

use super::event::{EventRef, create_event};
use super::{
    define_method, define_value, dom_ctx, js_str, node_id_of_value, this_node_id, to_rust_string,
};

const CLASSES: &[(&str, &str)] = &[
    ("Event", ""),
    ("CustomEvent", "Event"),
    ("UIEvent", "Event"),
    ("MouseEvent", "UIEvent"),
    ("PointerEvent", "MouseEvent"),
    ("KeyboardEvent", "UIEvent"),
    ("FocusEvent", "UIEvent"),
    ("InputEvent", "UIEvent"),
    ("WheelEvent", "MouseEvent"),
    ("TouchEvent", "UIEvent"),
    ("ProgressEvent", "Event"),
    ("PopStateEvent", "Event"),
    ("HashChangeEvent", "Event"),
    ("StorageEvent", "Event"),
    ("SubmitEvent", "Event"),
    ("AnimationEvent", "Event"),
    ("TransitionEvent", "Event"),
    ("PageTransitionEvent", "Event"),
    ("CompositionEvent", "UIEvent"),
    ("DragEvent", "MouseEvent"),
    ("ClipboardEvent", "Event"),
    ("MessageEvent", "Event"),
];

#[derive(Trace, Finalize, JsData)]
struct EventInterfaces {
    prototypes: Vec<JsObject>,
    touch: JsObject,
    touch_list: JsObject,
    touches: GcRefCell<Vec<JsObject>>,
    #[unsafe_ignore_trace]
    movement: RefCell<Vec<(u64, bool, f64, f64)>>,
    #[unsafe_ignore_trace]
    time_origin: web_time::Instant,
}

#[derive(Clone, Copy)]
enum Kind {
    Boolean,
    Long,
    UnsignedLong,
    Short,
    UnsignedShort,
    UnsignedLongLong,
    Double(f64),
    Float,
    String,
    NullableString,
    Any,
    Interface(&'static str),
    Touches,
    Ranges,
    MessageSource,
    Ports,
}

#[derive(Clone, Copy)]
struct Field {
    name: &'static str,
    kind: Kind,
}

macro_rules! fields {
    ($($name:literal => $kind:expr),* $(,)?) => {
        &[$(Field { name: $name, kind: $kind }),*]
    };
}

fn fields(class: &str) -> &'static [Field] {
    use Kind::*;
    match class {
        "CustomEvent" => fields!["detail" => Any],
        "UIEvent" => fields!["detail" => Long, "view" => Interface("Window")],
        "MouseEvent" => fields![
            "altKey" => Boolean, "button" => Short, "buttons" => UnsignedShort,
            "clientX" => Double(0.0), "clientY" => Double(0.0), "ctrlKey" => Boolean,
            "metaKey" => Boolean, "movementX" => Long, "movementY" => Long,
            "relatedTarget" => Interface("EventTarget"), "screenX" => Long, "screenY" => Long,
            "shiftKey" => Boolean,
            "modifierAltGraph" => Boolean, "modifierCapsLock" => Boolean,
            "modifierFn" => Boolean, "modifierFnLock" => Boolean,
            "modifierHyper" => Boolean, "modifierNumLock" => Boolean,
            "modifierScrollLock" => Boolean, "modifierSuper" => Boolean,
            "modifierSymbol" => Boolean, "modifierSymbolLock" => Boolean
        ],
        "PointerEvent" => fields![
            "altitudeAngle" => Double(std::f64::consts::FRAC_PI_2),
            "azimuthAngle" => Double(0.0), "height" => Double(1.0), "isPrimary" => Boolean,
            "pointerId" => Long, "pointerType" => String, "pressure" => Float,
            "tangentialPressure" => Float, "tiltX" => Long, "tiltY" => Long,
            "twist" => Long, "width" => Double(1.0), "persistentDeviceId" => Long
        ],
        "KeyboardEvent" => fields![
            "altKey" => Boolean, "charCode" => UnsignedLong, "code" => String,
            "ctrlKey" => Boolean, "isComposing" => Boolean, "key" => String,
            "keyCode" => UnsignedLong, "location" => UnsignedLong,
            "metaKey" => Boolean, "repeat" => Boolean, "shiftKey" => Boolean,
            "modifierAltGraph" => Boolean, "modifierCapsLock" => Boolean,
            "modifierFn" => Boolean, "modifierFnLock" => Boolean,
            "modifierHyper" => Boolean, "modifierNumLock" => Boolean,
            "modifierScrollLock" => Boolean, "modifierSuper" => Boolean,
            "modifierSymbol" => Boolean, "modifierSymbolLock" => Boolean
        ],
        "FocusEvent" => fields!["relatedTarget" => Interface("EventTarget")],
        "InputEvent" => fields![
            "data" => NullableString, "dataTransfer" => Interface("DataTransfer"),
            "inputType" => String, "isComposing" => Boolean, "targetRanges" => Ranges
        ],
        "WheelEvent" => fields![
            "deltaMode" => UnsignedLong, "deltaX" => Double(0.0),
            "deltaY" => Double(0.0), "deltaZ" => Double(0.0)
        ],
        "TouchEvent" => fields![
            "altKey" => Boolean, "changedTouches" => Touches, "ctrlKey" => Boolean,
            "metaKey" => Boolean, "shiftKey" => Boolean,
            "targetTouches" => Touches, "touches" => Touches
        ],
        "ProgressEvent" => fields![
            "lengthComputable" => Boolean, "loaded" => UnsignedLongLong,
            "total" => UnsignedLongLong
        ],
        "PopStateEvent" => fields!["state" => Any],
        "HashChangeEvent" => fields!["newURL" => String, "oldURL" => String],
        "StorageEvent" => fields![
            "key" => NullableString, "newValue" => NullableString,
            "oldValue" => NullableString, "storageArea" => Interface("Storage"), "url" => String
        ],
        "SubmitEvent" => fields!["submitter" => Interface("HTMLElement")],
        "AnimationEvent" => fields![
            "animationName" => String, "elapsedTime" => Float, "pseudoElement" => String
        ],
        "TransitionEvent" => fields![
            "elapsedTime" => Float, "propertyName" => String, "pseudoElement" => String
        ],
        "PageTransitionEvent" => fields!["persisted" => Boolean],
        "CompositionEvent" => fields!["data" => String],
        "DragEvent" => fields!["dataTransfer" => Interface("DataTransfer")],
        "ClipboardEvent" => fields!["clipboardData" => Interface("DataTransfer")],
        "MessageEvent" => fields![
            "data" => Any, "origin" => String, "lastEventId" => String,
            "source" => MessageSource, "ports" => Ports
        ],
        _ => &[],
    }
}

fn parent(class: &str) -> &'static str {
    CLASSES
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, parent)| *parent)
        .unwrap_or("")
}

pub(crate) fn inherits(mut class: &str, ancestor: &str) -> bool {
    while !class.is_empty() {
        if class == ancestor {
            return true;
        }
        class = parent(class);
    }
    false
}

fn prototype(class: &str, context: &Context) -> JsObject {
    let index = CLASSES
        .iter()
        .position(|(name, _)| *name == class)
        .expect("event interface");
    context
        .get_data::<EventInterfaces>()
        .expect("event interfaces installed")
        .prototypes[index]
        .clone()
}

pub(crate) fn timestamp(context: &Context) -> f64 {
    context
        .get_data::<EventInterfaces>()
        .map(|interfaces| interfaces.time_origin.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub(crate) fn named_error(name: &str, message: &str, context: &mut Context) -> JsError {
    crate::dom_exception::error(name, message, context)
}

fn require_event(this: &JsValue, class: &str) -> JsResult<JsObject> {
    this.as_object()
        .filter(|object| {
            object
                .downcast_ref::<EventRef>()
                .is_some_and(|event| inherits(event.interface.get(), class))
        })
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Illegal invocation")
                .into()
        })
}

fn slot(object: &JsObject, name: &str) -> JsValue {
    object
        .downcast_ref::<EventRef>()
        .expect("validated Event")
        .get(name)
}

fn accessor(proto: &JsObject, class: &'static str, name: &'static str, context: &mut Context) {
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |this, _, context| {
            let event = require_event(this, class)?;
            Ok(match name {
                "x" => slot(&event, "clientX"),
                "y" => slot(&event, "clientY"),
                "pageX" | "pageY" => {
                    let stored = slot(&event, name);
                    if !stored.is_undefined() {
                        stored
                    } else {
                        let scroll = dom_ctx(context)?.doc.borrow().viewport_scroll();
                        let (client, scroll) = if name == "pageX" {
                            ("clientX", scroll.x)
                        } else {
                            ("clientY", scroll.y)
                        };
                        JsValue::from(slot(&event, client).as_number().unwrap_or(0.0) + scroll)
                    }
                }
                "offsetX" => {
                    let stored = slot(&event, name);
                    if stored.is_undefined() {
                        slot(&event, "clientX")
                    } else {
                        stored
                    }
                }
                "offsetY" => {
                    let stored = slot(&event, name);
                    if stored.is_undefined() {
                        slot(&event, "clientY")
                    } else {
                        stored
                    }
                }
                "which" if class == "KeyboardEvent" => {
                    if slot(&event, "type")
                        .as_string()
                        .is_some_and(|s| s == js_string!("keypress"))
                    {
                        slot(&event, "charCode")
                    } else {
                        slot(&event, "keyCode")
                    }
                }
                "which" => {
                    let stored = slot(&event, "__which");
                    if stored.is_undefined() {
                        JsValue::from(0)
                    } else {
                        stored
                    }
                }
                _ => slot(&event, name),
            })
        }),
    )
    .name(JsString::from(format!("get {name}")))
    .length(0)
    .build();
    proto
        .define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .get(getter)
                .enumerable(true)
                .configurable(true),
            context,
        )
        .expect("event accessor");
}

pub(crate) fn init_base_accessors(proto: &JsObject, context: &mut Context) {
    for name in [
        "type",
        "target",
        "srcElement",
        "currentTarget",
        "eventPhase",
        "bubbles",
        "cancelable",
        "composed",
        "timeStamp",
    ] {
        accessor(proto, "Event", name, context);
    }
}

pub(crate) fn install_trust_accessor(event: &JsObject, context: &mut Context) {
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|this, _, _| {
            let event = require_event(this, "Event")?;
            Ok(slot(&event, "isTrusted"))
        }),
    )
    .name(js_string!("get isTrusted"))
    .length(0)
    .build();
    event
        .define_property_or_throw(
            js_string!("isTrusted"),
            PropertyDescriptor::builder()
                .get(getter)
                .enumerable(true)
                .configurable(false),
            context,
        )
        .expect("unforgeable isTrusted");
}

fn tag(proto: &JsObject, name: &str, context: &mut Context) {
    proto
        .define_property_or_throw(
            JsSymbol::to_string_tag(),
            PropertyDescriptor::builder()
                .value(JsString::from(name))
                .configurable(true),
            context,
        )
        .expect("interface tag");
}

fn constant(object: &JsObject, name: &str, value: u32, context: &mut Context) {
    object
        .define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().value(value).enumerable(true),
            context,
        )
        .expect("interface constant");
}

fn bind_constructor(
    name: &'static str,
    proto: &JsObject,
    function: NativeFunction,
    context: &mut Context,
) -> JsObject {
    context
        .register_global_callable(JsString::from(name), 1, function)
        .expect("event constructor");
    let constructor = context
        .global_object()
        .get(JsString::from(name), context)
        .expect("constructor registered")
        .as_object()
        .expect("constructor object");
    constructor
        .define_property_or_throw(
            js_string!("prototype"),
            PropertyDescriptor::builder()
                .value(proto.clone())
                .writable(false),
            context,
        )
        .expect("interface prototype");
    define_value(proto, "constructor", constructor.clone().into(), context);
    tag(proto, name, context);
    constructor
}

pub(crate) fn register(base: &JsObject, context: &mut Context) {
    crate::dom_exception::register(context);
    let mut prototypes: Vec<JsObject> = Vec::with_capacity(CLASSES.len());
    let mut constructors: Vec<JsObject> = Vec::with_capacity(CLASSES.len());
    for &(class, parent_class) in CLASSES {
        let proto = if class == "Event" {
            base.clone()
        } else {
            let parent_index = CLASSES
                .iter()
                .position(|(name, _)| *name == parent_class)
                .unwrap();
            JsObject::from_proto_and_data(Some(prototypes[parent_index].clone()), ())
        };
        for field in fields(class) {
            if !field.name.starts_with("modifier") && field.name != "targetRanges" {
                accessor(&proto, class, field.name, context);
            }
        }
        if class == "MouseEvent" {
            for name in ["x", "y", "pageX", "pageY", "offsetX", "offsetY", "which"] {
                accessor(&proto, class, name, context);
            }
        }
        if class == "KeyboardEvent" {
            accessor(&proto, class, "which", context);
        }
        if matches!(class, "MouseEvent" | "KeyboardEvent") {
            define_method(&proto, "getModifierState", 1, get_modifier_state, context);
        }
        if class == "InputEvent" {
            define_method(&proto, "getTargetRanges", 0, get_target_ranges, context);
        }
        if matches!(
            class,
            "CustomEvent"
                | "UIEvent"
                | "MouseEvent"
                | "KeyboardEvent"
                | "CompositionEvent"
                | "StorageEvent"
                | "MessageEvent"
        ) {
            let method = match class {
                "CustomEvent" => "initCustomEvent",
                "UIEvent" => "initUIEvent",
                "MouseEvent" => "initMouseEvent",
                "KeyboardEvent" => "initKeyboardEvent",
                "CompositionEvent" => "initCompositionEvent",
                "MessageEvent" => "initMessageEvent",
                _ => "initStorageEvent",
            };
            let function = FunctionObjectBuilder::new(
                context.realm(),
                NativeFunction::from_copy_closure(move |this, args, context| {
                    legacy_init(class, this, args, context)
                }),
            )
            .name(JsString::from(method))
            .length(1)
            .build();
            define_value(&proto, method, function.into(), context);
        }
        let constructor = bind_constructor(
            class,
            &proto,
            NativeFunction::from_copy_closure(move |new_target, args, context| {
                construct(class, new_target, args, context)
            }),
            context,
        );
        if !parent_class.is_empty() {
            let index = CLASSES
                .iter()
                .position(|(name, _)| *name == parent_class)
                .unwrap();
            constructor.set_prototype(Some(constructors[index].clone()));
        }
        let constants: &[(&str, u32)] = match class {
            "Event" => &[
                ("NONE", 0),
                ("CAPTURING_PHASE", 1),
                ("AT_TARGET", 2),
                ("BUBBLING_PHASE", 3),
            ],
            "KeyboardEvent" => &[
                ("DOM_KEY_LOCATION_STANDARD", 0),
                ("DOM_KEY_LOCATION_LEFT", 1),
                ("DOM_KEY_LOCATION_RIGHT", 2),
                ("DOM_KEY_LOCATION_NUMPAD", 3),
            ],
            "WheelEvent" => &[
                ("DOM_DELTA_PIXEL", 0),
                ("DOM_DELTA_LINE", 1),
                ("DOM_DELTA_PAGE", 2),
            ],
            _ => &[],
        };
        for &(name, value) in constants {
            constant(&proto, name, value, context);
            constant(&constructor, name, value, context);
        }
        prototypes.push(proto);
        constructors.push(constructor);
    }

    let touch = JsObject::with_object_proto(context.intrinsics());
    for field in touch_fields() {
        let name = field.name;
        let getter = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, _, _| {
                let object = this
                    .as_object()
                    .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
                let data = object
                    .downcast_ref::<TouchRef>()
                    .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
                Ok(data.get(name))
            }),
        )
        .name(JsString::from(format!("get {name}")))
        .length(0)
        .build();
        touch
            .define_property_or_throw(
                JsString::from(name),
                PropertyDescriptor::builder()
                    .get(getter)
                    .enumerable(true)
                    .configurable(true),
                context,
            )
            .expect("Touch accessor");
    }
    bind_constructor(
        "Touch",
        &touch,
        NativeFunction::from_fn_ptr(construct_touch),
        context,
    );

    let touch_list = JsObject::with_object_proto(context.intrinsics());
    bind_constructor(
        "TouchList",
        &touch_list,
        NativeFunction::from_copy_closure(|_, _, _| {
            Err(JsNativeError::typ()
                .with_message("Illegal constructor")
                .into())
        }),
        context,
    );
    let length = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|this, _, _| {
            let object = this
                .as_object()
                .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
            let list = object
                .downcast_ref::<TouchListRef>()
                .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
            Ok(JsValue::from(list.items.len() as u32))
        }),
    )
    .name(js_string!("get length"))
    .length(0)
    .build();
    touch_list
        .define_property_or_throw(
            js_string!("length"),
            PropertyDescriptor::builder()
                .get(length)
                .enumerable(true)
                .configurable(true),
            context,
        )
        .expect("TouchList length");
    define_method(&touch_list, "item", 1, touch_list_item, context);
    let iterator = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(touch_list_iterator),
    )
    .name(js_string!("values"))
    .length(0)
    .build();
    touch_list
        .define_property_or_throw(
            JsSymbol::iterator(),
            PropertyDescriptor::builder()
                .value(iterator)
                .writable(true)
                .configurable(true),
            context,
        )
        .expect("TouchList iterator");

    context.insert_data(EventInterfaces {
        prototypes,
        touch,
        touch_list,
        touches: GcRefCell::new(Vec::new()),
        movement: RefCell::new(Vec::new()),
        time_origin: web_time::Instant::now(),
    });
    let document = dom_ctx(context)
        .expect("DOM context")
        .state
        .borrow()
        .protos()
        .document
        .clone();
    define_method(&document, "createEvent", 1, create_event_legacy, context);
}

fn dictionary(value: Option<&JsValue>) -> JsResult<Option<JsObject>> {
    match value {
        None => Ok(None),
        Some(value) if value.is_null_or_undefined() => Ok(None),
        Some(value) => value.as_object().map(Some).ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Event init dictionary must be an object")
                .into()
        }),
    }
}

fn member(init: &Option<JsObject>, name: &str, context: &mut Context) -> JsResult<JsValue> {
    match init {
        Some(init) => init.get(JsString::from(name), context),
        None => Ok(JsValue::undefined()),
    }
}

fn has_interface(value: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    if name == "Window" {
        return Ok(JsObject::equals(&object, &context.global_object()));
    }
    if name == "EventTarget"
        && (node_id_of_value(value).is_some()
            || JsObject::equals(&object, &context.global_object()))
    {
        return Ok(true);
    }
    if name == "Storage" {
        for key in ["localStorage", "sessionStorage"] {
            let storage = context
                .global_object()
                .get(JsString::from(key), context)?
                .as_object();
            if storage.is_some_and(|storage| JsObject::equals(&object, &storage)) {
                return Ok(true);
            }
        }
    }
    let constructor = context
        .global_object()
        .get(JsString::from(name), context)?
        .as_object();
    let Some(constructor) = constructor else {
        return Ok(false);
    };
    let Some(expected) = constructor
        .get(js_string!("prototype"), context)?
        .as_object()
    else {
        return Ok(false);
    };
    let mut current = object.prototype();
    while let Some(proto) = current {
        if JsObject::equals(&proto, &expected) {
            return Ok(true);
        }
        current = proto.prototype();
    }
    Ok(false)
}

fn convert(kind: Kind, value: JsValue, context: &mut Context) -> JsResult<JsValue> {
    if value.is_undefined() {
        return Ok(match kind {
            Kind::Boolean => JsValue::from(false),
            Kind::String => js_str(""),
            Kind::NullableString | Kind::Any | Kind::Interface(_) | Kind::MessageSource => {
                JsValue::null()
            }
            Kind::Double(default) => JsValue::from(default),
            Kind::Touches => make_touch_list(Vec::new(), context).into(),
            Kind::Ranges => JsArray::from_iter([], context).into(),
            Kind::Ports => {
                let ports: JsObject = JsArray::from_iter([], context).into();
                ports.set_integrity_level(IntegrityLevel::Frozen, context)?;
                ports.into()
            }
            _ => JsValue::from(0),
        });
    }
    Ok(match kind {
        Kind::Boolean => JsValue::from(value.to_boolean()),
        Kind::Long => JsValue::from(value.to_i32(context)?),
        Kind::UnsignedLong => JsValue::from(value.to_u32(context)?),
        Kind::Short => JsValue::from(value.to_i32(context)? as i16 as i32),
        Kind::UnsignedShort => JsValue::from(value.to_u32(context)? as u16 as u32),
        Kind::UnsignedLongLong => {
            let number = value.to_number(context)?;
            let number = if number.is_finite() {
                number.trunc().rem_euclid(18_446_744_073_709_551_616.0)
            } else {
                0.0
            };
            JsValue::from(number)
        }
        Kind::Double(_) | Kind::Float => {
            let number = value.to_number(context)?;
            let number = if matches!(kind, Kind::Float) {
                (number as f32) as f64
            } else {
                number
            };
            if !number.is_finite() {
                return Err(JsNativeError::typ()
                    .with_message("Event numeric member must be finite")
                    .into());
            }
            JsValue::from(number)
        }
        Kind::String => value.to_string(context)?.into(),
        Kind::NullableString if value.is_null() => JsValue::null(),
        Kind::NullableString => value.to_string(context)?.into(),
        Kind::Any => value,
        Kind::Interface(_) if value.is_null() => JsValue::null(),
        Kind::Interface(name) => {
            if !has_interface(&value, name, context)? {
                return Err(JsNativeError::typ()
                    .with_message(format!("Event member requires {name}"))
                    .into());
            }
            value
        }
        Kind::Touches => {
            let items = sequence(&value, "Touch", context)?;
            make_touch_list(items, context).into()
        }
        Kind::Ranges => {
            let items = sequence(&value, "StaticRange", context)?;
            JsArray::from_iter(items.into_iter().map(Into::into), context).into()
        }
        Kind::MessageSource if value.is_null() => JsValue::null(),
        Kind::MessageSource => {
            if !has_interface(&value, "Window", context)?
                && !has_interface(&value, "MessagePort", context)?
                && !has_interface(&value, "ServiceWorker", context)?
            {
                return Err(JsNativeError::typ()
                    .with_message("Invalid MessageEvent source")
                    .into());
            }
            value
        }
        Kind::Ports => {
            let items = sequence(&value, "MessagePort", context)?;
            let ports: JsObject =
                JsArray::from_iter(items.into_iter().map(Into::into), context).into();
            ports.set_integrity_level(IntegrityLevel::Frozen, context)?;
            ports.into()
        }
    })
}

fn sequence(value: &JsValue, interface: &str, context: &mut Context) -> JsResult<Vec<JsObject>> {
    let method = value
        .to_object(context)?
        .get(JsSymbol::iterator(), context)?
        .as_callable()
        .ok_or_else(|| JsNativeError::typ().with_message("Event sequence must be iterable"))?;
    let iterator_value = method.call(value, &[], context)?;
    let iterator = iterator_value
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Iterator must be an object"))?;
    let next = iterator
        .get(js_string!("next"), context)?
        .as_object()
        .filter(JsObject::is_callable)
        .ok_or_else(|| JsNativeError::typ().with_message("Iterator next must be callable"))?;
    let mut items = Vec::new();
    loop {
        let result = next
            .call(&iterator_value, &[], context)?
            .as_object()
            .ok_or_else(|| {
                JsNativeError::typ().with_message("Iterator result must be an object")
            })?;
        if result.get(js_string!("done"), context)?.to_boolean() {
            return Ok(items);
        }
        let value = result.get(js_string!("value"), context)?;
        let valid = if interface == "Touch" {
            value
                .as_object()
                .is_some_and(|object| object.downcast_ref::<TouchRef>().is_some())
        } else {
            has_interface(&value, interface, context)?
        };
        if !valid {
            if let Some(close) = iterator
                .get(js_string!("return"), context)
                .ok()
                .and_then(|value| value.as_callable())
            {
                let _ = close.call(&iterator_value, &[], context);
            }
            return Err(JsNativeError::typ()
                .with_message(format!("Sequence member requires {interface}"))
                .into());
        }
        items.push(value.as_object().expect("validated object"));
    }
}

fn initialize_fields(
    event: &JsObject,
    class: &'static str,
    init: &Option<JsObject>,
    context: &mut Context,
) -> JsResult<()> {
    let parent_class = parent(class);
    if !parent_class.is_empty() {
        initialize_fields(event, parent_class, init, context)?;
    }
    for field in fields(class) {
        let value = member(init, field.name, context)?;
        let value = convert(field.kind, value, context)?;
        define_value(event, field.name, value, context);
    }
    Ok(())
}

fn construct(
    class: &'static str,
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let constructor = new_target
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Event constructor requires new"))?;
    let event_type = args
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("Event constructor requires a type"))?;
    let event_type = to_rust_string(event_type, context)?;
    let init = dictionary(args.get(1))?;
    let bubbles = member(&init, "bubbles", context)?.to_boolean();
    let cancelable = member(&init, "cancelable", context)?.to_boolean();
    let composed = member(&init, "composed", context)?.to_boolean();
    let ctx = dom_ctx(context)?;
    let event = create_event(
        &ctx,
        &event_type,
        bubbles,
        cancelable,
        &JsValue::null(),
        context,
    );
    event
        .downcast_ref::<EventRef>()
        .expect("Event")
        .interface
        .set(class);
    initialize_fields(&event, class, &init, context)?;
    let proto = constructor
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or_else(|| prototype(class, context));
    event.set_prototype(Some(proto));
    define_value(&event, "composed", JsValue::from(composed), context);
    define_value(&event, "isTrusted", JsValue::from(false), context);
    Ok(event.into())
}

pub(crate) fn initialize_native(event: &JsObject, class: &'static str, context: &mut Context) {
    event
        .downcast_ref::<EventRef>()
        .expect("Event")
        .interface
        .set(class);
    event.set_prototype(Some(prototype(class, context)));
    initialize_fields(event, class, &None, context).expect("default event dictionary");
}

pub(crate) fn class_for_dom_event(data: &DomEventData) -> &'static str {
    match data {
        DomEventData::PointerMove(_)
        | DomEventData::PointerDown(_)
        | DomEventData::PointerUp(_)
        | DomEventData::PointerCancel(_)
        | DomEventData::PointerEnter(_)
        | DomEventData::PointerLeave(_)
        | DomEventData::PointerOver(_)
        | DomEventData::PointerOut(_)
        | DomEventData::Click(_)
        | DomEventData::ContextMenu(_) => "PointerEvent",
        DomEventData::MouseMove(_)
        | DomEventData::MouseDown(_)
        | DomEventData::MouseUp(_)
        | DomEventData::MouseEnter(_)
        | DomEventData::MouseLeave(_)
        | DomEventData::MouseOver(_)
        | DomEventData::MouseOut(_)
        | DomEventData::DoubleClick(_) => "MouseEvent",
        DomEventData::KeyDown(_) | DomEventData::KeyUp(_) | DomEventData::KeyPress(_) => {
            "KeyboardEvent"
        }
        DomEventData::Wheel(_) => "WheelEvent",
        DomEventData::Focus(_)
        | DomEventData::Blur(_)
        | DomEventData::FocusIn(_)
        | DomEventData::FocusOut(_) => "FocusEvent",
        DomEventData::Input(_) => "InputEvent",
        DomEventData::TouchStart(_)
        | DomEventData::TouchMove(_)
        | DomEventData::TouchEnd(_)
        | DomEventData::TouchCancel(_) => "TouchEvent",
        DomEventData::Submit(_) => "SubmitEvent",
        _ => "Event",
    }
}

fn get_modifier_state(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let event = require_event(this, "UIEvent")?;
    let class = event
        .downcast_ref::<EventRef>()
        .expect("Event")
        .interface
        .get();
    if !inherits(class, "MouseEvent") && class != "KeyboardEvent" {
        return Err(JsNativeError::typ()
            .with_message("Illegal invocation")
            .into());
    }
    let name = args
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("Modifier name required"))?;
    let name = to_rust_string(name, context)?;
    let field = match name.as_str() {
        "Alt" => "altKey",
        "Control" => "ctrlKey",
        "Meta" => "metaKey",
        "Shift" => "shiftKey",
        "AltGraph" => "modifierAltGraph",
        "CapsLock" => "modifierCapsLock",
        "Fn" => "modifierFn",
        "FnLock" => "modifierFnLock",
        "Hyper" => "modifierHyper",
        "NumLock" => "modifierNumLock",
        "ScrollLock" => "modifierScrollLock",
        "Super" => "modifierSuper",
        "Symbol" => "modifierSymbol",
        "SymbolLock" => "modifierSymbolLock",
        _ => return Ok(JsValue::from(false)),
    };
    Ok(JsValue::from(slot(&event, field).to_boolean()))
}

fn get_target_ranges(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let event = require_event(this, "InputEvent")?;
    let array = slot(&event, "targetRanges")
        .as_object()
        .expect("internal range sequence");
    let length = array
        .get(js_string!("length"), context)?
        .to_length(context)?;
    let mut values = Vec::with_capacity(length as usize);
    for index in 0..length {
        values.push(array.get(index as u32, context)?);
    }
    Ok(JsArray::from_iter(values, context).into())
}

fn find_field(mut class: &str, name: &str) -> Field {
    while !class.is_empty() {
        if let Some(field) = fields(class).iter().find(|field| field.name == name) {
            return *field;
        }
        class = parent(class);
    }
    panic!("legacy initializer field")
}

pub(crate) fn legacy_init(
    class: &'static str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let event = require_event(this, class)?;
    let event_type = args
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("Event type required"))?;
    let event_type = event_type.to_string(context)?;
    let bubbles = args.get(1).is_some_and(JsValue::to_boolean);
    let cancelable = args.get(2).is_some_and(JsValue::to_boolean);
    let names: &[&str] = match class {
        "CustomEvent" => &["detail"],
        "UIEvent" => &["view", "detail"],
        "MouseEvent" => &[
            "view",
            "detail",
            "screenX",
            "screenY",
            "clientX",
            "clientY",
            "ctrlKey",
            "altKey",
            "shiftKey",
            "metaKey",
            "button",
            "relatedTarget",
        ],
        "KeyboardEvent" => &[
            "view", "key", "location", "ctrlKey", "altKey", "shiftKey", "metaKey",
        ],
        "CompositionEvent" => &["view", "data"],
        "StorageEvent" => &["key", "oldValue", "newValue", "url", "storageArea"],
        "MessageEvent" => &["data", "origin", "lastEventId", "source", "ports"],
        _ => &[],
    };
    let mut values = Vec::with_capacity(names.len());
    for (index, &name) in names.iter().enumerate() {
        let field = find_field(class, name);
        let value = args.get(index + 3).cloned().unwrap_or_default();
        values.push((name, convert(field.kind, value, context)?));
    }
    let data = event.downcast_ref::<EventRef>().expect("Event");
    if data.dispatching.get() {
        return Ok(JsValue::undefined());
    }
    data.initialized.set(true);
    data.prevented.set(false);
    data.stopped.set(false);
    data.stopped_immediate.set(false);
    data.path.borrow_mut().clear();
    data.put("type", event_type.into());
    data.put("bubbles", JsValue::from(bubbles));
    data.put("cancelable", JsValue::from(cancelable));
    data.put("composed", JsValue::from(false));
    data.put("isTrusted", JsValue::from(false));
    data.put("target", JsValue::null());
    data.put("srcElement", JsValue::null());
    data.put("currentTarget", JsValue::null());
    data.put("eventPhase", JsValue::from(0));
    for (name, value) in values {
        data.put(name, value);
    }
    if class == "MouseEvent" {
        data.put("buttons", JsValue::from(0));
        data.put("movementX", JsValue::from(0));
        data.put("movementY", JsValue::from(0));
    }
    Ok(JsValue::undefined())
}

fn create_event_legacy(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _ = this_node_id(this)?;
    let name = args
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("Event interface required"))?;
    let name = to_rust_string(name, context)?.to_ascii_lowercase();
    let class = match name.as_str() {
        "event" | "events" | "htmlevents" | "svgevents" => "Event",
        "customevent" => "CustomEvent",
        "uievent" | "uievents" => "UIEvent",
        "mouseevent" | "mouseevents" => "MouseEvent",
        "keyboardevent" => "KeyboardEvent",
        "focusevent" => "FocusEvent",
        "compositionevent" => "CompositionEvent",
        "dragevent" => "DragEvent",
        "touchevent" => "TouchEvent",
        "hashchangeevent" => "HashChangeEvent",
        "storageevent" => "StorageEvent",
        "messageevent" => "MessageEvent",
        _ => {
            return Err(named_error(
                "NotSupportedError",
                "Unsupported legacy event interface",
                context,
            ));
        }
    };
    let ctx = dom_ctx(context)?;
    let event = create_event(&ctx, "", false, false, &JsValue::null(), context);
    initialize_native(&event, class, context);
    {
        let data = event.downcast_ref::<EventRef>().expect("Event");
        data.initialized.set(false);
        data.put("isTrusted", JsValue::from(false));
    }
    Ok(event.into())
}

#[derive(Trace, Finalize, JsData)]
struct TouchRef {
    values: Vec<(JsString, JsValue)>,
}

impl TouchRef {
    fn get(&self, name: &str) -> JsValue {
        let key = JsString::from(name);
        self.values
            .iter()
            .find(|(stored, _)| stored == &key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }
}

fn touch_fields() -> &'static [Field] {
    use Kind::*;
    fields![
        "identifier" => Long, "target" => Interface("EventTarget"),
        "clientX" => Double(0.0), "clientY" => Double(0.0),
        "screenX" => Double(0.0), "screenY" => Double(0.0),
        "pageX" => Double(0.0), "pageY" => Double(0.0),
        "radiusX" => Float, "radiusY" => Float, "rotationAngle" => Float, "force" => Float,
        "altitudeAngle" => Double(0.0), "azimuthAngle" => Double(0.0), "touchType" => String
    ]
}

fn construct_touch(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let constructor = new_target
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Touch constructor requires new"))?;
    let init = dictionary(args.first())?;
    let identifier = member(&init, "identifier", context)?;
    let target = member(&init, "target", context)?;
    if identifier.is_undefined() || target.is_null_or_undefined() {
        return Err(JsNativeError::typ()
            .with_message("Touch requires identifier and target")
            .into());
    }
    let mut values = Vec::with_capacity(touch_fields().len());
    for field in touch_fields() {
        let value = match field.name {
            "identifier" => identifier.clone(),
            "target" => target.clone(),
            _ => member(&init, field.name, context)?,
        };
        let value = if field.name == "touchType" && value.is_undefined() {
            js_str("direct")
        } else {
            convert(field.kind, value, context)?
        };
        if field.name == "touchType" {
            let name = value
                .as_string()
                .expect("converted string")
                .to_std_string_lossy();
            if !matches!(name.as_str(), "direct" | "stylus") {
                return Err(JsNativeError::typ()
                    .with_message("Invalid TouchType")
                    .into());
            }
        }
        values.push((JsString::from(field.name), value));
    }
    let default = context
        .get_data::<EventInterfaces>()
        .expect("event interfaces")
        .touch
        .clone();
    let proto = constructor
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or(default);
    Ok(JsObject::from_proto_and_data(Some(proto), TouchRef { values }).into())
}

#[derive(Trace, Finalize, JsData)]
struct TouchListRef {
    items: Vec<JsObject>,
}

fn make_touch_list(items: Vec<JsObject>, context: &mut Context) -> JsObject {
    let proto = context
        .get_data::<EventInterfaces>()
        .expect("event interfaces")
        .touch_list
        .clone();
    let list = JsObject::from_proto_and_data(
        Some(proto),
        TouchListRef {
            items: items.clone(),
        },
    );
    for (index, touch) in items.into_iter().enumerate() {
        list.define_property_or_throw(
            index as u32,
            PropertyDescriptor::builder().value(touch).enumerable(true),
            context,
        )
        .expect("TouchList index");
    }
    list
}

fn touch_list_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let index = args
        .first()
        .ok_or_else(|| JsNativeError::typ().with_message("Index required"))?
        .to_u32(context)?;
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
    let list = object
        .downcast_ref::<TouchListRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
    Ok(list
        .items
        .get(index as usize)
        .cloned()
        .map(Into::into)
        .unwrap_or_else(JsValue::null))
}

#[derive(Trace, Finalize, JsData)]
struct TouchIteratorRef {
    items: Vec<JsObject>,
    #[unsafe_ignore_trace]
    index: Cell<usize>,
}

fn touch_list_iterator(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
    let items = object
        .downcast_ref::<TouchListRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?
        .items
        .clone();
    let iterator = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        TouchIteratorRef {
            items,
            index: Cell::new(0),
        },
    );
    define_method(&iterator, "next", 0, touch_iterator_next, context);
    let identity = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|this, _, _| Ok(this.clone())),
    )
    .length(0)
    .build();
    iterator.define_property_or_throw(
        JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(identity)
            .writable(true)
            .configurable(true),
        context,
    )?;
    Ok(iterator.into())
}

fn touch_iterator_next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
    let (value, done) = {
        let iterator = object
            .downcast_ref::<TouchIteratorRef>()
            .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
        let index = iterator.index.get();
        let value = iterator.items.get(index).cloned();
        iterator.index.set(index.saturating_add(1));
        let done = value.is_none();
        (value.map(JsValue::from).unwrap_or_default(), done)
    };
    Ok(ObjectInitializer::new(context)
        .property(js_string!("value"), value, Attribute::all())
        .property(js_string!("done"), done, Attribute::all())
        .build()
        .into())
}

fn pointer_id(pointer: &BlitzPointerEvent) -> u64 {
    match pointer.id {
        BlitzPointerId::Mouse => 1,
        BlitzPointerId::Pen => 2,
        BlitzPointerId::Finger(id) => id.saturating_add(3),
    }
}

pub(crate) fn add_movement(
    event: &JsObject,
    pointer: &BlitzPointerEvent,
    event_type: &str,
    context: &mut Context,
) {
    if !matches!(event_type, "mousemove" | "pointermove") {
        return;
    }
    let id = pointer_id(pointer);
    let mouse = event_type == "mousemove";
    let x = pointer.screen_x() as f64;
    let y = pointer.screen_y() as f64;
    let (dx, dy) = {
        let interfaces = context
            .get_data::<EventInterfaces>()
            .expect("event interfaces");
        let mut movement = interfaces.movement.borrow_mut();
        if let Some((_, _, previous_x, previous_y)) = movement
            .iter_mut()
            .find(|(stored, stored_mouse, _, _)| *stored == id && *stored_mouse == mouse)
        {
            let delta = (x - *previous_x, y - *previous_y);
            *previous_x = x;
            *previous_y = y;
            delta
        } else {
            movement.push((id, mouse, x, y));
            (0.0, 0.0)
        }
    };
    define_value(event, "movementX", JsValue::from(dx), context);
    define_value(event, "movementY", JsValue::from(dy), context);
}

pub(crate) fn add_touch_fields(
    event: &JsObject,
    pointer: &BlitzPointerEvent,
    event_type: &str,
    target: &JsValue,
    context: &mut Context,
) {
    let identifier = pointer_id(pointer) as i32;
    let mut active = context
        .get_data::<EventInterfaces>()
        .expect("event interfaces")
        .touches
        .borrow()
        .clone();
    let index = active.iter().position(|touch| {
        touch
            .downcast_ref::<TouchRef>()
            .expect("Touch")
            .get("identifier")
            .as_number()
            == Some(identifier as f64)
    });
    let original_target = index
        .map(|index| {
            active[index]
                .downcast_ref::<TouchRef>()
                .expect("Touch")
                .get("target")
        })
        .unwrap_or_else(|| target.clone());
    let mut values = Vec::with_capacity(touch_fields().len());
    for field in touch_fields() {
        let value = match field.name {
            "identifier" => JsValue::from(identifier),
            "target" => original_target.clone(),
            "clientX" => JsValue::from(pointer.client_x()),
            "clientY" => JsValue::from(pointer.client_y()),
            "screenX" => JsValue::from(pointer.screen_x()),
            "screenY" => JsValue::from(pointer.screen_y()),
            "pageX" => JsValue::from(pointer.page_x()),
            "pageY" => JsValue::from(pointer.page_y()),
            "force" => JsValue::from(pointer.details.pressure),
            "altitudeAngle" => JsValue::from(pointer.details.altitude),
            "azimuthAngle" => JsValue::from(pointer.details.azimuth),
            "touchType" => js_str(if matches!(pointer.id, BlitzPointerId::Pen) {
                "stylus"
            } else {
                "direct"
            }),
            _ => JsValue::from(0),
        };
        values.push((JsString::from(field.name), value));
    }
    let proto = context
        .get_data::<EventInterfaces>()
        .expect("event interfaces")
        .touch
        .clone();
    let changed = JsObject::from_proto_and_data(Some(proto), TouchRef { values });
    if matches!(event_type, "touchend" | "touchcancel") {
        if let Some(index) = index {
            active.remove(index);
        }
    } else if let Some(index) = index {
        active[index] = changed.clone();
    } else {
        active.push(changed.clone());
    }
    let target_touches = active
        .iter()
        .filter(|touch| {
            let touch_target = touch
                .downcast_ref::<TouchRef>()
                .expect("Touch")
                .get("target");
            match (touch_target.as_object(), target.as_object()) {
                (Some(left), Some(right)) => JsObject::equals(&left, &right),
                _ => false,
            }
        })
        .cloned()
        .collect();
    *context
        .get_data::<EventInterfaces>()
        .expect("event interfaces")
        .touches
        .borrow_mut() = active.clone();
    let touches = make_touch_list(active, context);
    let target_touches = make_touch_list(target_touches, context);
    let changed_touches = make_touch_list(vec![changed], context);
    define_value(event, "touches", touches.into(), context);
    define_value(event, "targetTouches", target_touches.into(), context);
    define_value(event, "changedTouches", changed_touches.into(), context);
}

pub(crate) fn add_legacy_key_fields(event: &JsObject, key: &BlitzKeyEvent, context: &mut Context) {
    let event_type = slot(event, "type")
        .as_string()
        .map(|s| s.to_std_string_lossy())
        .unwrap_or_default();
    let key_name = key.key.to_string();
    let code = key.code.to_string();
    let char_code = if event_type == "keypress" {
        key.text
            .as_ref()
            .map(|text| text.as_str())
            .unwrap_or(&key_name)
            .encode_utf16()
            .next()
            .unwrap_or(0) as u32
    } else {
        0
    };
    let key_code = if event_type == "keypress" {
        char_code
    } else {
        match key_name.as_str() {
            "Backspace" => 8,
            "Tab" => 9,
            "Enter" => 13,
            "Shift" => 16,
            "Control" => 17,
            "Alt" => 18,
            "Pause" => 19,
            "CapsLock" => 20,
            "Escape" => 27,
            " " => 32,
            "PageUp" => 33,
            "PageDown" => 34,
            "End" => 35,
            "Home" => 36,
            "ArrowLeft" => 37,
            "ArrowUp" => 38,
            "ArrowRight" => 39,
            "ArrowDown" => 40,
            "PrintScreen" => 44,
            "Insert" => 45,
            "Delete" => 46,
            "Meta" => 91,
            "ContextMenu" => 93,
            "NumLock" => 144,
            "ScrollLock" => 145,
            _ if key_name.len() == 1 && key_name.as_bytes()[0].is_ascii_alphanumeric() => {
                key_name.as_bytes()[0].to_ascii_uppercase() as u32
            }
            _ => match code.as_str() {
                "Semicolon" => 186,
                "Equal" => 187,
                "Comma" => 188,
                "Minus" => 189,
                "Period" => 190,
                "Slash" => 191,
                "Backquote" => 192,
                "BracketLeft" => 219,
                "Backslash" => 220,
                "BracketRight" => 221,
                "Quote" => 222,
                "NumpadMultiply" => 106,
                "NumpadAdd" => 107,
                "NumpadSubtract" => 109,
                "NumpadDecimal" => 110,
                "NumpadDivide" => 111,
                _ if code.starts_with("Numpad") => code[6..]
                    .parse::<u32>()
                    .ok()
                    .filter(|digit| *digit <= 9)
                    .map(|digit| 96 + digit)
                    .unwrap_or(0),
                _ if key_name.starts_with('F') => key_name[1..]
                    .parse::<u32>()
                    .ok()
                    .filter(|number| (1..=24).contains(number))
                    .map(|number| 111 + number)
                    .unwrap_or(0),
                _ => 0,
            },
        }
    };
    define_value(event, "keyCode", JsValue::from(key_code), context);
    define_value(event, "charCode", JsValue::from(char_code), context);
}

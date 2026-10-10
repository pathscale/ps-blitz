//! DOMException is shared by document and document-free worker realms.

use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace, js_string,
};

#[derive(Trace, Finalize, JsData)]
struct ExceptionPrototype {
    prototype: JsObject,
}

#[derive(Trace, Finalize, JsData)]
struct ExceptionRef {
    name: JsString,
    message: JsString,
}

const LEGACY: &[(&str, &str, u32)] = &[
    ("INDEX_SIZE_ERR", "IndexSizeError", 1),
    ("DOMSTRING_SIZE_ERR", "", 2),
    ("HIERARCHY_REQUEST_ERR", "HierarchyRequestError", 3),
    ("WRONG_DOCUMENT_ERR", "WrongDocumentError", 4),
    ("INVALID_CHARACTER_ERR", "InvalidCharacterError", 5),
    ("NO_DATA_ALLOWED_ERR", "", 6),
    (
        "NO_MODIFICATION_ALLOWED_ERR",
        "NoModificationAllowedError",
        7,
    ),
    ("NOT_FOUND_ERR", "NotFoundError", 8),
    ("NOT_SUPPORTED_ERR", "NotSupportedError", 9),
    ("INUSE_ATTRIBUTE_ERR", "InUseAttributeError", 10),
    ("INVALID_STATE_ERR", "InvalidStateError", 11),
    ("SYNTAX_ERR", "SyntaxError", 12),
    ("INVALID_MODIFICATION_ERR", "InvalidModificationError", 13),
    ("NAMESPACE_ERR", "NamespaceError", 14),
    ("INVALID_ACCESS_ERR", "InvalidAccessError", 15),
    ("VALIDATION_ERR", "", 16),
    ("TYPE_MISMATCH_ERR", "TypeMismatchError", 17),
    ("SECURITY_ERR", "SecurityError", 18),
    ("NETWORK_ERR", "NetworkError", 19),
    ("ABORT_ERR", "AbortError", 20),
    ("URL_MISMATCH_ERR", "URLMismatchError", 21),
    ("QUOTA_EXCEEDED_ERR", "QuotaExceededError", 22),
    ("TIMEOUT_ERR", "TimeoutError", 23),
    ("INVALID_NODE_TYPE_ERR", "InvalidNodeTypeError", 24),
    ("DATA_CLONE_ERR", "DataCloneError", 25),
];

fn create(prototype: JsObject, name: JsString, message: JsString) -> JsObject {
    JsObject::from_proto_and_data(Some(prototype), ExceptionRef { name, message })
}

/// Native brand check, including instances made through subclass constructors.
pub fn is_instance(value: &JsValue) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.downcast_ref::<ExceptionRef>().is_some())
}

/// Construct a platform exception without consulting mutable JavaScript globals.
pub fn error(name: &str, message: &str, context: &mut Context) -> JsError {
    register(context);
    let prototype = context
        .get_data::<ExceptionPrototype>()
        .expect("DOMException prototype")
        .prototype
        .clone();
    JsError::from_opaque(create(prototype, JsString::from(name), JsString::from(message)).into())
}

fn construct(new_target: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = new_target
        .as_object()
        .filter(|object| object.is_constructor())
        .ok_or_else(|| JsNativeError::typ().with_message("DOMException requires 'new'"))?;
    let message = match args.first() {
        Some(value) if !value.is_undefined() => value.to_string(context)?,
        _ => js_string!(""),
    };
    let name = match args.get(1) {
        Some(value) if !value.is_undefined() => value.to_string(context)?,
        _ => js_string!("Error"),
    };
    let prototype = target
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or_else(|| {
            context
                .get_data::<ExceptionPrototype>()
                .expect("DOMException prototype")
                .prototype
                .clone()
        });
    Ok(create(prototype, name, message).into())
}

/// Install once per context. No DOM context or host resources are required.
pub fn register(context: &mut Context) {
    if context.get_data::<ExceptionPrototype>().is_some() {
        return;
    }
    let prototype = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().error().prototype()),
        (),
    );
    for field in ["name", "message", "code"] {
        let getter = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, _, _| {
                let object = this
                    .as_object()
                    .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
                let data = object
                    .downcast_ref::<ExceptionRef>()
                    .ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
                Ok(match field {
                    "name" => data.name.clone().into(),
                    "message" => data.message.clone().into(),
                    _ => LEGACY
                        .iter()
                        .find(|(_, name, _)| !name.is_empty() && data.name == JsString::from(*name))
                        .map_or(0, |(_, _, code)| *code)
                        .into(),
                })
            }),
        )
        .name(JsString::from(format!("get {field}")))
        .length(0)
        .build();
        prototype
            .define_property_or_throw(
                JsString::from(field),
                PropertyDescriptor::builder()
                    .get(getter)
                    .enumerable(true)
                    .configurable(true),
                context,
            )
            .expect("DOMException accessor");
    }
    prototype
        .define_property_or_throw(
            JsSymbol::to_string_tag(),
            PropertyDescriptor::builder()
                .value(js_string!("DOMException"))
                .configurable(true),
            context,
        )
        .expect("DOMException tag");
    let constructor =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(construct))
            .name(js_string!("DOMException"))
            .length(0)
            .constructor(true)
            .build();
    constructor
        .define_property_or_throw(
            js_string!("prototype"),
            PropertyDescriptor::builder().value(prototype.clone()),
            context,
        )
        .expect("DOMException constructor prototype");
    prototype
        .define_property_or_throw(
            js_string!("constructor"),
            PropertyDescriptor::builder()
                .value(constructor.clone())
                .writable(true)
                .configurable(true),
            context,
        )
        .expect("DOMException prototype constructor");
    for &(constant, _, code) in LEGACY {
        for object in [&prototype, &constructor] {
            object
                .define_property_or_throw(
                    JsString::from(constant),
                    PropertyDescriptor::builder().value(code).enumerable(true),
                    context,
                )
                .expect("DOMException constant");
        }
    }
    context.insert_data(ExceptionPrototype { prototype });
    context
        .register_global_property(
            js_string!("DOMException"),
            constructor,
            Attribute::WRITABLE | Attribute::CONFIGURABLE,
        )
        .expect("DOMException global");
}

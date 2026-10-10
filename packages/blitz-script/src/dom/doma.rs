//! Attribute identities and DOM tree operations.

pub(crate) mod attr;
pub(crate) mod traversal;
pub(crate) mod tree;

use std::cell::{Cell, RefCell};
use std::sync::{RwLock, Weak};

use blitz_dom::node::AttributeNode;
use boa_engine::object::{FunctionObjectBuilder, JsObject, ObjectInitializer, WeakJsObject};
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace, js_string,
};

use super::{define_method, define_value};
use crate::state::DomCtx;

#[derive(Trace, Finalize, JsData)]
pub(crate) struct DomaState {
    pub attr_proto: JsObject,
    pub map_proto: JsObject,
    #[unsafe_ignore_trace]
    pub attrs: RefCell<Vec<(Weak<RwLock<AttributeNode>>, WeakJsObject)>>,
}

pub(crate) fn install(ctx: &DomCtx, context: &mut Context) {
    let (node, element, document, character_data) = {
        let state = ctx.state.borrow();
        let protos = state.protos();
        (
            protos.node.clone(),
            protos.element.clone(),
            protos.document.clone(),
            protos.character_data.clone(),
        )
    };
    let attr_proto = JsObject::from_proto_and_data(Some(node.clone()), ());
    let map_proto = JsObject::with_object_proto(context.intrinsics());
    // The DocumentFragment interface prototype registered by `interfaces`, so
    // fragment wrappers (which take that prototype) get these methods.
    let fragment_proto = super::interfaces::prototype("DocumentFragment", context);
    context.insert_data(DomaState {
        attr_proto: attr_proto.clone(),
        map_proto: map_proto.clone(),
        attrs: RefCell::new(Vec::new()),
    });
    tree::install_node(&node, context);
    tree::install_parent(&element, context);
    tree::install_parent(&document, context);
    tree::install_parent(&fragment_proto, context);
    tree::install_fragment(&fragment_proto, context);
    tree::install_child(&element, context);
    tree::install_child(&character_data, context);
    attr::install(&attr_proto, &map_proto, &element, &document, context);
    interface("Attr", &attr_proto, context);
    interface("NamedNodeMap", &map_proto, context);
    traversal::install(&document, context);
}

fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("Illegal constructor")
        .into())
}

fn interface(name: &str, proto: &JsObject, context: &mut Context) {
    context
        .register_global_callable(
            JsString::from(name),
            0,
            NativeFunction::from_fn_ptr(illegal_constructor),
        )
        .expect("failed to register DOMA interface");
    let constructor = context
        .global_object()
        .clone()
        .get(JsString::from(name), context)
        .expect("DOMA constructor missing")
        .as_object()
        .expect("DOMA constructor must be an object");
    constructor
        .set(js_string!("prototype"), proto.clone(), true, context)
        .expect("failed to set DOMA prototype");
    define_value(proto, "constructor", constructor.into(), context);
    proto
        .define_property_or_throw(
            JsSymbol::to_string_tag(),
            PropertyDescriptor::builder()
                .value(JsString::from(name))
                .writable(false)
                .enumerable(false)
                .configurable(true)
                .build(),
            context,
        )
        .expect("failed to define DOMA toStringTag");
}

pub(crate) fn dom_error(name: &str, message: &str, context: &mut Context) -> JsError {
    let error = ObjectInitializer::new(context)
        .property(js_string!("name"), JsString::from(name), Attribute::all())
        .property(
            js_string!("message"),
            JsString::from(message),
            Attribute::all(),
        )
        .build();
    JsError::from_opaque(error.into())
}

pub(crate) fn index_arg(args: &[JsValue], context: &mut Context) -> JsResult<usize> {
    let number = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)?;
    Ok(if number.is_finite() {
        number.trunc().rem_euclid(4_294_967_296.0) as usize
    } else {
        0
    })
}

#[derive(Trace, Finalize, JsData)]
struct ListIterator {
    source: JsObject,
    #[unsafe_ignore_trace]
    index: Cell<usize>,
}

pub(crate) fn install_iterator(proto: &JsObject, context: &mut Context) {
    let function =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(list_iterator))
            .name(js_string!("values"))
            .length(0)
            .build();
    proto
        .define_property_or_throw(
            JsSymbol::iterator(),
            PropertyDescriptor::builder()
                .value(function)
                .writable(true)
                .enumerable(false)
                .configurable(true)
                .build(),
            context,
        )
        .expect("failed to install DOMA iterator");
}

fn list_iterator(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let source = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("iterator receiver is not an object"))?;
    let iterator = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        ListIterator {
            source,
            index: Cell::new(0),
        },
    );
    define_method(&iterator, "next", 0, iterator_next, context);
    let identity = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(iterator_identity),
    )
    .build();
    iterator.define_property_or_throw(
        JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(identity)
            .writable(true)
            .configurable(true)
            .build(),
        context,
    )?;
    Ok(iterator.into())
}

fn iterator_identity(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(this.clone())
}

fn iterator_next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("invalid iterator"))?;
    let (source, index) = {
        let data = object
            .downcast_ref::<ListIterator>()
            .ok_or_else(|| JsNativeError::typ().with_message("invalid iterator"))?;
        (data.source.clone(), data.index.get())
    };
    let length = source
        .get(js_string!("length"), context)?
        .to_number(context)?;
    let done = index as f64 >= length;
    let value = if done {
        JsValue::undefined()
    } else {
        source.get(JsString::from(index.to_string()), context)?
    };
    if !done {
        object
            .downcast_ref::<ListIterator>()
            .expect("checked iterator")
            .index
            .set(index + 1);
    }
    Ok(ObjectInitializer::new(context)
        .property(js_string!("value"), value, Attribute::all())
        .property(js_string!("done"), done, Attribute::all())
        .build()
        .into())
}

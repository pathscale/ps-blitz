//! Live DOM collections. Owners and snapshot members are traced JS wrappers.

use std::sync::Arc;

use blitz_dom::NodeId;
use boa_engine::object::builtins::{JsArray, JsProxy};
use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::{Attribute, PropertyDescriptor, PropertyKey};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace,
};

use super::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_wrapper, this_node_id,
};

#[derive(Clone)]
enum Source {
    Snapshot,
    Children {
        elements: bool,
    },
    Tag {
        name: Arc<str>,
        namespace: Option<Arc<str>>,
    },
    Class(Arc<[Arc<str>]>),
    Tokens,
}

#[derive(Trace, Finalize, JsData)]
struct Collection {
    owner: Option<JsObject>,
    items: Vec<JsValue>,
    #[unsafe_ignore_trace]
    source: Source,
    #[unsafe_ignore_trace]
    named: bool,
}

#[derive(Trace, Finalize, JsData)]
struct CollectionKey {
    symbol: JsSymbol,
}

fn record(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid collection receiver"))?;
    if object.downcast_ref::<Collection>().is_some() {
        return Ok(object);
    }
    let key = context
        .get_data::<CollectionKey>()
        .expect("missing collection key")
        .symbol
        .clone();
    object
        .get(key, context)?
        .as_object()
        .filter(|object| object.downcast_ref::<Collection>().is_some())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid collection receiver")
                .into()
        })
}

fn split_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in value.split(|character| matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' '))
    {
        if !token.is_empty() && !tokens.iter().any(|existing| existing == token) {
            tokens.push(token.to_owned());
        }
    }
    tokens
}

fn values(this: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let object = record(this, context)?;
    let (source, owner, items) = {
        let data = object
            .downcast_ref::<Collection>()
            .expect("validated collection");
        (data.source.clone(), data.owner.clone(), data.items.clone())
    };
    if matches!(source, Source::Snapshot) {
        return Ok(items);
    }
    let owner = owner.expect("live collection without owner");
    let owner_id = this_node_id(&owner.clone().into())?;
    let ctx = dom_ctx(context)?;
    if matches!(source, Source::Tokens) {
        let doc = ctx.doc.borrow();
        let class = doc
            .get_node(owner_id)
            .and_then(|node| node.attr(blitz_dom::local_name!("class")))
            .unwrap_or_default();
        return Ok(split_tokens(class)
            .iter()
            .map(|token| js_str(token))
            .collect());
    }
    let ids: Vec<NodeId> = {
        let doc = ctx.doc.borrow();
        let mut result = Vec::new();
        let mut stack: Vec<_> = doc
            .get_node(owner_id)
            .map(|node| node.children.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            let Some(node) = doc.get_node(id) else {
                continue;
            };
            let element = node.element_data();
            let selected = match &source {
                Source::Children { elements } => !elements || element.is_some(),
                Source::Tag { name, namespace } => element.is_some_and(|element| {
                    let tag_matches = name.as_ref() == "*"
                        || if namespace.is_none() && element.name.ns == markup5ever::ns!(html) {
                            element.name.local.as_ref() == name.to_ascii_lowercase()
                        } else {
                            element.name.local.as_ref() == name.as_ref()
                        };
                    tag_matches
                        && namespace.as_ref().is_none_or(|namespace| {
                            namespace.as_ref() == "*"
                                || element.name.ns.as_ref() == namespace.as_ref()
                        })
                }),
                Source::Class(classes) => element.is_some_and(|element| {
                    let tokens = split_tokens(
                        element
                            .attr(blitz_dom::local_name!("class"))
                            .unwrap_or_default(),
                    );
                    !classes.is_empty()
                        && classes
                            .iter()
                            .all(|class| tokens.iter().any(|token| token == class.as_ref()))
                }),
                Source::Tokens | Source::Snapshot => false,
            };
            if selected {
                result.push(id);
            }
            if !matches!(source, Source::Children { .. }) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
        result
    };
    Ok(ids
        .into_iter()
        .map(|id| node_wrapper(&ctx, id, context).into())
        .collect())
}

fn named_value(this: &JsValue, name: &str, context: &mut Context) -> JsResult<JsValue> {
    if name.is_empty() {
        return Ok(JsValue::null());
    }
    let items = values(this, context)?;
    let ctx = dom_ctx(context)?;
    let doc = ctx.doc.borrow();
    for item in &items {
        let id = super::node_id_of_value(item).expect("HTMLCollection contains a non-node");
        let Some(element) = doc.get_node(id).and_then(|node| node.element_data()) else {
            continue;
        };
        if element.attr(blitz_dom::local_name!("id")) == Some(name)
            || (element.name.ns == markup5ever::ns!(html)
                && element.attr(blitz_dom::local_name!("name")) == Some(name))
        {
            return Ok(item.clone());
        }
    }
    Ok(JsValue::null())
}

fn index(key: &PropertyKey) -> Option<u32> {
    match key {
        PropertyKey::Index(index) => Some(index.get()),
        PropertyKey::String(string) => {
            let string = string.to_std_string_lossy();
            let number = string.parse::<u32>().ok()?;
            (number.to_string() == string && number != u32::MAX).then_some(number)
        }
        PropertyKey::Symbol(_) => None,
    }
}

fn get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args[0].as_object().expect("proxy target");
    let key = args[1].to_property_key(context)?;
    let private = context
        .get_data::<CollectionKey>()
        .expect("missing collection key")
        .symbol
        .clone();
    if key == PropertyKey::from(private) {
        return Ok(target.into());
    }
    if let Some(index) = index(&key) {
        return Ok(values(&args[0], context)?
            .get(index as usize)
            .cloned()
            .unwrap_or_default());
    }
    if target.has_property(key.clone(), context)? {
        return target.get(key, context);
    }
    let named = target
        .downcast_ref::<Collection>()
        .expect("collection proxy target")
        .named;
    if named && let PropertyKey::String(name) = key {
        let value = named_value(&args[0], &name.to_std_string_lossy(), context)?;
        if !value.is_null() {
            return Ok(value);
        }
    }
    Ok(JsValue::undefined())
}

fn supported(this: &JsValue, key: &PropertyKey, context: &mut Context) -> JsResult<bool> {
    if let Some(index) = index(key) {
        return Ok((index as usize) < values(this, context)?.len());
    }
    let object = record(this, context)?;
    let named = object
        .downcast_ref::<Collection>()
        .expect("validated collection")
        .named;
    if named && let PropertyKey::String(name) = key {
        if object.has_property(key.clone(), context)? {
            return Ok(false);
        }
        return Ok(!named_value(this, &name.to_std_string_lossy(), context)?.is_null());
    }
    Ok(false)
}

fn has(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args[0].as_object().expect("proxy target");
    let key = args[1].to_property_key(context)?;
    Ok(JsValue::from(
        supported(&args[0], &key, context)? || target.has_property(key, context)?,
    ))
}

fn set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args[0].as_object().expect("proxy target");
    let key = args[1].to_property_key(context)?;
    if index(&key).is_some() || supported(&args[0], &key, context)? {
        return Ok(JsValue::from(false));
    }
    Ok(JsValue::from(target.set(
        key,
        args[2].clone(),
        false,
        context,
    )?))
}

fn own_keys(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let items = values(&args[0], context)?;
    let mut names: Vec<String> = (0..items.len()).map(|index| index.to_string()).collect();
    let target = args[0].as_object().expect("proxy target");
    let named = target
        .downcast_ref::<Collection>()
        .expect("collection proxy target")
        .named;
    if named {
        let ctx = dom_ctx(context)?;
        let doc = ctx.doc.borrow();
        for value in items {
            let id = super::node_id_of_value(&value).expect("HTMLCollection contains a non-node");
            if let Some(element) = doc.get_node(id).and_then(|node| node.element_data()) {
                for name in [
                    element.attr(blitz_dom::local_name!("id")),
                    (element.name.ns == markup5ever::ns!(html))
                        .then(|| element.attr(blitz_dom::local_name!("name")))
                        .flatten(),
                ]
                .into_iter()
                .flatten()
                {
                    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
                        names.push(name.to_owned());
                    }
                }
            }
        }
    }
    let mut keys: Vec<JsValue> = names.iter().map(|name| js_str(name)).collect();
    for key in target.own_property_keys(context)? {
        let value = match key {
            PropertyKey::Index(index) => js_str(&index.get().to_string()),
            PropertyKey::String(name) => name.into(),
            PropertyKey::Symbol(symbol) => symbol.into(),
        };
        if !keys.iter().any(|existing| existing == &value) {
            keys.push(value);
        }
    }
    Ok(JsArray::from_iter(keys, context).into())
}

fn descriptor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = args[1].to_property_key(context)?;
    if !supported(&args[0], &key, context)? {
        let reflect = context
            .global_object()
            .get(boa_engine::js_string!("Reflect"), context)?
            .as_object()
            .expect("missing Reflect");
        let method = reflect
            .get(boa_engine::js_string!("getOwnPropertyDescriptor"), context)?
            .as_object()
            .expect("missing Reflect.getOwnPropertyDescriptor");
        return method.call(&reflect.into(), args, context);
    }
    let value = get(&JsValue::undefined(), args, context)?;
    let object = boa_engine::object::ObjectInitializer::new(context)
        .property(boa_engine::js_string!("value"), value, Attribute::all())
        .property(boa_engine::js_string!("writable"), false, Attribute::all())
        .property(
            boa_engine::js_string!("enumerable"),
            index(&key).is_some(),
            Attribute::all(),
        )
        .property(
            boa_engine::js_string!("configurable"),
            true,
            Attribute::all(),
        )
        .build();
    Ok(object.into())
}

fn make(
    name: &str,
    source: Source,
    owner: Option<JsObject>,
    items: Vec<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let target = JsObject::from_proto_and_data(
        Some(super::interfaces::prototype(name, context)),
        Collection {
            owner,
            items,
            source,
            named: name == "HTMLCollection",
        },
    );
    Ok(JsProxy::builder(target)
        .get(get)
        .has(has)
        .set(set)
        .own_keys(own_keys)
        .get_own_property_descriptor(descriptor)
        .build(context)?
        .into())
}

pub(super) fn snapshot(
    name: &str,
    items: Vec<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    make(name, Source::Snapshot, None, items, context)
}

fn same_object(
    this: &JsValue,
    key: &str,
    name: &str,
    source: Source,
    context: &mut Context,
) -> JsResult<JsValue> {
    let owner = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid node receiver"))?;
    this_node_id(this)?;
    if owner.has_own_property(JsString::from(key), context)? {
        return owner.get(JsString::from(key), context);
    }
    let collection = make(name, source, Some(owner.clone()), Vec::new(), context)?;
    define_value(&owner, key, collection.clone(), context);
    Ok(collection)
}

fn children(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    same_object(
        this,
        "__blitz_children",
        "HTMLCollection",
        Source::Children { elements: true },
        context,
    )
}

fn child_nodes(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    same_object(
        this,
        "__blitz_child_nodes",
        "NodeList",
        Source::Children { elements: false },
        context,
    )
}

fn class_list(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    same_object(
        this,
        "__blitz_tokens",
        "DOMTokenList",
        Source::Tokens,
        context,
    )
}

fn length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(values(this, context)?.len() as f64))
}

fn item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let index = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)?;
    Ok(values(this, context)?
        .get(index as usize)
        .cloned()
        .unwrap_or_else(JsValue::null))
}

fn named_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = super::to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    named_value(this, &name, context)
}

fn token_owner(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let object = record(this, context)?;
    let data = object
        .downcast_ref::<Collection>()
        .expect("validated collection");
    if !matches!(data.source, Source::Tokens) {
        return Err(JsNativeError::typ()
            .with_message("Invalid DOMTokenList receiver")
            .into());
    }
    Ok(data.owner.clone().expect("DOMTokenList without owner"))
}

fn token_value(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let owner = token_owner(this, context)?;
    let ctx = dom_ctx(context)?;
    let id = this_node_id(&owner.into())?;
    let doc = ctx.doc.borrow();
    Ok(js_str(
        doc.get_node(id)
            .and_then(|node| node.attr(blitz_dom::local_name!("class")))
            .unwrap_or_default(),
    ))
}

fn write_tokens(this: &JsValue, value: &str, context: &mut Context) -> JsResult<()> {
    let owner = token_owner(this, context)?;
    let id = this_node_id(&owner.into())?;
    dom_ctx(context)?.mutate_doc().mutate().set_attribute(
        id,
        super::element::attr_name("class"),
        value,
    );
    Ok(())
}

fn set_token_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = super::to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    write_tokens(this, &value, context)?;
    Ok(JsValue::undefined())
}

fn token(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let token = super::to_rust_string(value, context)?;
    if token.is_empty() {
        return Err(super::interfaces::exception(
            "SyntaxError",
            "Token must not be empty",
            context,
        ));
    }
    if token
        .chars()
        .any(|character| matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' '))
    {
        return Err(super::interfaces::exception(
            "InvalidCharacterError",
            "Token must not contain HTML whitespace",
            context,
        ));
    }
    Ok(token)
}

fn token_operation(
    operation: u8,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    token_owner(this, context)?;
    let required = if operation == 4 { 2 } else { args.len().min(1) };
    let mut requested = Vec::new();
    let count = if matches!(operation, 1 | 2) {
        args.len()
    } else {
        required
    };
    for index in 0..count {
        requested.push(token(
            args.get(index).unwrap_or(&JsValue::undefined()),
            context,
        )?);
    }
    if requested.is_empty() && !matches!(operation, 1 | 2) {
        requested.push(token(&JsValue::undefined(), context)?);
    }
    let raw = token_value(this, &[], context)?;
    let mut current = split_tokens(&super::to_rust_string(&raw, context)?);
    let found = requested
        .first()
        .is_some_and(|token| current.contains(token));
    let result = match operation {
        0 => return Ok(JsValue::from(found)),
        1 => {
            for token in requested {
                if !current.contains(&token) {
                    current.push(token);
                }
            }
            JsValue::undefined()
        }
        2 => {
            current.retain(|token| !requested.contains(token));
            JsValue::undefined()
        }
        3 => {
            let wanted = args.get(1).map(JsValue::to_boolean).unwrap_or(!found);
            let value = &requested[0];
            if wanted && !found {
                current.push(value.clone());
            }
            if !wanted && found {
                current.retain(|token| token != value);
            }
            if wanted == found {
                return Ok(JsValue::from(wanted));
            }
            JsValue::from(wanted)
        }
        4 => {
            if !found {
                return Ok(JsValue::from(false));
            }
            let old = &requested[0];
            let new = &requested[1];
            if old != new {
                if current.contains(new) {
                    current.retain(|token| token != old);
                } else if let Some(index) = current.iter().position(|token| token == old) {
                    current[index] = new.clone();
                }
            }
            JsValue::from(true)
        }
        _ => unreachable!(),
    };
    write_tokens(this, &current.join(" "), context)?;
    Ok(result)
}

fn descendants(
    operation: u8,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    this_node_id(this)?;
    let first = super::to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let source = match operation {
        0 => Source::Tag {
            name: first.into(),
            namespace: None,
        },
        1 => {
            let local =
                super::to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
            let namespace = if args.first().is_none_or(JsValue::is_null_or_undefined) {
                Arc::from("")
            } else {
                Arc::from(first)
            };
            Source::Tag {
                name: local.into(),
                namespace: Some(namespace),
            }
        }
        2 => Source::Class(split_tokens(&first).into_iter().map(Arc::from).collect()),
        _ => unreachable!(),
    };
    make(
        "HTMLCollection",
        source,
        this.as_object(),
        Vec::new(),
        context,
    )
}

fn wrap_query(proto: &JsObject, context: &mut Context) {
    let original = proto
        .get(boa_engine::js_string!("querySelectorAll"), context)
        .expect("missing querySelectorAll")
        .as_object()
        .expect("invalid querySelectorAll");
    let function = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, original, context| {
                let result = original.call(this, args, context)?;
                let array = result
                    .as_object()
                    .ok_or_else(|| JsNativeError::typ().with_message("Invalid query result"))?;
                let length = array
                    .get(boa_engine::js_string!("length"), context)?
                    .to_u32(context)?;
                let mut items = Vec::with_capacity(length as usize);
                for index in 0..length {
                    items.push(array.get(index, context)?);
                }
                snapshot("NodeList", items, context)
            },
            original,
        ),
    )
    .name(boa_engine::js_string!("querySelectorAll"))
    .length(1)
    .build();
    define_value(proto, "querySelectorAll", function.into(), context);
}

pub(super) fn init(
    node: &JsObject,
    element: &JsObject,
    document: &JsObject,
    context: &mut Context,
) {
    context.insert_data(CollectionKey {
        symbol: JsSymbol::new(Some(boa_engine::js_string!("DOM collection data")))
            .expect("failed to allocate collection symbol"),
    });
    for name in ["NodeList", "HTMLCollection", "DOMTokenList", "DOMRectList"] {
        let proto = JsObject::with_object_proto(context.intrinsics());
        super::interfaces::register(
            name,
            None,
            proto.clone(),
            0,
            NativeFunction::from_fn_ptr(super::interfaces::illegal),
            context,
        );
        define_accessor(&proto, "length", Some(length), None, context);
        define_method(&proto, "item", 1, item, context);
        if name == "HTMLCollection" {
            define_method(&proto, "namedItem", 1, named_item, context);
        }
        if matches!(name, "NodeList" | "DOMTokenList" | "HTMLCollection") {
            let array = context.intrinsics().constructors().array().prototype();
            for method in ["keys", "values", "entries", "forEach"] {
                if name == "HTMLCollection" && method != "values" {
                    continue;
                }
                let function = array
                    .get(JsString::from(method), context)
                    .expect("missing array method");
                define_value(&proto, method, function.clone(), context);
                if method == "values" {
                    proto
                        .define_property_or_throw(
                            JsSymbol::iterator(),
                            PropertyDescriptor::builder()
                                .value(function)
                                .writable(true)
                                .enumerable(false)
                                .configurable(true),
                            context,
                        )
                        .expect("failed to define collection iterator");
                }
            }
        }
    }
    let tokens = super::interfaces::prototype("DOMTokenList", context);
    define_accessor(
        &tokens,
        "value",
        Some(token_value),
        Some(set_token_value),
        context,
    );
    define_method(&tokens, "toString", 0, token_value, context);
    for (name, operation, length) in [
        ("contains", 0, 1),
        ("add", 1, 0),
        ("remove", 2, 0),
        ("toggle", 3, 1),
        ("replace", 4, 2),
    ] {
        let function = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, args, context| {
                token_operation(operation, this, args, context)
            }),
        )
        .name(JsString::from(name))
        .length(length)
        .build();
        define_value(&tokens, name, function.into(), context);
    }
    define_accessor(node, "childNodes", Some(child_nodes), None, context);
    define_accessor(element, "children", Some(children), None, context);
    define_accessor(element, "classList", Some(class_list), None, context);
    define_accessor(document, "children", Some(children), None, context);
    let fragment = super::interfaces::prototype("DocumentFragment", context);
    define_accessor(&fragment, "children", Some(children), None, context);
    for proto in [element, document] {
        wrap_query(proto, context);
        for (name, operation, length) in [
            ("getElementsByTagName", 0, 1),
            ("getElementsByTagNameNS", 1, 2),
            ("getElementsByClassName", 2, 1),
        ] {
            let function = FunctionObjectBuilder::new(
                context.realm(),
                NativeFunction::from_copy_closure(move |this, args, context| {
                    descendants(operation, this, args, context)
                }),
            )
            .name(JsString::from(name))
            .length(length)
            .build();
            define_value(proto, name, function.into(), context);
        }
    }
}

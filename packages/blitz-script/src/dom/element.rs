//! The `Element` prototype: attributes, DOM properties (`value`, `checked`, ...),
//! `style`, `innerHTML` and friends.

use blitz_dom::{LocalName, QualName};
use boa_engine::object::{JsObject, ObjectInitializer, builtins::JsProxyBuilder};
use boa_engine::property::{Attribute as PropAttribute, PropertyKey};
use boa_engine::value::JsValue;
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, Trace, js_string};

use super::{
    define_accessor, define_method, dom_ctx, js_str, node_wrapper, this_node_id, to_rust_string,
};
use crate::state::DomCtx;

/// Construct a `QualName` for an attribute (no namespace)
pub(crate) fn attr_name(local: &str) -> QualName {
    QualName::new(None, markup5ever::ns!(), LocalName::from(local))
}

pub(crate) fn init_element_proto(proto: &JsObject, context: &mut Context) {
    define_accessor(proto, "tagName", Some(tag_name), None, context);
    define_accessor(proto, "localName", Some(local_name), None, context);
    define_accessor(proto, "namespaceURI", Some(namespace_uri), None, context);
    define_accessor(proto, "id", Some(get_id), Some(set_id), context);
    define_accessor(
        proto,
        "className",
        Some(get_class_name),
        Some(set_class_name),
        context,
    );
    define_accessor(proto, "value", Some(get_value), Some(set_value), context);
    define_accessor(
        proto,
        "checked",
        Some(get_checked),
        Some(set_checked),
        context,
    );
    define_accessor(
        proto,
        "disabled",
        Some(get_disabled),
        Some(set_disabled),
        context,
    );
    define_accessor(
        proto,
        "placeholder",
        Some(get_placeholder),
        Some(set_placeholder),
        context,
    );
    define_accessor(proto, "type", Some(get_type), Some(set_type), context);
    define_accessor(
        proto,
        "autofocus",
        Some(get_autofocus),
        Some(set_autofocus),
        context,
    );
    define_accessor(proto, "style", Some(get_style), None, context);
    define_accessor(proto, "dataset", Some(get_dataset), None, context);
    define_accessor(proto, "classList", Some(get_class_list), None, context);
    define_accessor(
        proto,
        "innerHTML",
        Some(get_inner_html),
        Some(set_inner_html),
        context,
    );
    define_accessor(proto, "outerHTML", Some(get_outer_html), None, context);
    define_accessor(proto, "children", Some(children), None, context);
    define_accessor(proto, "content", Some(get_template_content), None, context);
    define_accessor(
        proto,
        "scrollLeft",
        Some(get_scroll_left),
        Some(set_scroll_left),
        context,
    );
    define_accessor(
        proto,
        "scrollTop",
        Some(get_scroll_top),
        Some(set_scroll_top),
        context,
    );
    define_accessor(proto, "scrollWidth", Some(get_scroll_width), None, context);
    define_accessor(
        proto,
        "scrollHeight",
        Some(get_scroll_height),
        None,
        context,
    );
    define_accessor(proto, "clientWidth", Some(get_client_width), None, context);
    define_accessor(
        proto,
        "clientHeight",
        Some(get_client_height),
        None,
        context,
    );

    define_method(proto, "getAttribute", 1, get_attribute, context);
    define_method(proto, "setAttribute", 2, set_attribute, context);
    define_method(proto, "removeAttribute", 1, remove_attribute, context);
    define_method(proto, "hasAttribute", 1, has_attribute, context);
    define_method(proto, "focus", 0, focus, context);
    define_method(proto, "blur", 0, blur, context);
    define_method(
        proto,
        "getBoundingClientRect",
        0,
        get_bounding_client_rect,
        context,
    );
    define_method(proto, "querySelector", 1, query_selector, context);
    define_method(proto, "querySelectorAll", 1, query_selector_all, context);
    define_method(proto, "matches", 1, matches_selector, context);
    define_method(proto, "closest", 1, closest, context);
    define_method(proto, "setPointerCapture", 1, set_pointer_capture, context);
    define_method(
        proto,
        "releasePointerCapture",
        1,
        release_pointer_capture,
        context,
    );
    define_method(proto, "hasPointerCapture", 1, has_pointer_capture, context);
}

fn pointer_id_arg(args: &[JsValue], context: &mut Context) -> JsResult<u64> {
    let value = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)?;
    if !value.is_finite() || value < 0.0 {
        return Err(JsNativeError::typ()
            .with_message("pointerId must be a non-negative finite number")
            .into());
    }
    Ok(value as u64)
}

fn set_pointer_capture(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let pointer_id = pointer_id_arg(args, context)?;
    ctx.state
        .borrow_mut()
        .pointer_capture
        .insert(pointer_id, node_id);
    Ok(JsValue::undefined())
}

fn release_pointer_capture(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let pointer_id = pointer_id_arg(args, context)?;
    let mut state = ctx.state.borrow_mut();
    if state.pointer_capture.get(&pointer_id) == Some(&node_id) {
        state.pointer_capture.remove(&pointer_id);
    }
    Ok(JsValue::undefined())
}

fn has_pointer_capture(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let pointer_id = pointer_id_arg(args, context)?;
    Ok(JsValue::from(
        ctx.state.borrow().pointer_capture.get(&pointer_id) == Some(&node_id),
    ))
}

// === Attribute helpers ===

fn read_attr(ctx: &DomCtx, node_id: usize, name: &str) -> Option<String> {
    let doc = ctx.doc.borrow();
    let node = doc.get_node(node_id)?;
    let element = node.element_data()?;
    element
        .attrs()
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.clone())
}

fn write_attr(ctx: &DomCtx, node_id: usize, name: &str, value: &str) {
    let mut doc = ctx.doc.borrow_mut();
    doc.mutate().set_attribute(node_id, attr_name(name), value);
}

fn clear_attr(ctx: &DomCtx, node_id: usize, name: &str) {
    let mut doc = ctx.doc.borrow_mut();
    doc.mutate().clear_attribute(node_id, attr_name(name));
}

fn attr_getter(name: &str, this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    Ok(js_str(&read_attr(&ctx, node_id, name).unwrap_or_default()))
}

fn attr_setter(
    name: &str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    write_attr(&ctx, node_id, name, &value);
    Ok(JsValue::undefined())
}

// === Basic element info ===

fn tag_name(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let name = doc
        .get_node(node_id)
        .and_then(|node| node.element_data())
        .map(|element| element.name.local.to_uppercase())
        .unwrap_or_default();
    Ok(js_str(&name))
}

fn local_name(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let name = doc
        .get_node(node_id)
        .and_then(|node| node.element_data())
        .map(|element| element.name.local.to_string())
        .unwrap_or_default();
    Ok(js_str(&name))
}

fn namespace_uri(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let ns = doc
        .get_node(node_id)
        .and_then(|node| node.element_data())
        .map(|element| element.name.ns.to_string())
        .unwrap_or_default();
    Ok(js_str(&ns))
}

fn children(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let child_ids: Vec<usize> = {
        let doc = ctx.doc.borrow();
        doc.get_node(node_id)
            .map(|node| {
                node.children
                    .iter()
                    .copied()
                    .filter(|child_id| {
                        doc.get_node(*child_id)
                            .is_some_and(|child| child.is_element())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let wrappers: Vec<JsValue> = child_ids
        .into_iter()
        .map(|child_id| node_wrapper(&ctx, child_id, context).into())
        .collect();
    Ok(boa_engine::object::builtins::JsArray::from_iter(wrappers, context).into())
}

// === Attributes ===

fn get_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    match read_attr(&ctx, node_id, &name) {
        Some(value) => Ok(js_str(&value)),
        None => Ok(JsValue::null()),
    }
}

fn set_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    let value = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    write_attr(&ctx, node_id, &name, &value);
    Ok(JsValue::undefined())
}

fn remove_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    clear_attr(&ctx, node_id, &name);
    Ok(JsValue::undefined())
}

fn has_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?
        .to_ascii_lowercase();
    Ok(JsValue::from(read_attr(&ctx, node_id, &name).is_some()))
}

// === Reflected DOM properties ===

fn get_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = args;
    attr_getter("id", this, context)
}
fn set_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    attr_setter("id", this, args, context)
}

fn get_class_name(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = args;
    attr_getter("class", this, context)
}
fn set_class_name(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    attr_setter("class", this, args, context)
}

fn get_placeholder(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = args;
    attr_getter("placeholder", this, context)
}
fn set_placeholder(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    attr_setter("placeholder", this, args, context)
}

fn get_type(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = args;
    attr_getter("type", this, context)
}
fn set_type(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    attr_setter("type", this, args, context)
}

fn get_autofocus(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    Ok(JsValue::from(
        read_attr(&ctx, node_id, "autofocus").is_some(),
    ))
}
fn set_autofocus(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = args.first().map(JsValue::to_boolean).unwrap_or(false);
    if value {
        // blitz-dom's autofocus handling expects the value "true"
        write_attr(&ctx, node_id, "autofocus", "true");
    } else {
        clear_attr(&ctx, node_id, "autofocus");
    }
    Ok(JsValue::undefined())
}

fn get_value(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = {
        let doc = ctx.doc.borrow();
        doc.get_node(node_id)
            .and_then(|node| node.element_data())
            .map(|element| match element.text_input_data() {
                Some(input_data) => input_data.editor.text().to_string(),
                None => element
                    .attrs()
                    .iter()
                    .find(|attr| &*attr.name.local == "value")
                    .map(|attr| attr.value.clone())
                    .unwrap_or_default(),
            })
            .unwrap_or_default()
    };
    Ok(js_str(&value))
}

fn set_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    attr_setter("value", this, args, context)
}

fn get_checked(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let checked = {
        let doc = ctx.doc.borrow();
        doc.get_node(node_id)
            .and_then(|node| node.element_data())
            .map(|element| {
                element
                    .checkbox_input_checked()
                    .unwrap_or_else(|| element.attr(blitz_dom::local_name!("checked")).is_some())
            })
            .unwrap_or(false)
    };
    Ok(JsValue::from(checked))
}

fn set_checked(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let checked = args.first().map(JsValue::to_boolean).unwrap_or(false);
    // blitz-dom's checked handling parses the value as a boolean
    write_attr(
        &ctx,
        node_id,
        "checked",
        if checked { "true" } else { "false" },
    );
    Ok(JsValue::undefined())
}

fn get_disabled(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    Ok(JsValue::from(
        read_attr(&ctx, node_id, "disabled").is_some(),
    ))
}

fn set_disabled(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let disabled = args.first().map(JsValue::to_boolean).unwrap_or(false);
    if disabled {
        write_attr(&ctx, node_id, "disabled", "");
    } else {
        clear_attr(&ctx, node_id, "disabled");
    }
    Ok(JsValue::undefined())
}

// === dataset ===

#[derive(Trace, Finalize, JsData)]
struct DatasetRef {
    #[unsafe_ignore_trace]
    node_id: usize,
}

fn dataset_ref(args: &[JsValue]) -> JsResult<usize> {
    args.first()
        .and_then(JsValue::as_object)
        .and_then(|target| target.downcast_ref::<DatasetRef>().map(|data| data.node_id))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("dataset proxy target is invalid")
                .into()
        })
}

fn dataset_key(value: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    Ok(match value.to_property_key(context)? {
        PropertyKey::String(key) => Some(key.to_std_string_lossy()),
        PropertyKey::Index(key) => Some(key.get().to_string()),
        PropertyKey::Symbol(_) => None,
    })
}

fn dataset_attr_name(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 5);
    name.push_str("data-");
    for ch in key.chars() {
        if ch.is_ascii_uppercase() {
            name.push('-');
            name.push(ch.to_ascii_lowercase());
        } else {
            name.push(ch);
        }
    }
    name
}

fn dataset_property_name(attr: &str) -> Option<String> {
    let suffix = attr.strip_prefix("data-")?;
    let mut key = String::with_capacity(suffix.len());
    let mut chars = suffix.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek().is_some_and(char::is_ascii_lowercase) {
            key.push(chars.next().expect("peeked character").to_ascii_uppercase());
        } else {
            key.push(ch);
        }
    }
    Some(key)
}

fn dataset_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args.get(1).unwrap_or(&JsValue::undefined()), context)? else {
        return Ok(JsValue::undefined());
    };
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    Ok(read_attr(&ctx, node_id, &dataset_attr_name(&key))
        .map_or_else(JsValue::undefined, |value| js_str(&value)))
}

fn dataset_set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args.get(1).unwrap_or(&JsValue::undefined()), context)? else {
        return Ok(JsValue::from(false));
    };
    let value = to_rust_string(args.get(2).unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    write_attr(&ctx, node_id, &dataset_attr_name(&key), &value);
    Ok(JsValue::from(true))
}

fn dataset_delete(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args.get(1).unwrap_or(&JsValue::undefined()), context)? else {
        return Ok(JsValue::from(true));
    };
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    clear_attr(&ctx, node_id, &dataset_attr_name(&key));
    Ok(JsValue::from(true))
}

fn dataset_has(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args.get(1).unwrap_or(&JsValue::undefined()), context)? else {
        return Ok(JsValue::from(false));
    };
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    Ok(JsValue::from(
        read_attr(&ctx, node_id, &dataset_attr_name(&key)).is_some(),
    ))
}

fn dataset_own_keys(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    let keys: Vec<JsValue> = {
        let doc = ctx.doc.borrow();
        doc.get_node(node_id)
            .and_then(|node| node.element_data())
            .map(|element| {
                element
                    .attrs()
                    .iter()
                    .filter_map(|attr| dataset_property_name(&attr.name.local))
                    .map(|key| js_str(&key))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(boa_engine::object::builtins::JsArray::from_iter(keys, context).into())
}

fn dataset_descriptor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args.get(1).unwrap_or(&JsValue::undefined()), context)? else {
        return Ok(JsValue::undefined());
    };
    let ctx = dom_ctx(context)?;
    let node_id = dataset_ref(args)?;
    let Some(value) = read_attr(&ctx, node_id, &dataset_attr_name(&key)) else {
        return Ok(JsValue::undefined());
    };
    Ok(ObjectInitializer::new(context)
        .property(js_string!("value"), js_str(&value), PropAttribute::all())
        .property(js_string!("writable"), true, PropAttribute::all())
        .property(js_string!("enumerable"), true, PropAttribute::all())
        .property(js_string!("configurable"), true, PropAttribute::all())
        .build()
        .into())
}

fn get_dataset(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    if let Some(dataset) = ctx.state.borrow().dataset_wrappers.get(&node_id) {
        return Ok(dataset.clone().into());
    }

    let target = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        DatasetRef { node_id },
    );
    let dataset: JsObject = JsProxyBuilder::new(target)
        .get(dataset_get)
        .set(dataset_set)
        .delete_property(dataset_delete)
        .has(dataset_has)
        .own_keys(dataset_own_keys)
        .get_own_property_descriptor(dataset_descriptor)
        .build(context)?
        .into();
    ctx.state
        .borrow_mut()
        .dataset_wrappers
        .insert(node_id, dataset.clone());
    Ok(dataset.into())
}

// === classList ===

fn class_tokens(ctx: &DomCtx, node_id: usize) -> Vec<String> {
    read_attr(ctx, node_id, "class")
        .unwrap_or_default()
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect()
}

fn write_class_tokens(ctx: &DomCtx, node_id: usize, tokens: &[String]) {
    write_attr(ctx, node_id, "class", &tokens.join(" "));
}

fn class_token(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let token = to_rust_string(value, context)?;
    if token.is_empty() || token.chars().any(|ch| ch.is_ascii_whitespace()) {
        return Err(JsNativeError::syntax()
            .with_message("classList token must be non-empty and contain no ASCII whitespace")
            .into());
    }
    Ok(token)
}

fn class_list_length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(JsValue::from(
        class_tokens(&ctx, this_node_id(this)?).len() as u32
    ))
}

fn class_list_value(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(js_str(
        &read_attr(&ctx, this_node_id(this)?, "class").unwrap_or_default(),
    ))
}

fn set_class_list_value(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let value = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    write_attr(&ctx, this_node_id(this)?, "class", &value);
    Ok(JsValue::undefined())
}

fn class_list_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let index = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)? as usize;
    let ctx = dom_ctx(context)?;
    Ok(class_tokens(&ctx, this_node_id(this)?)
        .get(index)
        .map_or_else(JsValue::null, |token| js_str(token)))
}

fn class_list_contains(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let token = class_token(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    Ok(JsValue::from(
        class_tokens(&ctx, this_node_id(this)?).contains(&token),
    ))
}

fn class_list_add(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let tokens_to_add = args
        .iter()
        .map(|value| class_token(value, context))
        .collect::<JsResult<Vec<_>>>()?;
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let mut tokens = class_tokens(&ctx, node_id);
    for token in tokens_to_add {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    write_class_tokens(&ctx, node_id, &tokens);
    Ok(JsValue::undefined())
}

fn class_list_remove(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let tokens_to_remove = args
        .iter()
        .map(|value| class_token(value, context))
        .collect::<JsResult<Vec<_>>>()?;
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let mut tokens = class_tokens(&ctx, node_id);
    tokens.retain(|token| !tokens_to_remove.contains(token));
    write_class_tokens(&ctx, node_id, &tokens);
    Ok(JsValue::undefined())
}

fn class_list_toggle(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let token = class_token(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let force = args.get(1).map(JsValue::to_boolean);
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let mut tokens = class_tokens(&ctx, node_id);
    let present = tokens.contains(&token);
    let retain = force.unwrap_or(!present);
    if retain && !present {
        tokens.push(token);
    } else if !retain && present {
        tokens.retain(|item| item != &token);
    }
    write_class_tokens(&ctx, node_id, &tokens);
    Ok(JsValue::from(retain))
}

fn class_list_replace(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let old = class_token(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let new = class_token(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let mut tokens = class_tokens(&ctx, node_id);
    let Some(index) = tokens.iter().position(|token| token == &old) else {
        return Ok(JsValue::from(false));
    };
    if old != new {
        tokens[index] = new;
        let mut seen = std::collections::HashSet::new();
        tokens.retain(|token| seen.insert(token.clone()));
    }
    write_class_tokens(&ctx, node_id, &tokens);
    Ok(JsValue::from(true))
}

fn class_list_to_string(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    class_list_value(this, args, context)
}

fn get_class_list(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    if let Some(class_list) = ctx.state.borrow().class_list_wrappers.get(&node_id) {
        return Ok(class_list.clone().into());
    }

    let class_list = JsObject::from_proto_and_data(
        Some(context.intrinsics().constructors().object().prototype()),
        super::NodeRef { node_id },
    );
    define_accessor(
        &class_list,
        "length",
        Some(class_list_length),
        None,
        context,
    );
    define_accessor(
        &class_list,
        "value",
        Some(class_list_value),
        Some(set_class_list_value),
        context,
    );
    define_method(&class_list, "item", 1, class_list_item, context);
    define_method(&class_list, "contains", 1, class_list_contains, context);
    define_method(&class_list, "add", 1, class_list_add, context);
    define_method(&class_list, "remove", 1, class_list_remove, context);
    define_method(&class_list, "toggle", 1, class_list_toggle, context);
    define_method(&class_list, "replace", 2, class_list_replace, context);
    define_method(&class_list, "toString", 0, class_list_to_string, context);
    ctx.state
        .borrow_mut()
        .class_list_wrappers
        .insert(node_id, class_list.clone());
    Ok(class_list.into())
}

// === Style ===

fn get_style(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let proto = ctx.state.borrow().protos().style.clone();
    Ok(JsObject::from_proto_and_data(Some(proto), super::NodeRef { node_id }).into())
}

// === innerHTML / outerHTML ===

fn get_template_content(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let is_template = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .and_then(|node| node.element_data())
        .is_some_and(|element| element.name.local == markup5ever::local_name!("template"));

    // Blitz currently stores parsed template children on the template node
    // itself. Expose that node as the content container until blitz-dom grows a
    // distinct DocumentFragment node type.
    Ok(if is_template {
        this.clone()
    } else {
        JsValue::undefined()
    })
}

fn get_inner_html(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let mut html = String::new();
    if let Some(node) = doc.get_node(node_id) {
        for child_id in &node.children {
            if let Some(child) = doc.get_node(*child_id) {
                child.write_outer_html(&mut html);
            }
        }
    }
    Ok(js_str(&html))
}

fn set_inner_html(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let html = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    let mut doc = ctx.doc.borrow_mut();
    let mut mutr = doc.mutate();
    // Detach (rather than drop) any existing children so that JS wrappers
    // referencing them remain valid.
    for child_id in mutr.child_ids(node_id) {
        mutr.remove_node(child_id);
    }
    mutr.set_inner_html(node_id, &html);
    Ok(JsValue::undefined())
}

fn get_outer_html(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let html = doc
        .get_node(node_id)
        .map(|node| node.outer_html())
        .unwrap_or_default();
    Ok(js_str(&html))
}

// === Focus ===

fn focus(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    ctx.doc.borrow_mut().set_focus_to(node_id);
    Ok(JsValue::undefined())
}

fn blur(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _ = this_node_id(this)?;
    ctx.doc.borrow_mut().clear_focus();
    Ok(JsValue::undefined())
}

// === Geometry ===

fn get_scroll_left(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| node.scroll_offset.x)
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn set_scroll_left(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_scroll_axis(this, args, context, true)
}

fn get_scroll_top(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| node.scroll_offset.y)
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn set_scroll_top(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_scroll_axis(this, args, context, false)
}

fn set_scroll_axis(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    horizontal: bool,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let requested = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)?;
    let target = if requested.is_finite() {
        requested.max(0.0)
    } else {
        0.0
    };
    let mut doc = ctx.doc.borrow_mut();
    let current = doc
        .get_node(node_id)
        .map(|node| {
            if horizontal {
                node.scroll_offset.x
            } else {
                node.scroll_offset.y
            }
        })
        .unwrap_or(0.0);
    let (delta_x, delta_y) = if horizontal {
        (current - target, 0.0)
    } else {
        (0.0, current - target)
    };
    if doc.scroll_by(Some(node_id), delta_x, delta_y, &mut |_| {}) {
        doc.shell_provider.request_redraw();
    }
    Ok(JsValue::undefined())
}

fn get_scroll_width(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| f64::from(node.final_layout.size.width + node.final_layout.scroll_width()))
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn get_scroll_height(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| f64::from(node.final_layout.size.height + node.final_layout.scroll_height()))
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn get_client_width(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| f64::from(node.final_layout.size.width))
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn get_client_height(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let value = ctx
        .doc
        .borrow()
        .get_node(node_id)
        .map(|node| f64::from(node.final_layout.size.height))
        .unwrap_or(0.0);
    Ok(JsValue::from(value))
}

fn get_bounding_client_rect(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let rect = ctx.doc.borrow().get_client_bounding_rect(node_id);
    let (x, y, width, height) = match rect {
        Some(rect) => (rect.x, rect.y, rect.width, rect.height),
        None => (0.0, 0.0, 0.0, 0.0),
    };
    let object = ObjectInitializer::new(context)
        .property(js_string!("x"), x, PropAttribute::all())
        .property(js_string!("y"), y, PropAttribute::all())
        .property(js_string!("width"), width, PropAttribute::all())
        .property(js_string!("height"), height, PropAttribute::all())
        .property(js_string!("left"), x, PropAttribute::all())
        .property(js_string!("top"), y, PropAttribute::all())
        .property(js_string!("right"), x + width, PropAttribute::all())
        .property(js_string!("bottom"), y + height, PropAttribute::all())
        .build();
    Ok(object.into())
}

// === Scoped selector queries ===

fn is_descendant_of(doc: &blitz_dom::BaseDocument, node_id: usize, ancestor_id: usize) -> bool {
    let mut current = doc.get_node(node_id).and_then(|node| node.parent);
    while let Some(id) = current {
        if id == ancestor_id {
            return true;
        }
        current = doc.get_node(id).and_then(|node| node.parent);
    }
    false
}

fn query_selector(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    let result = {
        let doc = ctx.doc.borrow();
        doc.query_selector_all(&selector).ok().and_then(|matches| {
            matches
                .into_iter()
                .find(|match_id| is_descendant_of(&doc, *match_id, node_id))
        })
    };
    Ok(super::node_or_null(&ctx, result, context))
}

fn query_selector_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    let matches: Vec<usize> = {
        let doc = ctx.doc.borrow();
        doc.query_selector_all(&selector)
            .map(|matches| {
                matches
                    .into_iter()
                    .filter(|match_id| is_descendant_of(&doc, *match_id, node_id))
                    .collect()
            })
            .unwrap_or_default()
    };
    let wrappers: Vec<JsValue> = matches
        .into_iter()
        .map(|match_id| node_wrapper(&ctx, match_id, context).into())
        .collect();
    Ok(boa_engine::object::builtins::JsArray::from_iter(wrappers, context).into())
}

fn matches_selector(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let is_match = ctx
        .doc
        .borrow()
        .query_selector_all(&selector)
        .is_ok_and(|matches| matches.contains(&node_id));
    Ok(JsValue::from(is_match))
}

fn closest(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let selector = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let result = {
        let doc = ctx.doc.borrow();
        let matches = doc.query_selector_all(&selector).unwrap_or_default();
        let mut current = Some(node_id);
        let mut result = None;
        while let Some(id) = current {
            if matches.contains(&id) {
                result = Some(id);
                break;
            }
            current = doc.get_node(id).and_then(|node| node.parent);
        }
        result
    };
    Ok(super::node_or_null(&ctx, result, context))
}

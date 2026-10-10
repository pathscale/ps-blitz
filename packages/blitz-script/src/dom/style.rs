//! CSSStyleDeclaration bindings backed by Stylo.

use blitz_dom::NodeId;
use boa_engine::object::JsObject;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsResult};

use super::element::attr_name;
use super::{define_accessor, define_method, dom_ctx, js_str, node_wrapper, to_rust_string};
use crate::domc::{StyleRef, exception, style_details};

/// Turn a JS style property name into its CSS spelling.
fn css_property_name(js_name: &str) -> String {
    if js_name == "cssFloat" {
        return "float".to_string();
    }
    if js_name.starts_with("--") || js_name.contains('-') {
        return js_name.to_string();
    }
    let mut css = String::with_capacity(js_name.len() + 2);
    if js_name.starts_with("webkit") {
        css.push('-');
    }
    for ch in js_name.chars() {
        if ch.is_ascii_uppercase() {
            css.push('-');
            css.push(ch.to_ascii_lowercase());
        } else {
            css.push(ch);
        }
    }
    css
}

fn is_api_member(name: &str) -> bool {
    matches!(
        name,
        "cssText"
            | "length"
            | "item"
            | "setProperty"
            | "removeProperty"
            | "getPropertyValue"
            | "getPropertyPriority"
            | "parentRule"
            | "constructor"
    )
}

fn ensure_mutable(this: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    let (node_id, pseudo) = style_details(this)?;
    if pseudo.is_some() {
        return Err(exception(
            "NoModificationAllowedError",
            "Computed style is read-only",
            context,
        ));
    }
    Ok(node_id)
}

fn style_set_trap(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args.first().cloned().unwrap_or_else(JsValue::undefined);
    ensure_mutable(&target, context)?;
    let key_value = &args.get(1).cloned().unwrap_or_default();
    let value = args.get(2).cloned().unwrap_or_else(JsValue::undefined);
    let object = target.as_object().expect("style proxy target is an object");
    if key_value.is_symbol() {
        return object
            .set(key_value.to_property_key(context)?, value, false, context)
            .map(JsValue::from);
    }
    let key = to_rust_string(key_value, context)?;
    if is_api_member(&key) {
        return object
            .set(js_str(&key).to_property_key(context)?, value, false, context)
            .map(JsValue::from);
    }
    let name = css_property_name(&key);
    let value = if value.is_null() {
        js_str("")
    } else {
        value
    };
    set_property(&target, &[js_str(&name), value], context)?;
    Ok(JsValue::from(true))
}

fn style_get_trap(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let Some(object) = target.as_object() else {
        return Ok(JsValue::undefined());
    };
    let key_value = &args.get(1).cloned().unwrap_or_default();
    let property = key_value.to_property_key(context)?;
    let key = if key_value.is_symbol() {
        None
    } else {
        Some(to_rust_string(key_value, context)?)
    };
    if key.is_none()
        || key.as_deref().is_some_and(is_api_member)
        || object.has_property(property.clone(), context)?
    {
        let value = object.get(property, context)?;
        if key.as_deref() != Some("constructor") {
            if let Some(function) = value.as_object()
                && function.is_callable()
            {
                let bind = function.get(boa_engine::js_string!("bind"), context)?;
                if let Some(bind) = bind.as_object() {
                    return bind.call(&value, std::slice::from_ref(&target), context);
                }
            }
        }
        return Ok(value);
    }
    let key = key.unwrap();
    if let Ok(index) = key.parse::<u32>() {
        if index.to_string() == key {
            let names = declaration_names(&target, context)?;
            return Ok(names
                .get(index as usize)
                .map_or_else(JsValue::undefined, |name| js_str(name)));
        }
    }
    get_property_value(&target, &[js_str(&css_property_name(&key))], context)
}

/// Keep the element wrapper alive for as long as its declaration is reachable.
pub(crate) fn make_style_object(
    proto: JsObject,
    node_id: NodeId,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = node_wrapper(&ctx, node_id, context);
    make_declaration(proto, owner, node_id, None, context)
}

pub(crate) fn make_declaration(
    proto: JsObject,
    owner: JsObject,
    node_id: NodeId,
    pseudo: Option<String>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let target = JsObject::from_proto_and_data(
        Some(proto),
        StyleRef {
            owner,
            node_id,
            pseudo,
        },
    );
    let proxy = boa_engine::object::builtins::JsProxy::builder(target)
        .set(style_set_trap)
        .get(style_get_trap)
        .build(context)?;
    Ok(JsValue::from(proxy))
}

pub(crate) fn init_style_proto(proto: &JsObject, context: &mut Context) {
    define_accessor(
        proto,
        "cssText",
        Some(get_css_text),
        Some(set_css_text),
        context,
    );
    define_accessor(proto, "length", Some(length), None, context);
    define_accessor(proto, "parentRule", Some(parent_rule), None, context);
    define_method(proto, "item", 1, item, context);
    define_method(proto, "setProperty", 2, set_property, context);
    define_method(proto, "removeProperty", 1, remove_property, context);
    define_method(proto, "getPropertyValue", 1, get_property_value, context);
    define_method(proto, "getPropertyPriority", 1, get_property_priority, context);
}

fn declaration_names(this: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    let (node_id, pseudo) = style_details(this)?;
    let ctx = dom_ctx(context)?;
    if let Some(pseudo) = pseudo {
        ctx.flush_layout();
        return Ok(ctx.doc.borrow().platform_computed_names(node_id, &pseudo));
    }
    Ok(ctx.doc.borrow().platform_inline_names(node_id))
}

fn length(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(declaration_names(this, context)?.len() as u32))
}

fn item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let index = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)?;
    let names = declaration_names(this, context)?;
    Ok(js_str(names.get(index as usize).map_or("", String::as_str)))
}

fn parent_rule(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    style_details(this)?;
    Ok(JsValue::null())
}

fn get_css_text(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (node_id, pseudo) = style_details(this)?;
    if pseudo.is_some() {
        return Ok(js_str(""));
    }
    let ctx = dom_ctx(context)?;
    Ok(js_str(&ctx.doc.borrow().platform_inline_text(node_id)))
}

fn set_css_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node_id = ensure_mutable(this, context)?;
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:style=");
    let css = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    ctx.mutate_doc()
        .mutate()
        .set_attribute(node_id, attr_name("style"), &css);
    Ok(JsValue::undefined())
}

fn set_property(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node_id = ensure_mutable(this, context)?;
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:style=");
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let value = match args.get(1) {
        None => "undefined".to_string(),
        Some(value) if value.is_null() => String::new(),
        Some(value) => to_rust_string(value, context)?,
    };
    let priority = match args.get(2) {
        None => String::new(),
        Some(value) if value.is_null() || value.is_undefined() => String::new(),
        Some(value) => to_rust_string(value, context)?,
    };
    let css = ctx
        .doc
        .borrow()
        .platform_edit_inline(node_id, &name, &value, &priority);
    if let Some(css) = css {
        ctx.mutate_doc()
            .mutate()
            .set_attribute(node_id, attr_name("style"), &css);
    }
    Ok(JsValue::undefined())
}

fn remove_property(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    ensure_mutable(this, context)?;
    let name = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let previous = get_property_value(this, std::slice::from_ref(&name), context)?;
    set_property(this, &[name, js_str("")], context)?;
    Ok(previous)
}

fn get_property_value(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (node_id, pseudo) = style_details(this)?;
    let ctx = dom_ctx(context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    if let Some(pseudo) = pseudo {
        ctx.flush_layout();
        return Ok(js_str(
            &ctx.doc
                .borrow()
                .platform_computed_value(node_id, &pseudo, &name),
        ));
    }
    Ok(js_str(
        &ctx.doc.borrow().platform_inline_value(node_id, &name, false),
    ))
}

fn get_property_priority(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (node_id, pseudo) = style_details(this)?;
    if pseudo.is_some() {
        return Ok(js_str(""));
    }
    let ctx = dom_ctx(context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    Ok(js_str(
        &ctx.doc.borrow().platform_inline_value(node_id, &name, true),
    ))
}

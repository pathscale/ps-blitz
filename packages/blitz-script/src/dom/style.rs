//! A minimal `CSSStyleDeclaration` binding (`element.style`).

use blitz_dom::NodeId;
use boa_engine::object::JsObject;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsResult};

use super::element::attr_name;
use super::{define_accessor, define_method, dom_ctx, js_str, this_node_id, to_rust_string};

/// Turn a JS style property name into its CSS spelling.
///
/// `style.maxHeight` is `max-height`, and `style.height` is already `height`.
/// Vendor-prefixed names come through as `webkitTransform`, which CSS spells
/// `-webkit-transform`, hence the leading dash when the first character is
/// upper case.
fn css_property_name(js_name: &str) -> String {
    let mut css = String::with_capacity(js_name.len() + 2);
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

/// Whether a property name is one of the declaration object's own members
/// rather than a CSS property being assigned.
fn is_api_member(name: &str) -> bool {
    matches!(
        name,
        "cssText" | "setProperty" | "removeProperty" | "getPropertyValue" | "constructor"
    )
}

/// `set` trap: `element.style.height = "70px"` writes to the style attribute.
///
/// Without this the assignment landed on a plain JS object and was discarded,
/// silently. `CSSStyleDeclaration` in a browser carries a named accessor for
/// every CSS property; this proto only ever defined `cssText`, `setProperty`,
/// `removeProperty` and `getPropertyValue`, so every `.style.x = y` in every
/// page was a no-op — including the composer's autosize, which sets its own
/// height after measuring and appeared simply not to grow.
///
/// A proxy rather than a list of accessors, because a list is the same failure
/// again for whatever property is not on it, and failing silently is what made
/// this expensive to find.
fn style_set_trap(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let key = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let value = args.get(2).cloned().unwrap_or_else(JsValue::undefined);

    if is_api_member(&key) {
        if let Some(object) = target.as_object() {
            object.set(
                js_str(&key).to_property_key(context)?,
                value,
                false,
                context,
            )?;
        }
        return Ok(JsValue::from(true));
    }

    let ctx = dom_ctx(context)?;
    ctx.mark_layout_dirty();
    let node_id = this_node_id(&target)?;
    let name = css_property_name(&key);
    let value = to_rust_string(&value, context)?;
    update_style_attr(&ctx, node_id, |decls| {
        decls.retain(|(prop, _)| !prop.eq_ignore_ascii_case(&name));
        if !value.is_empty() {
            decls.push((name.to_ascii_lowercase(), value));
        }
    });
    Ok(JsValue::from(true))
}

/// `get` trap: read a CSS property back, and leave the real API alone.
fn style_get_trap(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let key = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;

    let Some(object) = target.as_object() else {
        return Ok(JsValue::undefined());
    };
    let property = js_str(&key).to_property_key(context)?;
    if is_api_member(&key) || object.has_property(property.clone(), context)? {
        let value = object.get(property, context)?;
        // Hand back API methods bound to the target.
        //
        // `proxy.setProperty(...)` would otherwise call the target's function
        // with `this` set to the proxy, which carries no `NodeRef`, and every
        // one of them would fail with "`this` is not a DOM node". Boa exposes
        // no way to read a proxy's target from outside, so the binding has to
        // happen here, where the target is still in hand.
        if let Some(function) = value.as_object()
            && function.is_callable()
        {
            let bind = function.get(boa_engine::js_string!("bind"), context)?;
            if let Some(bind) = bind.as_object() {
                return bind.call(&value, std::slice::from_ref(&target), context);
            }
        }
        return Ok(value);
    }

    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(&target)?;
    let name = css_property_name(&key);
    let doc = ctx.doc.borrow();
    let style_attr = doc
        .get_node(node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("style")))
        .unwrap_or_default();
    let value = parse_declarations(style_attr)
        .into_iter()
        .find(|(prop, _)| prop.eq_ignore_ascii_case(&name))
        .map(|(_, value)| value)
        .unwrap_or_default();
    Ok(js_str(&value))
}

/// Build the `element.style` object: a `NodeRef` declaration behind a proxy.
pub(crate) fn make_style_object(
    proto: JsObject,
    node_id: NodeId,
    context: &mut Context,
) -> JsResult<JsValue> {
    let target = JsObject::from_proto_and_data(Some(proto), super::NodeRef { node_id });
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
    define_method(proto, "setProperty", 2, set_property, context);
    define_method(proto, "removeProperty", 1, remove_property, context);
    define_method(proto, "getPropertyValue", 1, get_property_value, context);
}

fn get_css_text(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    let css = doc
        .get_node(node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("style")))
        .unwrap_or_default();
    Ok(js_str(css))
}

fn set_css_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _t = crate::script_stats::Timed::new("dom:style=");
    let ctx = dom_ctx(context)?;
    // A style write changes geometry, and the composer measures its own
    // scrollHeight immediately after setting height to auto. Without this the
    // measurement returns the pre-write layout and the field never grows.
    ctx.mark_layout_dirty();
    let node_id = this_node_id(this)?;
    let css = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let mut doc = ctx.mutate_doc();
    doc.mutate()
        .set_attribute(node_id, attr_name("style"), &css);
    Ok(JsValue::undefined())
}

/// Parse a style attribute string into (property, value) pairs.
///
/// This is a simplification: it does not handle `;` or `:` characters inside
/// values (e.g. in `url(...)` or quoted strings).
fn parse_declarations(style_attr: &str) -> Vec<(String, String)> {
    style_attr
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .map(|(prop, value)| (prop.trim().to_string(), value.trim().to_string()))
        .filter(|(prop, _)| !prop.is_empty())
        .collect()
}

fn serialize_declarations(decls: &[(String, String)]) -> String {
    decls
        .iter()
        .map(|(prop, value)| format!("{prop}: {value};"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn update_style_attr(
    ctx: &crate::state::DomCtx,
    node_id: NodeId,
    f: impl FnOnce(&mut Vec<(String, String)>),
) {
    // Every `element.style.x = y` lands here, re-parsing and re-serialising the
    // whole style attribute each time. It was entirely uncounted, which is how
    // the composer's two height writes per keystroke stayed invisible.
    let _t = crate::script_stats::Timed::new("dom:style=");
    let mut doc = ctx.mutate_doc();
    let style_attr = doc
        .get_node(node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("style")))
        .unwrap_or_default()
        .to_string();
    let mut decls = parse_declarations(&style_attr);
    f(&mut decls);
    let new_style = serialize_declarations(&decls);
    doc.mutate()
        .set_attribute(node_id, attr_name("style"), &new_style);
}

fn set_property(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    // A style write changes geometry, and the composer measures its own
    // scrollHeight immediately after setting height to auto. Without this the
    // measurement returns the pre-write layout and the field never grows.
    ctx.mark_layout_dirty();
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let value = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    update_style_attr(&ctx, node_id, |decls| {
        decls.retain(|(prop, _)| !prop.eq_ignore_ascii_case(&name));
        if !value.is_empty() {
            decls.push((name.to_ascii_lowercase(), value));
        }
    });
    Ok(JsValue::undefined())
}

fn remove_property(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    // A style write changes geometry, and the composer measures its own
    // scrollHeight immediately after setting height to auto. Without this the
    // measurement returns the pre-write layout and the field never grows.
    ctx.mark_layout_dirty();
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let mut removed = String::new();
    update_style_attr(&ctx, node_id, |decls| {
        if let Some((_, value)) = decls
            .iter()
            .find(|(prop, _)| prop.eq_ignore_ascii_case(&name))
        {
            removed = value.clone();
        }
        decls.retain(|(prop, _)| !prop.eq_ignore_ascii_case(&name));
    });
    Ok(js_str(&removed))
}

fn get_property_value(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let node_id = this_node_id(this)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;

    // Parse the style attribute looking for the requested property.
    // This is a simplification: it does not consult the computed style.
    let doc = ctx.doc.borrow();
    let style_attr = doc
        .get_node(node_id)
        .and_then(|node| node.attr(blitz_dom::local_name!("style")))
        .unwrap_or_default();
    let value = style_attr
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .find(|(prop, _)| prop.trim().eq_ignore_ascii_case(&name))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default();
    Ok(js_str(&value))
}

//! Engine-backed DOMC bindings and their realm-local lifecycle state.

use std::cell::Cell;
use std::sync::Arc;

use blitz_dom::NodeId;
use blitz_dom::node::NodeData;
use blitz_dom::platform::{PlatformMediaQuery, PlatformStyleSheet};
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsValue, NativeFunction, Trace,
    js_string,
};

use crate::dom::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_id_of_value, node_or_null,
    node_wrapper, this_node_id, to_rust_string,
};

#[derive(Trace, Finalize, JsData)]
struct PlatformState {
    #[unsafe_ignore_trace]
    ready_state: Cell<&'static str>,
    #[unsafe_ignore_trace]
    current_script: Cell<Option<NodeId>>,
}

#[derive(Trace, Finalize, JsData)]
pub(crate) struct StyleRef {
    pub owner: JsObject,
    #[unsafe_ignore_trace]
    pub node_id: NodeId,
    /// None is inline style; Some("") is the element's computed declaration.
    #[unsafe_ignore_trace]
    pub pseudo: Option<String>,
}

#[derive(Trace, Finalize, JsData)]
struct QueryRef {
    #[unsafe_ignore_trace]
    query: Arc<PlatformMediaQuery>,
}

#[derive(Trace, Finalize, JsData)]
struct SheetRef {
    pub owner: JsObject,
    #[unsafe_ignore_trace]
    sheet: Arc<PlatformStyleSheet>,
}

pub(crate) fn exception(name: &str, message: &str, context: &mut Context) -> JsError {
    let error: JsError = JsNativeError::error()
        .with_message(message.to_owned())
        .into();
    let value = match error.into_opaque(context) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if let Some(object) = value.as_object() {
        let _ = object.set(js_string!("name"), js_str(name), false, context);
    }
    JsError::from_opaque(value)
}

pub(crate) fn style_details(this: &JsValue) -> JsResult<(NodeId, Option<String>)> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal CSSStyleDeclaration receiver"))?;
    let data = object
        .downcast_ref::<StyleRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Illegal CSSStyleDeclaration receiver"))?;
    Ok((data.node_id, data.pseudo.clone()))
}

fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("Illegal constructor")
        .into())
}

pub(crate) fn install(context: &mut Context) {
    context.insert_data(PlatformState {
        ready_state: Cell::new("loading"),
        current_script: Cell::new(None),
    });
    let ctx = dom_ctx(context).expect("DOMC needs a DOM context");
    let (document_proto, style_proto) = {
        let state = ctx.state.borrow();
        (
            state.protos().document.clone(),
            state.protos().style.clone(),
        )
    };
    define_accessor(
        &document_proto,
        "readyState",
        Some(ready_state),
        None,
        context,
    );
    define_accessor(
        &document_proto,
        "currentScript",
        Some(current_script),
        None,
        context,
    );
    define_accessor(
        &document_proto,
        "visibilityState",
        Some(visibility_state),
        None,
        context,
    );
    define_accessor(&document_proto, "hidden", Some(hidden), None, context);
    define_accessor(
        &document_proto,
        "characterSet",
        Some(character_set),
        None,
        context,
    );
    define_accessor(&document_proto, "domain", Some(domain), None, context);
    define_method(
        &document_proto,
        "elementFromPoint",
        2,
        element_from_point,
        context,
    );
    define_method(
        &document_proto,
        "elementsFromPoint",
        2,
        elements_from_point,
        context,
    );

    // Engine-originated events created through script constructors (the media
    // query change) are trusted; isTrusted is an unforgeable native slot, so
    // the shim marks them through this rather than redefining the property.
    context
        .register_global_builtin_callable(
            js_string!("__blitzMarkTrusted"),
            1,
            NativeFunction::from_fn_ptr(|_, args, _| {
                if let Some(event) = args.first().and_then(JsValue::as_object) {
                    crate::dom::event::set_event_field(&event, "isTrusted", &JsValue::from(true));
                }
                Ok(JsValue::undefined())
            }),
        )
        .expect("failed to register __blitzMarkTrusted");
    context
        .register_global_callable(
            js_string!("CSSStyleDeclaration"),
            0,
            NativeFunction::from_fn_ptr(illegal_constructor),
        )
        .expect("failed to register CSSStyleDeclaration");
    let global = context.global_object().clone();
    let constructor = global
        .get(js_string!("CSSStyleDeclaration"), context)
        .expect("CSSStyleDeclaration constructor missing")
        .as_object()
        .expect("CSSStyleDeclaration is not an object");
    constructor
        .set(js_string!("prototype"), style_proto.clone(), true, context)
        .expect("failed to set CSSStyleDeclaration prototype");
    define_value(&style_proto, "constructor", constructor.into(), context);

    define_method(&global, "__domcSupports", 1, supports, context);
    define_method(&global, "__domcMedia", 1, media_query, context);
    define_method(&global, "__domcMatches", 1, media_matches, context);
    define_method(&global, "__domcCollection", 3, collection, context);
    define_method(&global, "__domcSheetOwners", 1, sheet_owners, context);
    define_method(&global, "__domcSheetHandle", 1, sheet_handle, context);
    define_method(&global, "__domcSheetRead", 2, sheet_read, context);
    define_method(&global, "__domcSheetWrite", 3, sheet_write, context);
    define_method(
        &global,
        "__domcImmediateStopped",
        1,
        immediate_stopped,
        context,
    );
}

pub(crate) fn set_current_script(context: &Context, script: Option<NodeId>) {
    if let Some(state) = context.get_data::<PlatformState>() {
        state.current_script.set(script);
    }
}

pub(crate) fn set_ready_state(context: &Context, next: &'static str) -> bool {
    let Some(state) = context.get_data::<PlatformState>() else {
        return false;
    };
    state.ready_state.replace(next) != next
}

fn is_main_document(this: &JsValue, context: &mut Context) -> JsResult<bool> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    let doc = ctx.doc.borrow();
    if !matches!(
        doc.get_node(id).map(|node| &node.data),
        Some(NodeData::Document(_))
    ) {
        return Err(JsNativeError::typ()
            .with_message("Illegal Document receiver")
            .into());
    }
    Ok(id == doc.root_node().id)
}

fn ready_state(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !is_main_document(this, context)? {
        return Ok(js_str("complete"));
    }
    Ok(js_str(
        context
            .get_data::<PlatformState>()
            .expect("DOMC state missing")
            .ready_state
            .get(),
    ))
}

fn current_script(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !is_main_document(this, context)? {
        return Ok(JsValue::null());
    }
    let id = context
        .get_data::<PlatformState>()
        .expect("DOMC state missing")
        .current_script
        .get();
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(&ctx, id, context))
}

fn visibility_state(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(js_str(if is_main_document(this, context)? {
        "visible"
    } else {
        "hidden"
    }))
}

fn hidden(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(!is_main_document(this, context)?))
}

fn character_set(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    is_main_document(this, context)?;
    Ok(js_str("UTF-8"))
}

fn domain(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !is_main_document(this, context)? {
        return Ok(js_str(""));
    }
    let ctx = dom_ctx(context)?;
    Ok(js_str(ctx.doc.borrow().url().host_str().unwrap_or("")))
}

pub(crate) fn get_computed_style(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let element = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let node_id = node_id_of_value(&element)
        .ok_or_else(|| JsNativeError::typ().with_message("getComputedStyle expects an element"))?;
    let ctx = dom_ctx(context)?;
    if !ctx
        .doc
        .borrow()
        .get_node(node_id)
        .is_some_and(|node| node.is_element())
    {
        return Err(JsNativeError::typ()
            .with_message("getComputedStyle expects an element")
            .into());
    }
    let pseudo = match args.get(1) {
        None => String::new(),
        Some(value) if value.is_null() || value.is_undefined() => String::new(),
        Some(value) => to_rust_string(value, context)?.trim().to_ascii_lowercase(),
    };
    let pseudo = match pseudo.as_str() {
        ":before" => "::before".to_string(),
        ":after" => "::after".to_string(),
        _ => pseudo,
    };
    if !pseudo.is_empty()
        && (!pseudo.starts_with(':')
            || pseudo.starts_with("::part(")
            || pseudo.starts_with("::slotted("))
    {
        return Err(JsNativeError::typ()
            .with_message("Invalid computed-style pseudo-element")
            .into());
    }
    ctx.flush_layout();
    let proto = ctx.state.borrow().protos().style.clone();
    crate::dom::style::make_declaration(
        proto,
        element.as_object().expect("element wrapper missing"),
        node_id,
        Some(pseudo),
        context,
    )
}

fn supports(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if args.is_empty() {
        return Err(JsNativeError::typ()
            .with_message("CSS.supports requires an argument")
            .into());
    }
    let first = to_rust_string(&args[0], context)?;
    let second = args
        .get(1)
        .map(|value| to_rust_string(value, context))
        .transpose()?;
    let ctx = dom_ctx(context)?;
    let doc = ctx.doc.borrow();
    Ok(JsValue::from(match second {
        Some(value) => doc.platform_supports_property(&first, &value),
        None => doc.platform_supports(&first),
    }))
}

fn media_query(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let query = Arc::new(ctx.doc.borrow().platform_media_query(&text));
    let media = query.media();
    let matches = ctx.doc.borrow().platform_media_matches(&query);
    let handle = JsObject::from_proto_and_data(None, QueryRef { query });
    Ok(JsArray::from_iter(
        [handle.into(), js_str(&media), JsValue::from(matches)],
        context,
    )
    .into())
}

fn media_matches(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid media query"))?;
    let query = object
        .downcast_ref::<QueryRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid media query"))?
        .query
        .clone();
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    Ok(JsValue::from(
        ctx.doc.borrow().platform_media_matches(&query),
    ))
}

fn wrap_ids(ids: Vec<NodeId>, context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let values: Vec<JsValue> = ids
        .into_iter()
        .map(|id| node_wrapper(&ctx, id, context).into())
        .collect();
    Ok(JsArray::from_iter(values, context).into())
}

fn collection(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = this_node_id(args.first().unwrap_or(&JsValue::undefined()))?;
    let kind = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let name = to_rust_string(args.get(2).unwrap_or(&js_str("")), context)?;
    let ctx = dom_ctx(context)?;
    let ids = {
        let doc = ctx.doc.borrow();
        let mut ids = Vec::new();
        let mut stack = doc
            .get_node(root)
            .map(|node| node.children.iter().rev().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            let Some(node) = doc.get_node(id) else {
                continue;
            };
            if let Some(element) = node.element_data() {
                if element.name.ns.as_ref() == "http://www.w3.org/1999/xhtml" {
                    let tag = element.name.local.as_ref();
                    let matches = match kind.as_str() {
                        "forms" => tag == "form",
                        "images" => tag == "img",
                        "links" => {
                            matches!(tag, "a" | "area")
                                && element.attr(blitz_dom::local_name!("href")).is_some()
                        }
                        "scripts" => tag == "script",
                        "embeds" => tag == "embed",
                        "name" => {
                            element.attr(blitz_dom::local_name!("name")) == Some(name.as_str())
                        }
                        _ => false,
                    };
                    if matches {
                        ids.push(id);
                    }
                }
            }
            stack.extend(node.children.iter().rev().copied());
        }
        ids
    };
    wrap_ids(ids, context)
}

fn point_ids(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Vec<NodeId>> {
    if !is_main_document(this, context)? {
        return Ok(Vec::new());
    }
    let x = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)? as f32;
    let y = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_number(context)? as f32;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    Ok(ctx.doc.borrow().platform_elements_from_point(x, y))
}

fn element_from_point(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = point_ids(this, args, context)?.first().copied();
    let ctx = dom_ctx(context)?;
    Ok(node_or_null(&ctx, id, context))
}

fn elements_from_point(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ids = point_ids(this, args, context)?;
    wrap_ids(ids, context)
}

fn sheet_owners(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = this_node_id(args.first().unwrap_or(&JsValue::undefined()))?;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let ids = ctx.doc.borrow().platform_sheet_owners(root);
    wrap_ids(ids, context)
}

fn sheet_handle(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let id = this_node_id(&value)?;
    let ctx = dom_ctx(context)?;
    let Some(sheet) = ctx.doc.borrow().platform_sheet(id) else {
        return Ok(JsValue::null());
    };
    Ok(JsObject::from_proto_and_data(
        None,
        SheetRef {
            owner: value.as_object().expect("sheet owner missing"),
            sheet: Arc::new(sheet),
        },
    )
    .into())
}

fn sheet_of(args: &[JsValue]) -> JsResult<Arc<PlatformStyleSheet>> {
    let object = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid sheet"))?;
    let data = object
        .downcast_ref::<SheetRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid sheet"))?;
    Ok(Arc::clone(&data.sheet))
}

fn string_array(values: Vec<String>, context: &mut Context) -> JsValue {
    JsArray::from_iter(values.iter().map(|value| js_str(value)), context).into()
}

fn sheet_read(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(args)?;
    let field = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    match field.as_str() {
        "href" => Ok(sheet.href().map_or_else(JsValue::null, js_str)),
        "disabled" => Ok(JsValue::from(sheet.disabled())),
        "media" => Ok(js_str(&sheet.media())),
        "mediaItems" => Ok(string_array(sheet.media_items(), context)),
        "rules" => {
            let rules = sheet.rules().ok_or_else(|| {
                exception(
                    "SecurityError",
                    "The stylesheet is not origin-clean",
                    context,
                )
            })?;
            Ok(string_array(rules, context))
        }
        _ => Err(JsNativeError::typ()
            .with_message("Unknown stylesheet field")
            .into()),
    }
}

fn sheet_write(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(args)?;
    let field = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let value = args.get(2).cloned().unwrap_or_else(JsValue::undefined);
    let ctx = dom_ctx(context)?;
    match field.as_str() {
        "disabled" => ctx
            .mutate_doc()
            .platform_disable_sheet(&sheet, value.to_boolean()),
        "media" => {
            let text = to_rust_string(&value, context)?;
            ctx.mutate_doc().platform_set_sheet_media(&sheet, &text);
        }
        "appendMedium" | "deleteMedium" => {
            let text = to_rust_string(&value, context)?;
            let changed =
                ctx.mutate_doc()
                    .platform_edit_sheet_medium(&sheet, &text, field == "deleteMedium");
            if !changed && field == "deleteMedium" {
                return Err(exception("NotFoundError", "Medium not found", context));
            }
        }
        _ => {
            return Err(JsNativeError::typ()
                .with_message("Unknown stylesheet field")
                .into());
        }
    }
    Ok(JsValue::undefined())
}

fn immediate_stopped(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let stopped = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| {
            object
                .downcast_ref::<crate::dom::event::EventRef>()
                .map(|event| event.stopped_immediate.get())
        })
        .unwrap_or(false);
    Ok(JsValue::from(stopped))
}

//! Native FontFace handles and FontFaceSet snapshots.

use std::sync::Arc;

use blitz_dom::net::web_fonts::WebFont;
use blitz_traits::net::Bytes;
use boa_engine::object::JsObject;
use boa_engine::object::builtins::{JsArray, JsArrayBuffer};
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsValue, Trace};

use crate::dom::{define_method, dom_ctx, js_str, to_rust_string};

#[derive(Trace, Finalize, JsData)]
struct FaceRef {
    #[unsafe_ignore_trace]
    face: Arc<WebFont>,
}

fn handle(face: Arc<WebFont>) -> JsValue {
    JsObject::from_proto_and_data(None, FaceRef { face }).into()
}

fn face(arguments: &[JsValue]) -> JsResult<Arc<WebFont>> {
    let object = arguments
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid FontFace receiver"))?;
    let face = object
        .downcast_ref::<FaceRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid FontFace receiver"))?;
    Ok(Arc::clone(&face.face))
}

pub(crate) fn install(context: &mut Context) {
    let global = context.global_object().clone();
    define_method(&global, "__docwriteFontCreate", 4, create, context);
    define_method(&global, "__docwriteFontRead", 1, read, context);
    define_method(&global, "__docwriteFontLoad", 1, load, context);
    define_method(&global, "__docwriteFontMember", 2, member, context);
    define_method(&global, "__docwriteFonts", 0, fonts, context);
    define_method(&global, "__docwriteFontQuery", 1, query, context);
    define_method(&global, "__docwriteFontFlush", 0, flush, context);
}

fn create(_: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let family = to_rust_string(arguments.first().unwrap_or(&JsValue::undefined()), context)?;
    let source = arguments.get(1).cloned().unwrap_or_else(JsValue::undefined);
    let style = to_rust_string(arguments.get(2).unwrap_or(&js_str("normal")), context)?;
    let weight = to_rust_string(arguments.get(3).unwrap_or(&js_str("normal")), context)?;
    let (source, bytes) = if let Some(object) = source.as_object() {
        let buffer = JsArrayBuffer::from_object(object)?;
        let bytes = buffer
            .data()
            .map(|data| Bytes::copy_from_slice(&data))
            .ok_or_else(|| JsNativeError::typ().with_message("Detached font ArrayBuffer"))?;
        (None, Some(bytes))
    } else {
        (Some(to_rust_string(&source, context)?), None)
    };
    let binary = bytes.is_some();
    let ctx = dom_ctx(context)?;
    let result = ctx
        .doc
        .borrow()
        .make_web_font(&family, source.as_deref(), bytes, &style, &weight);
    let face =
        result.map_err(|message| crate::domc::exception("SyntaxError", &message, context))?;
    if binary {
        ctx.mutate_doc().load_web_font(&face);
    }
    Ok(handle(face))
}

fn read(_: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let face = face(arguments)?;
    Ok(JsArray::from_iter(
        [
            JsValue::from(face.id as f64),
            js_str(face.family()),
            js_str(face.style()),
            js_str(&face.weight().to_string()),
            js_str(face.status()),
        ],
        context,
    )
    .into())
}

fn load(_: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let face = face(arguments)?;
    let ctx = dom_ctx(context)?;
    ctx.mutate_doc().load_web_font(&face);
    Ok(JsValue::undefined())
}

fn member(_: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let face = face(arguments)?;
    let enabled = arguments.get(1).is_some_and(JsValue::to_boolean);
    let ctx = dom_ctx(context)?;
    let changed = ctx.mutate_doc().set_web_font_member(&face, enabled);
    Ok(JsValue::from(changed))
}

fn fonts(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let (faces, pending) = {
        let document = ctx.doc.borrow();
        (document.web_fonts().to_vec(), document.web_fonts_pending())
    };
    let faces = JsArray::from_iter(faces.into_iter().map(handle), context);
    Ok(JsArray::from_iter(
        [
            faces.into(),
            JsValue::from(pending || crate::docwrite::active(context)),
        ],
        context,
    )
    .into())
}

fn query(_: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let shorthand = to_rust_string(arguments.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let result = ctx.doc.borrow().matching_web_fonts(&shorthand);
    let faces =
        result.map_err(|message| crate::domc::exception("SyntaxError", &message, context))?;
    Ok(JsArray::from_iter(faces.into_iter().map(handle), context).into())
}

fn flush(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    ctx.mark_layout_dirty();
    ctx.flush_layout();
    Ok(JsValue::undefined())
}

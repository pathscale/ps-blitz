//! Device-free HTMLMediaElement load requests for the embedding media host.

use blitz_dom::NodeId;
use boa_engine::object::JsObject;
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsValue, Trace};
use boa_gc::GcRefCell;

use crate::dom::{define_method, dom_ctx, this_node_id};

/// The wrapper keeps the node alive until the host consumes the request.
#[derive(Trace, Finalize, JsData)]
pub struct MediaLoadRequest {
    #[unsafe_ignore_trace]
    pub node_id: NodeId,
    pub element: JsObject,
}

#[derive(Trace, Finalize, JsData)]
struct MediaLoads {
    pending: GcRefCell<Vec<MediaLoadRequest>>,
}

pub(crate) fn install(proto: &JsObject, context: &mut Context) {
    context.insert_data(MediaLoads {
        pending: GcRefCell::new(Vec::new()),
    });
    define_method(proto, "load", 0, load, context);
}

fn load(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node_id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    let valid = ctx.doc.borrow().get_node(node_id).is_some_and(|node| {
        node.element_data().is_some_and(|element| {
            element.name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                && matches!(element.name.local.as_ref(), "audio" | "video")
        })
    });
    if !valid {
        return Err(JsNativeError::typ()
            .with_message("Illegal HTMLMediaElement receiver")
            .into());
    }
    let element = this.as_object().expect("validated media wrapper");
    {
        let loads = context
            .get_data::<MediaLoads>()
            .expect("media loads installed");
        let mut pending = loads.pending.borrow_mut();
        pending.retain(|request| request.node_id != node_id);
        pending.push(MediaLoadRequest { node_id, element });
    }
    ctx.doc.borrow().shell_provider.request_redraw();
    Ok(JsValue::undefined())
}

/// Drain from `ScriptDocument::with_js_context` in the embedding host's poll hook.
///
/// The host must abort the element's previous media load and run source
/// selection. This layer queues requests only and never opens an audio device.
pub fn take_load_requests(context: &mut Context) -> Vec<MediaLoadRequest> {
    let Some(loads) = context.get_data::<MediaLoads>() else {
        return Vec::new();
    };
    std::mem::take(&mut *loads.pending.borrow_mut())
}

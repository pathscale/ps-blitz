//! Event dispatch through slots and shadow roots.

use blitz_dom::NodeId;
use boa_engine::object::JsObject;
use boa_engine::{Context, JsNativeError, JsResult, JsString, JsValue, js_string};

use super::{
    define_value,
    event::{EventRef, begin_dispatch, set_event_path},
    node_wrapper, to_rust_string,
};
use crate::state::DomCtx;

pub(crate) struct DispatchResult {
    pub called: bool,
    pub prevented: bool,
    pub stopped: bool,
}

fn stopped(event: &JsObject) -> bool {
    event
        .downcast_ref::<EventRef>()
        .is_some_and(|data| data.stopped.get())
}

fn immediate(event: &JsObject) -> bool {
    event
        .downcast_ref::<EventRef>()
        .is_some_and(|data| data.stopped_immediate.get())
}

struct Entry {
    id: NodeId,
    target: NodeId,
    visible: Vec<NodeId>,
}

pub(crate) fn dispatch(
    ctx: &DomCtx,
    target_id: NodeId,
    event: &JsObject,
    scripted: bool,
    context: &mut Context,
) -> JsResult<DispatchResult> {
    if event.downcast_ref::<EventRef>().is_none() {
        return Err(JsNativeError::typ()
            .with_message("dispatchEvent requires an Event")
            .into());
    }
    let _dispatch = begin_dispatch(event, scripted, context)?;
    let name = to_rust_string(&event.get(js_string!("type"), context)?, context)?;
    let bubbles = event.get(js_string!("bubbles"), context)?.to_boolean();
    let composed = event.get(js_string!("composed"), context)?.to_boolean();
    let cancelable = event.get(js_string!("cancelable"), context)?.to_boolean();
    let (entries, reaches_window, outer_path, outer_target) = {
        let mut doc = ctx.doc.borrow_mut();
        let path = doc.shadow_event_path(target_id, composed);
        let document_id = doc.root_node().id;
        let entries: Vec<_> = path
            .iter()
            .copied()
            .map(|id| Entry {
                id,
                target: doc.retarget_shadow_event(target_id, id),
                visible: doc.visible_shadow_event_path(&path, id),
            })
            .collect();
        (
            entries,
            path.last() == Some(&document_id),
            doc.visible_shadow_event_path(&path, document_id),
            doc.retarget_shadow_event(target_id, document_id),
        )
    };

    // Cleanup uses the last observer's adjusted target. A local event must
    // not gain an outside target that was never on its propagation path.
    let final_target = entries.last().map_or(target_id, |entry| entry.target);
    let clear_target = ctx
        .doc
        .borrow()
        .containing_shadow_root(final_target)
        .is_some();
    let result = (|| {
        let mut called = false;
        for entry in entries.iter().rev().filter(|entry| entry.id != target_id) {
            if stopped(event) {
                break;
            }
            called |= invoke(ctx, event, &name, entry, true, reaches_window, context)?;
        }
        for entry in &entries {
            if stopped(event) {
                break;
            }
            let at_target = entry.id == target_id || entry.id == entry.target;
            if !at_target && !bubbles {
                continue;
            }
            if entry.id == target_id {
                called |= invoke(ctx, event, &name, entry, true, reaches_window, context)?;
            }
            if !immediate(event) {
                called |= invoke(ctx, event, &name, entry, false, reaches_window, context)?;
            }
        }

        if reaches_window && bubbles && !stopped(event) {
            let global = context.global_object().clone();
            let listeners = {
                let mut state = ctx.state.borrow_mut();
                let listeners = state.window_listeners.entry(name.clone()).or_default();
                let callbacks = listeners.clone();
                listeners.retain(|listener| !listener.once);
                callbacks
            };
            let target: JsValue = node_wrapper(ctx, outer_target, context).into();
            define_value(event, "target", target.clone(), context);
            define_value(event, "srcElement", target, context);
            define_value(event, "currentTarget", global.clone().into(), context);
            define_value(event, "eventPhase", JsValue::from(3), context);
            let mut path: Vec<_> = outer_path
                .iter()
                .map(|&id| node_wrapper(ctx, id, context))
                .collect();
            path.push(global.clone());
            set_event_path(event, path);
            for listener in listeners {
                called = true;
                let _ = listener.callback.call(
                    &global.clone().into(),
                    &[event.clone().into()],
                    context,
                );
                if immediate(event) {
                    break;
                }
            }
        }
        let prevented = cancelable
            && event
                .downcast_ref::<EventRef>()
                .is_some_and(|data| data.prevented.get());
        Ok(DispatchResult {
            called,
            prevented,
            stopped: stopped(event),
        })
    })();

    define_value(event, "currentTarget", JsValue::null(), context);
    define_value(event, "eventPhase", JsValue::from(0), context);
    set_event_path(event, Vec::new());
    let target = if clear_target {
        JsValue::null()
    } else {
        node_wrapper(ctx, final_target, context).into()
    };
    define_value(event, "target", target.clone(), context);
    define_value(event, "srcElement", target, context);
    result
}

fn invoke(
    ctx: &DomCtx,
    event: &JsObject,
    name: &str,
    entry: &Entry,
    capture: bool,
    reaches_window: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let callbacks = {
        let mut state = ctx.state.borrow_mut();
        let mut callbacks = Vec::new();
        if let Some(listeners) = state
            .node_listeners
            .get_mut(&entry.id)
            .and_then(|by_type| by_type.get_mut(name))
        {
            for listener in listeners
                .iter()
                .filter(|listener| listener.capture == capture)
            {
                if let Some(callback) = listener.callback.upgrade() {
                    callbacks.push(callback);
                }
            }
            listeners.retain(|listener| !(listener.capture == capture && listener.once));
        }
        callbacks
    };
    super::node::sync_node_listener_callbacks(ctx, entry.id, context);
    let wrapper = node_wrapper(ctx, entry.id, context);
    let mut callbacks = callbacks;
    if !capture {
        if let Some(handler) = wrapper
            .get(JsString::from(format!("on{name}")), context)?
            .as_callable()
        {
            callbacks.push(handler.clone());
        }
    }
    if callbacks.is_empty() {
        return Ok(false);
    }

    let target: JsValue = node_wrapper(ctx, entry.target, context).into();
    define_value(event, "target", target.clone(), context);
    define_value(event, "srcElement", target, context);
    define_value(event, "currentTarget", wrapper.clone().into(), context);
    let phase = if entry.id == entry.target {
        2
    } else if capture {
        1
    } else {
        3
    };
    define_value(event, "eventPhase", JsValue::from(phase), context);
    let mut path: Vec<_> = entry
        .visible
        .iter()
        .map(|&id| node_wrapper(ctx, id, context))
        .collect();
    if reaches_window {
        path.push(context.global_object().clone());
    }
    set_event_path(event, path);
    for callback in callbacks {
        let _ = callback.call(&wrapper.clone().into(), &[event.clone().into()], context);
        if immediate(event) {
            break;
        }
    }
    Ok(true)
}

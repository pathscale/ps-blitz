//! JavaScript DOM API bindings backed by `blitz-dom`.
//!
//! The bindings are implemented as prototype objects built with Boa's native object
//! APIs. Each DOM node is represented by a single cached JS wrapper object (see
//! [`node_wrapper`]) whose native data is a [`NodeRef`] holding the `blitz-dom`
//! node id. Native functions look the document up via the [`DomCtx`] stored as
//! host-defined data on the Boa [`Context`].

pub(crate) mod custom_elements;
pub(crate) mod document;
pub(crate) mod element;
pub(crate) mod event;
pub(crate) mod node;
pub(crate) mod style;

use blitz_dom::NodeId;
use blitz_dom::node::NodeData;
use blitz_dom::{LocalName, Namespace, QualName};
use boa_engine::object::{FunctionObjectBuilder, JsObject, WeakJsObject};
use boa_engine::property::{PropertyDescriptor, PropertyKey};
use boa_engine::value::JsValue;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, NativeFunction, Trace,
};

use crate::state::{DomCtx, DomProtos};

/// Native data attached to DOM node wrapper objects
#[derive(Trace, Finalize, JsData)]
pub(crate) struct NodeRef {
    #[unsafe_ignore_trace]
    pub node_id: NodeId,
}

/// Fetch the [`DomCtx`] from the Boa context's host-defined data
pub(crate) fn dom_ctx(context: &mut Context) -> JsResult<DomCtx> {
    context.get_data::<DomCtx>().cloned().ok_or_else(|| {
        JsNativeError::typ()
            .with_message("DOM context missing")
            .into()
    })
}

/// Extract the node id from a JS DOM node wrapper
pub(crate) fn node_id_of_value(value: &JsValue) -> Option<NodeId> {
    value.as_object().and_then(|obj| {
        obj.downcast_ref::<NodeRef>()
            .map(|node_ref| node_ref.node_id)
    })
}

/// Extract the node id from the `this` value of a native function
pub(crate) fn this_node_id(this: &JsValue) -> JsResult<NodeId> {
    node_id_of_value(this).ok_or_else(|| {
        JsNativeError::typ()
            .with_message("`this` is not a DOM node")
            .into()
    })
}

/// Get (or create) the unique JS wrapper object for a DOM node.
///
/// Wrappers are cached in [`RuntimeState::node_wrappers`](crate::state::RuntimeState::node_wrappers)
/// so that object identity (`===`) and expando properties behave as scripts expect.
pub(crate) fn node_wrapper(ctx: &DomCtx, node_id: NodeId, _context: &mut Context) -> JsObject {
    // Upgraded rather than cloned: the cache holds weak handles, so an entry
    // whose wrapper nothing else kept has been collected and a fresh one is
    // built below. Identity still holds for every wrapper something is actually
    // holding, which is the only case identity can be observed in.
    if let Some(wrapper) = ctx
        .state
        .borrow()
        .node_wrappers
        .get(&node_id)
        .and_then(WeakJsObject::upgrade)
    {
        return wrapper;
    }

    let proto = {
        let doc = ctx.doc.borrow();
        let state = ctx.state.borrow();
        let protos = state.protos();
        match doc.get_node(node_id).map(|node| &node.data) {
            Some(NodeData::Document(_)) => protos.document.clone(),
            Some(NodeData::Element(_)) | Some(NodeData::AnonymousBlock(_)) => {
                protos.element.clone()
            }
            Some(NodeData::Text(_)) | Some(NodeData::Comment { .. }) => {
                protos.character_data.clone()
            }
            // A shadow root is not exposed through any of the specific
            // prototypes: script reaches it only via `element.shadowRoot`,
            // which is not implemented, so the plain Node prototype is right.
            Some(NodeData::ShadowRoot(_)) => protos.node.clone(),
            None => protos.node.clone(),
        }
    };

    let wrapper = JsObject::from_proto_and_data(Some(proto), NodeRef { node_id });
    ctx.state
        .borrow_mut()
        .node_wrappers
        .insert(node_id, wrapper.downgrade());
    wrapper
}

/// Detach a node, and free it when script can no longer reach it.
///
/// `remove_node` keeps a removed node alive so a wrapper still holding it stays
/// usable, which is the right contract - `removeChild` returns the node and a
/// browser keeps it working. Applying that to *every* removed node is what grew
/// the document without bound, because the overwhelming majority are rows a
/// framework built and dropped and no script will mention again.
///
/// The wrapper cache is weak now, so it answers this honestly: an entry that
/// upgrades means something is still holding the wrapper, and one that does not
/// means the collector has taken it and the node is unreachable.
pub(crate) fn remove_and_free_node(ctx: &DomCtx, node_id: NodeId, context: &mut Context) {
    node::unroot_detached_listener_subtree(ctx, node_id, context);
    ctx.mutate_doc().mutate().remove_node(node_id);
    // Parked rather than judged. Asking "can script still reach this" right now
    // always answers yes: the wrapper was alive a moment ago, because the call
    // that removed the node went through it. `sweep_detached_nodes` asks later,
    // once the collector has had a chance to disagree.
    ctx.state.borrow_mut().detached_nodes.push(node_id);
}

/// Stop treating a node as detached when script inserts it again before the
/// collector judged it. DOM move operations commonly remove and append in one
/// turn, and a later sweep must not drop the newly connected subtree.
pub(crate) fn mark_node_reattached(ctx: &DomCtx, node_id: NodeId) {
    let ids = {
        let doc = ctx.doc.borrow();
        let mut ids = Vec::new();
        let mut stack = vec![node_id];
        while let Some(id) = stack.pop() {
            ids.push(id);
            if let Some(node) = doc.get_node(id) {
                stack.extend(node.children.iter().copied());
            }
        }
        ids
    };
    let mut state = ctx.state.borrow_mut();
    state
        .detached_nodes
        .retain(|candidate| *candidate != node_id);
    for id in ids {
        let has_listeners = state
            .node_listeners
            .get(&id)
            .is_some_and(|by_type| by_type.values().any(|listeners| !listeners.is_empty()));
        if has_listeners
            && let Some(wrapper) = state
                .node_wrappers
                .get(&id)
                .and_then(|wrapper| wrapper.upgrade())
        {
            state.listener_wrappers.insert(id, wrapper);
        }
    }
}

/// Free detached nodes whose wrappers the collector has taken.
///
/// This is the half that could not be written before wrappers were weak. A node
/// is removed while script may still hold it, and that cannot be judged at the
/// moment of removal; it can be judged later, and "later" is any point after a
/// collection. An entry that still upgrades is a node something is holding on
/// to, so it stays detached exactly as before.
///
/// Called from the poll loop, so the cost is bounded by how much was removed
/// rather than by the size of the document.
pub(crate) fn sweep_detached_nodes(ctx: &DomCtx) {
    // Boa's automatic collector is allocation-driven, so a large application
    // can repeatedly remount DOM trees without crossing its collection
    // threshold soon enough. Weak wrappers only become observably dead after a
    // collection; without this bound, detached nodes and their listener-owned
    // Solid closures grow linearly while the visible tree remains flat.
    //
    // Do not collect on every removal. Small detached sets are normal DOM
    // behavior and Boa may collect them naturally. Crossing this threshold is
    // the signal that delayed collection is now more expensive than one GC.
    const DETACHED_NODE_GC_THRESHOLD: usize = 256;
    if ctx.state.borrow().detached_nodes.len() >= DETACHED_NODE_GC_THRESHOLD {
        boa_gc::force_collect();
    }

    let candidates = std::mem::take(&mut ctx.state.borrow_mut().detached_nodes);
    if candidates.is_empty() {
        return;
    }

    let mut keep = Vec::new();
    let mut freed = Vec::new();

    for node_id in candidates {
        let held = {
            let state = ctx.state.borrow();
            let doc = ctx.doc.borrow();
            // Gone already, through a parent that was freed first.
            if doc.get_node(node_id).is_none() {
                continue;
            }
            let mut stack = vec![node_id];
            let mut held = false;
            while let Some(id) = stack.pop() {
                let wrapper_alive = state
                    .node_wrappers
                    .get(&id)
                    .and_then(WeakJsObject::upgrade)
                    .is_some();
                // A listener belongs to its node; it is not an external root.
                // Treating the listener table itself as reachability keeps
                // every removed interactive subtree forever, because buttons
                // necessarily have listeners. A live wrapper is the evidence
                // that script outside the node still owns it. If no wrapper
                // survives, the listeners are dropped with the node below.
                if wrapper_alive {
                    held = true;
                    break;
                }
                if let Some(node) = doc.get_node(id) {
                    stack.extend(node.children.iter().copied());
                }
            }
            held
        };

        if held {
            // Still reachable. Look again after the next collection: a
            // framework often holds a row for a tick and then lets go.
            keep.push(node_id);
            continue;
        }

        ctx.mutate_doc()
            .mutate()
            .remove_and_drop_node_with(node_id, &mut |id| freed.push(id));
    }

    // The caches are keyed by id, so a freed id has to leave them: the slot can
    // be reused, and a stale entry would hand out a wrapper for a different
    // node entirely.
    let mut state = ctx.state.borrow_mut();
    state.detached_nodes = keep;
    for id in freed {
        state.node_wrappers.remove(&id);
        state.dataset_wrappers.remove(&id);
        state.class_list_wrappers.remove(&id);
        state.node_listeners.remove(&id);
        state.listener_wrappers.remove(&id);
    }
}

/// Convert an optional node id to a JS value (wrapper object or `null`)
pub(crate) fn node_or_null(
    ctx: &DomCtx,
    node_id: Option<NodeId>,
    context: &mut Context,
) -> JsValue {
    match node_id {
        Some(node_id) => node_wrapper(ctx, node_id, context).into(),
        None => JsValue::null(),
    }
}

/// Construct an HTML `QualName` from a string
pub(crate) fn qual_name(local: &str) -> QualName {
    QualName::new(None, markup5ever::ns!(html), LocalName::from(local))
}

/// Construct a `QualName` in the given namespace from a string
pub(crate) fn qual_name_ns(local: &str, ns: &str) -> QualName {
    QualName::new(None, Namespace::from(ns), LocalName::from(local))
}

pub(crate) fn js_str(s: &str) -> JsValue {
    JsString::from(s).into()
}

/// Convert a JS value to a Rust `String` (via ECMAScript `ToString`)
///
/// This is the single point at which a string crosses from the JavaScript heap
/// into the host, so it is also where that traffic is counted. See
/// [`crate::script_stats::BoundaryCounters`]: the count is off unless deep
/// profiling is on, and it exists because bytes across the boundary is the
/// quantity a binding design changes, while a wall-clock number folds it
/// together with everything else the call did.
pub(crate) fn to_rust_string(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let string = value.to_string(context)?.to_std_string_lossy();
    crate::script_stats::record_boundary_string(string.len());
    Ok(string)
}

type NativeFnPtr = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// Define a method on a prototype object
pub(crate) fn define_method(
    obj: &JsObject,
    name: &str,
    length: usize,
    body: NativeFnPtr,
    context: &mut Context,
) {
    let function = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(body))
        .name(JsString::from(name))
        .length(length)
        .build();
    obj.define_property_or_throw(
        PropertyKey::from(JsString::from(name)),
        PropertyDescriptor::builder()
            .value(function)
            .writable(true)
            .enumerable(false)
            .configurable(true)
            .build(),
        context,
    )
    .expect("failed to define DOM method");
}

/// Define an accessor (getter/setter pair) on a prototype object
pub(crate) fn define_accessor(
    obj: &JsObject,
    name: &str,
    getter: Option<NativeFnPtr>,
    setter: Option<NativeFnPtr>,
    context: &mut Context,
) {
    let getter = getter.map(|g| {
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(g))
            .name(JsString::from(format!("get {name}")))
            .length(0)
            .build()
    });
    let setter = setter.map(|s| {
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(s))
            .name(JsString::from(format!("set {name}")))
            .length(1)
            .build()
    });
    let mut builder = PropertyDescriptor::builder()
        .enumerable(false)
        .configurable(true);
    if let Some(getter) = getter {
        builder = builder.get(getter);
    }
    if let Some(setter) = setter {
        builder = builder.set(setter);
    }
    obj.define_property_or_throw(
        PropertyKey::from(JsString::from(name)),
        builder.build(),
        context,
    )
    .expect("failed to define DOM accessor");
}

/// Define a plain data property
pub(crate) fn define_value(obj: &JsObject, name: &str, value: JsValue, context: &mut Context) {
    obj.define_property_or_throw(
        PropertyKey::from(JsString::from(name)),
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .enumerable(false)
            .configurable(true)
            .build(),
        context,
    )
    .expect("failed to define DOM property");
}

/// Event types for which `on<event>` IDL-style properties are defined on the
/// node prototype. Frameworks (e.g. Preact) use `'onclick' in dom` checks to
/// infer the correct casing of event names, so these need to be present.
pub(crate) const ON_EVENT_TYPES: &[&str] = &[
    "click",
    "dblclick",
    "contextmenu",
    "mousedown",
    "mouseup",
    "mousemove",
    "mouseenter",
    "mouseleave",
    "mouseover",
    "mouseout",
    "pointerdown",
    "pointerup",
    "pointermove",
    "pointercancel",
    "pointerenter",
    "pointerleave",
    "pointerover",
    "pointerout",
    "touchstart",
    "touchmove",
    "touchend",
    "touchcancel",
    "keydown",
    "keyup",
    "keypress",
    "input",
    "change",
    "focus",
    "blur",
    "focusin",
    "focusout",
    "submit",
    "scroll",
    "wheel",
    "load",
];

/// Initialise the DOM prototype objects and store them in the runtime state
pub(crate) fn init_protos(ctx: &DomCtx, context: &mut Context) {
    let object_proto = context.intrinsics().constructors().object().prototype();

    let node_proto = JsObject::with_object_proto(context.intrinsics());
    node::init_node_proto(&node_proto, context);

    // `on<event>` IDL-style properties (default null)
    for event_type in ON_EVENT_TYPES {
        define_value(
            &node_proto,
            &format!("on{event_type}"),
            JsValue::null(),
            context,
        );
    }

    let element_proto = JsObject::with_object_proto(context.intrinsics());
    element_proto.set_prototype(Some(node_proto.clone()));
    element::init_element_proto(&element_proto, context);

    let character_data_proto = JsObject::with_object_proto(context.intrinsics());
    character_data_proto.set_prototype(Some(node_proto.clone()));
    node::init_character_data_proto(&character_data_proto, context);

    let document_proto = JsObject::with_object_proto(context.intrinsics());
    document_proto.set_prototype(Some(node_proto.clone()));
    document::init_document_proto(&document_proto, context);

    let event_proto = JsObject::with_object_proto(context.intrinsics());
    event::init_event_proto(&event_proto, context);

    let style_proto = JsObject::with_object_proto(context.intrinsics());
    style::init_style_proto(&style_proto, context);

    let _ = object_proto;

    ctx.state.borrow_mut().protos = Some(DomProtos {
        node: node_proto,
        element: element_proto,
        character_data: character_data_proto.clone(),
        document: document_proto,
        event: event_proto.clone(),
        style: style_proto,
    });
    event::register_event_constructor(&event_proto, context);
    event::register_custom_event_constructor(&event_proto, context);
    document::register_text_constructor(&character_data_proto, context);
}

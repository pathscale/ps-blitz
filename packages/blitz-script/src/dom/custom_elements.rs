//! Script custom elements, backed by the native DOM candidate index.
//!
//! Mutators record native reactions. DOM binding boundaries invoke the current
//! element queue; mutations outside those boundaries use a microtask backup
//! queue. Constructors receive the existing wrapper through the HTML
//! construction stack, so class bodies execute without replacing node identity.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::node::script_custom_element::{ReactionKind, State};
use blitz_dom::{Attribute, LocalName, NodeData, NodeId, ns};
use boa_engine::object::builtins::JsPromise;
use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::Attribute as PropertyAttribute;
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsString, JsValue, NativeFunction,
    Trace, js_string,
};
use rustc_hash::FxHashMap;

use super::{
    define_method, dom_ctx, js_str, node_id_of_value, node_wrapper, qual_name, qual_name_ns,
    this_node_id, to_rust_string,
};
use crate::state::DomCtx;

struct Definition {
    constructor: JsObject,
    prototype: JsObject,
    local_name: String,
    interface: &'static str,
    customized: bool,
    observed: Arc<HashSet<String>>,
    connected: Option<JsObject>,
    disconnected: Option<JsObject>,
    attribute_changed: Option<JsObject>,
    adopted: Option<JsObject>,
}

struct PendingDefinition {
    promise: JsPromise,
    resolve: JsObject,
}

enum ElementReaction {
    Upgrade,
    Callback(JsObject, Vec<JsValue>),
}

struct ElementQueue {
    wrapper: JsObject,
    reactions: VecDeque<ElementReaction>,
}

struct Construction {
    constructor: JsObject,
    element: JsObject,
    consumed: bool,
}

#[derive(Default)]
struct RegistryState {
    definitions: FxHashMap<String, Arc<Definition>>,
    pending: FxHashMap<String, PendingDefinition>,
    elements: FxHashMap<NodeId, ElementQueue>,
    stack: Vec<VecDeque<NodeId>>,
    backup: VecDeque<NodeId>,
    construction: Vec<Construction>,
    defining: bool,
    backup_scheduled: bool,
    registry: Option<JsObject>,
    sequence: Option<JsObject>,
    promise_then: Option<JsObject>,
}

#[derive(Clone, Trace, Finalize, JsData)]
struct RegistryContext {
    #[unsafe_ignore_trace]
    state: Rc<RefCell<RegistryState>>,
}

fn registry_context(context: &Context) -> Option<RegistryContext> {
    context.get_data::<RegistryContext>().cloned()
}

fn registry(context: &Context) -> RegistryContext {
    registry_context(context).expect("custom element registry not installed")
}

fn dom_error(name: &str, message: &str, context: &mut Context) -> JsError {
    crate::dom_exception::error(name, message, context)
}

fn check_receiver(this: &JsValue, context: &mut Context) -> JsResult<()> {
    let ce = registry(context);
    let valid = this.as_object().is_some_and(|object| {
        ce.state
            .borrow()
            .registry
            .as_ref()
            .is_some_and(|registry| JsObject::equals(&object, registry))
    });
    if !valid {
        return Err(JsNativeError::typ()
            .with_message("Illegal CustomElementRegistry receiver")
            .into());
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && name.contains('-')
        && chars.all(|character| {
            matches!(
                character,
                '-' | '.' | '0'..='9' | '_' | 'a'..='z'
                    | '\u{b7}'
                    | '\u{c0}'..='\u{d6}'
                    | '\u{d8}'..='\u{f6}'
                    | '\u{f8}'..='\u{37d}'
                    | '\u{37f}'..='\u{1fff}'
                    | '\u{200c}'..='\u{200d}'
                    | '\u{203f}'..='\u{2040}'
                    | '\u{2070}'..='\u{218f}'
                    | '\u{2c00}'..='\u{2fef}'
                    | '\u{3001}'..='\u{d7ff}'
                    | '\u{f900}'..='\u{fdcf}'
                    | '\u{fdf0}'..='\u{fffd}'
                    | '\u{10000}'..='\u{effff}'
            )
        })
        && !matches!(
            name,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
}

fn builtin_interface(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "a" => "HTMLAnchorElement",
        "body" => "HTMLBodyElement",
        "button" => "HTMLButtonElement",
        "div" => "HTMLDivElement",
        "form" => "HTMLFormElement",
        "head" => "HTMLHeadElement",
        "img" => "HTMLImageElement",
        "input" => "HTMLInputElement",
        "li" => "HTMLLIElement",
        "ol" => "HTMLOListElement",
        "option" => "HTMLOptionElement",
        "p" => "HTMLParagraphElement",
        "script" => "HTMLScriptElement",
        "select" => "HTMLSelectElement",
        "span" => "HTMLSpanElement",
        "style" => "HTMLStyleElement",
        "template" => "HTMLTemplateElement",
        "textarea" => "HTMLTextAreaElement",
        "ul" => "HTMLUListElement",
        "abbr" | "address" | "article" | "aside" | "b" | "bdi" | "bdo" | "code" | "dd" | "dfn"
        | "dt" | "em" | "footer" | "header" | "hgroup" | "i" | "main" | "nav" | "s" | "section"
        | "small" | "strong" | "sub" | "sup" | "u" | "var" => "HTMLElement",
        _ => return None,
    })
}

fn callback(prototype: &JsObject, name: &str, context: &mut Context) -> JsResult<Option<JsObject>> {
    let value = prototype.get(JsString::from(name), context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let Some(function) = value.as_object().filter(|object| object.is_callable()) else {
        return Err(JsNativeError::typ()
            .with_message(format!("{name} must be callable"))
            .into());
    };
    Ok(Some(function))
}

pub(crate) fn install(ctx: &DomCtx, context: &mut Context) {
    let promise_constructor = context
        .global_object()
        .get(js_string!("Promise"), context)
        .expect("Promise missing")
        .as_object()
        .expect("Promise is not an object");
    let promise_prototype = promise_constructor
        .get(js_string!("prototype"), context)
        .expect("Promise prototype missing")
        .as_object()
        .expect("Promise prototype is not an object");
    let promise_then = promise_prototype
        .get(js_string!("then"), context)
        .expect("Promise.then missing")
        .as_object()
        .expect("Promise.then is not an object");
    let object = JsObject::with_object_proto(context.intrinsics());
    context.insert_data(RegistryContext {
        state: Rc::new(RefCell::new(RegistryState {
            registry: Some(object.clone()),
            promise_then: Some(promise_then),
            ..Default::default()
        })),
    });
    define_method(&object, "define", 2, define, context);
    define_method(&object, "get", 1, get, context);
    define_method(&object, "getName", 1, get_name, context);
    define_method(&object, "whenDefined", 1, when_defined, context);
    define_method(&object, "upgrade", 1, upgrade, context);
    context
        .register_global_property(
            js_string!("customElements"),
            object,
            PropertyAttribute::WRITABLE.union(PropertyAttribute::CONFIGURABLE),
        )
        .expect("failed to install customElements");
    // The native HTMLElement interfaces (dom/interfaces.rs) call this when a
    // class extending them is constructed, so `super()` in a custom element
    // class runs the custom element construction steps below.
    context.insert_data(super::interfaces::HtmlConstructorHook {
        construct: interface_construct,
    });
    for (name, length, body) in [
        (
            "__blitzHTMLConstructor",
            2,
            html_constructor as super::NativeFnPtr,
        ),
        (
            "__blitzDOMPrototype",
            1,
            interface_prototype as super::NativeFnPtr,
        ),
        (
            "__blitzCEInitialize",
            1,
            initialize_sequence as super::NativeFnPtr,
        ),
    ] {
        context
            .register_global_callable(
                JsString::from(name),
                length,
                NativeFunction::from_fn_ptr(body),
            )
            .expect("failed to install custom element bridge");
    }
    let document = ctx.state.borrow().protos().document.clone();
    define_method(&document, "createElement", 1, create_element, context);
    define_method(&document, "createElementNS", 2, create_element_ns, context);
    define_method(&document, "createTextNode", 1, create_text, context);
    define_method(&document, "createComment", 1, create_comment, context);
    define_method(
        &document,
        "createDocumentFragment",
        0,
        create_fragment,
        context,
    );
    define_method(&document, "adoptNode", 1, adopt_node, context);
    define_method(&document, "importNode", 2, import_node, context);
}

fn initialize_sequence(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    registry(context).state.borrow_mut().sequence = args.first().and_then(JsValue::as_object);
    Ok(JsValue::undefined())
}

/// The native interface prototype for a DOM interface name (dom/interfaces.rs).
fn interface_prototype(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    Ok(super::interfaces::prototype(&name, context).into())
}

/// Select the HTML interface prototype, or the captured custom prototype when
/// reconstructing a weakly cached wrapper.
pub(crate) fn element_prototype(ctx: &DomCtx, node: NodeId, context: &Context) -> Option<JsObject> {
    let ce = registry_context(context)?;
    let doc = ctx.doc.borrow();
    let element = doc.get_node(node)?.element_data()?;
    if element.name.ns != ns!(html) {
        return None;
    }
    let state = ce.state.borrow();
    if matches!(
        doc.script_custom_element_state(node),
        State::Custom | State::Upgrading
    ) && let Some(name) = doc.script_custom_element_name(node)
        && let Some(definition) = state.definitions.get(name)
    {
        return Some(definition.prototype.clone());
    }
    // Everything else takes its native interface (dom/interfaces.rs).
    None
}

pub(crate) fn define(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    check_receiver(this, context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(constructor) = args
        .get(1)
        .and_then(JsValue::as_object)
        .filter(|constructor| constructor.is_constructor())
    else {
        return Err(JsNativeError::typ()
            .with_message("Constructor required")
            .into());
    };
    if !valid_name(&name) {
        return Err(dom_error(
            "SyntaxError",
            "Invalid custom element name",
            context,
        ));
    }
    let ce = registry(context);
    {
        let state = ce.state.borrow();
        if state.defining {
            drop(state);
            return Err(dom_error(
                "NotSupportedError",
                "A definition is already running",
                context,
            ));
        }
        if state.definitions.contains_key(&name)
            || state
                .definitions
                .values()
                .any(|definition| JsObject::equals(&definition.constructor, &constructor))
        {
            drop(state);
            return Err(dom_error(
                "NotSupportedError",
                "Duplicate custom element definition",
                context,
            ));
        }
    }
    ce.state.borrow_mut().defining = true;
    let definition = (|| -> JsResult<Definition> {
        let mut local_name = name.clone();
        let mut interface = "HTMLElement";
        let mut customized = false;
        if let Some(options) = args.get(2).filter(|value| !value.is_null_or_undefined()) {
            let Some(options) = options.as_object() else {
                return Err(JsNativeError::typ()
                    .with_message("Options must be a dictionary")
                    .into());
            };
            let extends = options.get(js_string!("extends"), context)?;
            if !extends.is_undefined() {
                local_name = to_rust_string(&extends, context)?;
                let Some(builtin) = builtin_interface(&local_name) else {
                    return Err(dom_error(
                        "NotSupportedError",
                        "Unsupported extends element",
                        context,
                    ));
                };
                interface = builtin;
                customized = true;
            }
        }
        let Some(prototype) = constructor
            .get(js_string!("prototype"), context)?
            .as_object()
        else {
            return Err(JsNativeError::typ()
                .with_message("Constructor prototype must be an object")
                .into());
        };
        let connected = callback(&prototype, "connectedCallback", context)?;
        let disconnected = callback(&prototype, "disconnectedCallback", context)?;
        let adopted = callback(&prototype, "adoptedCallback", context)?;
        let attribute_changed = callback(&prototype, "attributeChangedCallback", context)?;
        let mut observed = HashSet::new();
        if attribute_changed.is_some() {
            let value = constructor.get(js_string!("observedAttributes"), context)?;
            if !value.is_undefined() {
                let sequence = ce
                    .state
                    .borrow()
                    .sequence
                    .clone()
                    .expect("CE sequence helper missing");
                let array = sequence
                    .call(&JsValue::undefined(), &[value], context)?
                    .as_object()
                    .expect("CE sequence helper returned a non-object");
                let length = array
                    .get(js_string!("length"), context)?
                    .to_number(context)? as usize;
                for index in 0..length {
                    observed.insert(to_rust_string(&array.get(index as u32, context)?, context)?);
                }
            }
        }
        Ok(Definition {
            constructor: constructor.clone(),
            prototype,
            local_name,
            interface,
            customized,
            observed: Arc::new(observed),
            connected,
            disconnected,
            attribute_changed,
            adopted,
        })
    })();
    ce.state.borrow_mut().defining = false;
    let definition = Arc::new(definition?);
    let ctx = dom_ctx(context)?;
    ce.state
        .borrow_mut()
        .definitions
        .insert(name.clone(), Arc::clone(&definition));
    ctx.state
        .borrow_mut()
        .custom_element_definitions
        .insert(name.clone(), constructor.clone());
    let existing = ctx.doc.borrow_mut().define_script_custom_element(
        &name,
        LocalName::from(definition.local_name.as_str()),
        Arc::clone(&definition.observed),
    );

    // The first definition also needs a reaction scope: the generic fast path
    // deliberately skips scopes while there are no definitions.
    collect(&ctx, context);
    ce.state.borrow_mut().stack.push(VecDeque::new());
    for node in existing {
        enqueue(&ctx, node, ElementReaction::Upgrade, context);
    }
    if let Some(waiting) = ce.state.borrow_mut().pending.remove(&name) {
        waiting
            .resolve
            .call(&JsValue::undefined(), &[constructor.into()], context)?;
    }
    finish_scope(&ctx, context);
    Ok(JsValue::undefined())
}

pub(crate) fn get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    check_receiver(this, context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let constructor = ctx
        .state
        .borrow()
        .custom_element_definitions
        .get(&name)
        .cloned();
    Ok(constructor.map_or(JsValue::undefined(), JsValue::from))
}

pub(crate) fn get_name(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    check_receiver(this, context)?;
    let Some(constructor) = args.first().and_then(JsValue::as_object) else {
        return Err(JsNativeError::typ()
            .with_message("Constructor required")
            .into());
    };
    let ctx = dom_ctx(context)?;
    let name = ctx
        .state
        .borrow()
        .custom_element_definitions
        .iter()
        .find(|(_, value)| JsObject::equals(value, &constructor))
        .map(|(name, _)| name.clone());
    Ok(name.map_or(JsValue::null(), |name| js_str(&name)))
}

pub(crate) fn when_defined(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    check_receiver(this, context)?;
    let name = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ce = registry(context);
    if !valid_name(&name) {
        let (promise, functions) = JsPromise::new_pending(context);
        let error = dom_error("SyntaxError", "Invalid custom element name", context)
            .into_opaque(context)?;
        functions
            .reject
            .call(&JsValue::undefined(), &[error], context)?;
        return Ok(promise.into());
    }
    if let Some(waiting) = ce.state.borrow().pending.get(&name) {
        return Ok(waiting.promise.clone().into());
    }
    let constructor = ce
        .state
        .borrow()
        .definitions
        .get(&name)
        .map(|definition| definition.constructor.clone());
    let (promise, functions) = JsPromise::new_pending(context);
    if let Some(constructor) = constructor {
        functions
            .resolve
            .call(&JsValue::undefined(), &[constructor.into()], context)?;
        return Ok(promise.into());
    }
    ce.state.borrow_mut().pending.insert(
        name,
        PendingDefinition {
            promise: promise.clone(),
            resolve: functions.resolve.into(),
        },
    );
    Ok(promise.into())
}

pub(crate) fn upgrade(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    check_receiver(this, context)?;
    let node = args
        .first()
        .and_then(node_id_of_value)
        .ok_or_else(|| JsNativeError::typ().with_message("A DOM root is required"))?;
    let ctx = dom_ctx(context)?;
    let candidates = ctx.doc.borrow().script_custom_element_candidates(node);
    for node in candidates {
        enqueue(&ctx, node, ElementReaction::Upgrade, context);
    }
    Ok(JsValue::undefined())
}

fn definition_for(ctx: &DomCtx, node: NodeId, context: &Context) -> Option<Arc<Definition>> {
    let name = {
        let doc = ctx.doc.borrow();
        if !doc.script_custom_element_is_defined(node) {
            return None;
        }
        doc.script_custom_element_name(node)?.to_owned()
    };
    registry(context)
        .state
        .borrow()
        .definitions
        .get(&name)
        .cloned()
}

fn enqueue(ctx: &DomCtx, node: NodeId, reaction: ElementReaction, context: &mut Context) {
    let wrapper = node_wrapper(ctx, node, context);
    let ce = registry(context);
    let mut state = ce.state.borrow_mut();
    state
        .elements
        .entry(node)
        .or_insert_with(|| ElementQueue {
            wrapper,
            reactions: VecDeque::new(),
        })
        .reactions
        .push_back(reaction);
    if let Some(queue) = state.stack.last_mut() {
        queue.push_back(node);
    } else {
        state.backup.push_back(node);
    }
}

fn enqueue_callback(
    ctx: &DomCtx,
    node: NodeId,
    callback: &Option<JsObject>,
    args: Vec<JsValue>,
    context: &mut Context,
) {
    if let Some(callback) = callback {
        enqueue(
            ctx,
            node,
            ElementReaction::Callback(callback.clone(), args),
            context,
        );
    }
}

fn collect(ctx: &DomCtx, context: &mut Context) {
    let records = ctx.doc.borrow_mut().take_script_custom_element_reactions();
    let ce = registry(context);
    for record in records {
        let definition = ce
            .state
            .borrow()
            .definitions
            .get(record.name.as_ref())
            .cloned();
        let Some(definition) = definition else {
            continue;
        };
        match record.kind {
            ReactionKind::Upgrade => enqueue(ctx, record.node, ElementReaction::Upgrade, context),
            ReactionKind::Connected => {
                enqueue_callback(ctx, record.node, &definition.connected, Vec::new(), context);
            }
            ReactionKind::Disconnected => {
                enqueue_callback(
                    ctx,
                    record.node,
                    &definition.disconnected,
                    Vec::new(),
                    context,
                );
            }
            ReactionKind::Attribute {
                name,
                old_value,
                new_value,
            } => {
                let namespace = if name.ns.as_ref().is_empty() {
                    JsValue::null()
                } else {
                    js_str(name.ns.as_ref())
                };
                enqueue_callback(
                    ctx,
                    record.node,
                    &definition.attribute_changed,
                    vec![
                        js_str(name.local.as_ref()),
                        old_value.as_deref().map_or(JsValue::null(), js_str),
                        new_value.as_deref().map_or(JsValue::null(), js_str),
                        namespace,
                    ],
                    context,
                );
            }
            ReactionKind::Adopted {
                old_document,
                new_document,
            } => {
                let old = node_wrapper(ctx, old_document, context);
                let new = node_wrapper(ctx, new_document, context);
                enqueue_callback(
                    ctx,
                    record.node,
                    &definition.adopted,
                    vec![old.into(), new.into()],
                    context,
                );
            }
        }
    }
}

fn perform_upgrade(ctx: &DomCtx, node: NodeId, creation: bool, context: &mut Context) {
    let definition = {
        let doc = ctx.doc.borrow();
        if doc.get_node(node).is_none()
            || doc.script_custom_element_state(node) != State::Undefined
            || doc.get_node(node).and_then(|node| node.owner_document) != Some(doc.root_node().id)
        {
            return;
        }
        drop(doc);
        definition_for(ctx, node, context)
    };
    let Some(definition) = definition else {
        return;
    };
    let wrapper = node_wrapper(ctx, node, context);
    let (attributes, connected) = {
        let doc = ctx.doc.borrow();
        let attributes = doc
            .get_node(node)
            .and_then(|node| node.element_data())
            .map(|element| {
                element
                    .attrs
                    .iter()
                    .filter(|attribute| definition.observed.contains(attribute.name.local.as_ref()))
                    .map(|attribute| (attribute.name.clone(), attribute.value.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (attributes, doc.script_node_is_connected(node))
    };
    ctx.doc
        .borrow_mut()
        .set_script_custom_element_state(node, State::Upgrading);
    for (name, value) in attributes {
        let namespace = if name.ns.as_ref().is_empty() {
            JsValue::null()
        } else {
            js_str(name.ns.as_ref())
        };
        enqueue_callback(
            ctx,
            node,
            &definition.attribute_changed,
            vec![
                js_str(name.local.as_ref()),
                JsValue::null(),
                js_str(&value),
                namespace,
            ],
            context,
        );
    }
    if connected {
        enqueue_callback(ctx, node, &definition.connected, Vec::new(), context);
    }
    let ce = registry(context);
    ce.state.borrow_mut().construction.push(Construction {
        constructor: definition.constructor.clone(),
        element: wrapper.clone(),
        consumed: false,
    });
    let result = definition.constructor.construct(&[], None, context);
    let construction = ce
        .state
        .borrow_mut()
        .construction
        .pop()
        .expect("construction stack missing");
    let result = result.and_then(|returned| {
        if !construction.consumed || !JsObject::equals(&returned, &wrapper) {
            return Err(JsNativeError::typ()
                .with_message("Custom constructor returned a different element")
                .into());
        }
        if creation {
            let doc = ctx.doc.borrow();
            let element = doc.get_node(node).and_then(|node| node.element_data());
            let invalid = doc.get_node(node).is_none_or(|node| {
                node.parent.is_some()
                    || !node.children.is_empty()
                    || node.owner_document != Some(doc.root_node().id)
            }) || element.is_none_or(|element| {
                element.name.ns != ns!(html)
                    || element.name.local.as_ref() != definition.local_name
                    || element.attrs.iter().any(|attribute| {
                        !definition.customized || attribute.name.local.as_ref() != "is"
                    })
            });
            if invalid {
                return Err(JsNativeError::typ()
                    .with_message("Custom constructor changed the new element's structure")
                    .into());
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => ctx
            .doc
            .borrow_mut()
            .set_script_custom_element_state(node, State::Custom),
        Err(error) => {
            ctx.doc
                .borrow_mut()
                .set_script_custom_element_state(node, State::Failed);
            ce.state.borrow_mut().elements.remove(&node);
            report(&error);
        }
    }
}

fn report(error: &JsError) {
    eprintln!("Custom element reaction: {error}");
}

fn invoke(ctx: &DomCtx, mut queue: VecDeque<NodeId>, context: &mut Context) {
    let ce = registry(context);
    while let Some(node) = queue.pop_front() {
        let constructing = ce
            .state
            .borrow()
            .construction
            .iter()
            .any(|frame| node_id_of_value(&frame.element.clone().into()) == Some(node));
        if constructing {
            continue;
        }
        loop {
            let next = {
                let mut state = ce.state.borrow_mut();
                state.elements.get_mut(&node).and_then(|queue| {
                    queue
                        .reactions
                        .pop_front()
                        .map(|reaction| (queue.wrapper.clone(), reaction))
                })
            };
            let Some((wrapper, reaction)) = next else {
                ce.state.borrow_mut().elements.remove(&node);
                break;
            };
            match reaction {
                ElementReaction::Upgrade => perform_upgrade(ctx, node, false, context),
                ElementReaction::Callback(callback, args) => {
                    if ctx.doc.borrow().script_custom_element_state(node) == State::Custom
                        && let Err(error) = callback.call(&wrapper.into(), &args, context)
                    {
                        report(&error);
                    }
                }
            }
            collect(ctx, context);
        }
    }
}

fn finish_scope(ctx: &DomCtx, context: &mut Context) {
    collect(ctx, context);
    let ce = registry(context);
    let queue = ce
        .state
        .borrow_mut()
        .stack
        .pop()
        .expect("reaction scope missing");
    invoke(ctx, queue, context);
    schedule_backup(context);
}

/// Deliver reactions recorded by a parser step before its next script runs.
/// The parser and DOM borrows must be released when `parse` returns, since
/// constructors and callbacks can reenter document input methods.
pub(crate) fn parser_scope<R>(context: &mut Context, parse: impl FnOnce(&mut Context) -> R) -> R {
    let Some(ce) = registry_context(context) else {
        return parse(context);
    };
    if ce.state.borrow().definitions.is_empty() {
        return parse(context);
    }
    let ctx = dom_ctx(context).expect("parser reaction scope needs a DOM context");
    collect(&ctx, context);
    ce.state.borrow_mut().stack.push(VecDeque::new());
    let result = parse(context);
    finish_scope(&ctx, context);
    result
}

/// Native binding boundary. There is no subtree walk here.
pub(crate) fn native_scope(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    body: super::NativeFnPtr,
) -> JsResult<JsValue> {
    let Some(ce) = registry_context(context) else {
        return body(this, args, context);
    };
    if ce.state.borrow().definitions.is_empty() {
        return body(this, args, context);
    }
    let ctx = dom_ctx(context)?;
    collect(&ctx, context);
    ce.state.borrow_mut().stack.push(VecDeque::new());
    let result = body(this, args, context);
    finish_scope(&ctx, context);
    result
}

fn schedule_backup(context: &mut Context) {
    let ce = registry(context);
    {
        let mut state = ce.state.borrow_mut();
        if state.backup.is_empty() || state.backup_scheduled {
            return;
        }
        state.backup_scheduled = true;
    }
    let callback =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(deliver_backup))
            .build();
    let (promise, functions) = JsPromise::new_pending(context);
    let then = ce.state.borrow().promise_then.clone().unwrap();
    let result = functions
        .resolve
        .call(&JsValue::undefined(), &[], context)
        .and_then(|_| then.call(&promise.into(), &[callback.into()], context));
    if let Err(error) = result {
        ce.state.borrow_mut().backup_scheduled = false;
        report(&error);
    }
}

fn deliver_backup(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let ce = registry(context);
    collect(&ctx, context);
    loop {
        let queue = std::mem::take(&mut ce.state.borrow_mut().backup);
        if queue.is_empty() {
            break;
        }
        invoke(&ctx, queue, context);
    }
    ce.state.borrow_mut().backup_scheduled = false;
    Ok(JsValue::undefined())
}

/// Called before the runtime drains jobs, for mutations made by Rust callers.
pub(crate) fn checkpoint(ctx: &DomCtx, context: &mut Context) {
    if let Some(ce) = registry_context(context)
        && !ce.state.borrow().definitions.is_empty()
    {
        collect(ctx, context);
        schedule_backup(context);
    }
}

/// `HtmlConstructorHook` entry: the native interface knows only new.target,
/// so the base interface is taken from the matching definition.
fn interface_construct(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(target) = new_target.as_object() else {
        return Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into());
    };
    let interface = registry(context)
        .state
        .borrow()
        .definitions
        .values()
        .find(|definition| JsObject::equals(&definition.constructor, &target))
        .map(|definition| definition.interface.clone());
    let Some(interface) = interface else {
        return Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into());
    };
    html_constructor(
        &JsValue::undefined(),
        &[new_target.clone(), js_str(&interface)],
        context,
    )
}

fn html_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(new_target) = args.first().and_then(JsValue::as_object) else {
        return Err(JsNativeError::typ()
            .with_message("Illegal HTML constructor")
            .into());
    };
    let interface = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let ce = registry(context);
    let found = ce
        .state
        .borrow()
        .definitions
        .iter()
        .find(|(_, definition)| JsObject::equals(&definition.constructor, &new_target))
        .map(|(name, definition)| (name.clone(), Arc::clone(definition)));
    let Some((name, definition)) = found else {
        return Err(JsNativeError::typ()
            .with_message("Unregistered HTML constructor")
            .into());
    };
    if interface != definition.interface {
        return Err(JsNativeError::typ()
            .with_message("Incorrect HTML base constructor")
            .into());
    }
    {
        let mut state = ce.state.borrow_mut();
        if let Some(frame) = state.construction.last_mut()
            && JsObject::equals(&frame.constructor, &new_target)
        {
            if frame.consumed {
                return Err(JsNativeError::typ()
                    .with_message("Element already constructed")
                    .into());
            }
            frame.consumed = true;
            frame
                .element
                .set_prototype(Some(definition.prototype.clone()));
            return Ok(frame.element.clone().into());
        }
    }
    let ctx = dom_ctx(context)?;
    let attributes = if definition.customized {
        vec![Attribute {
            name: qual_name_ns("is", ""),
            value: name.as_str().into(),
        }]
    } else {
        Vec::new()
    };
    let node = ctx
        .doc
        .borrow_mut()
        .mutate()
        .create_element(qual_name(&definition.local_name), attributes);
    ctx.doc
        .borrow_mut()
        .set_script_custom_element_state(node, State::Custom);
    let wrapper = node_wrapper(&ctx, node, context);
    wrapper.set_prototype(Some(definition.prototype.clone()));
    Ok(wrapper.into())
}

fn created_element(
    ctx: &DomCtx,
    owner: NodeId,
    name: blitz_dom::QualName,
    options: Option<&JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut attributes = Vec::new();
    if let Some(options) = options.filter(|value| !value.is_null_or_undefined()) {
        let Some(options) = options.as_object() else {
            return Err(JsNativeError::typ()
                .with_message("Options must be a dictionary")
                .into());
        };
        let is = options.get(js_string!("is"), context)?;
        if !is.is_undefined() {
            let is = to_rust_string(&is, context)?;
            attributes.push(Attribute {
                name: qual_name_ns("is", ""),
                value: is.as_str().into(),
            });
        }
    }
    let node = ctx
        .doc
        .borrow_mut()
        .mutate()
        .create_element(name, attributes);
    ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    let wrapper = node_wrapper(ctx, node, context);
    perform_upgrade(ctx, node, true, context);
    Ok(wrapper.into())
}

fn create_element(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:createElement");
    let owner = this_node_id(this)?;
    let html = ctx
        .doc
        .borrow()
        .get_node(owner)
        .is_some_and(|node| node.is_html_document());
    let tag = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let name = if html {
        qual_name(&tag.to_ascii_lowercase())
    } else {
        qual_name_ns(&tag, "")
    };
    created_element(&ctx, owner, name, args.get(1), context)
}

fn create_element_ns(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = this_node_id(this)?;
    let namespace = &args.first().cloned().unwrap_or_default();
    let namespace = if namespace.is_null_or_undefined() {
        String::new()
    } else {
        to_rust_string(namespace, context)?
    };
    let tag = to_rust_string(args.get(1).unwrap_or(&JsValue::undefined()), context)?;
    let (prefix, local) = match tag.split_once(':') {
        Some((prefix, local)) => (Some(markup5ever::Prefix::from(prefix)), local),
        None => (None, tag.as_str()),
    };
    let name = blitz_dom::QualName::new(
        prefix,
        blitz_dom::Namespace::from(namespace.as_str()),
        blitz_dom::LocalName::from(local),
    );
    created_element(&ctx, owner, name, args.get(2), context)
}

fn create_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let _t = crate::script_stats::Timed::new(&ctx, "dom:createTextNode");
    let owner = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node = ctx.doc.borrow_mut().mutate().create_text_node(&text);
    ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    Ok(node_wrapper(&ctx, node, context).into())
}

fn create_comment(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = this_node_id(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let node = ctx.doc.borrow_mut().mutate().create_comment_node(&text);
    ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    Ok(node_wrapper(&ctx, node, context).into())
}

fn create_fragment(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = this_node_id(this)?;
    let node = ctx.doc.borrow_mut().mutate().create_document_fragment();
    ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    Ok(node_wrapper(&ctx, node, context).into())
}

fn adopt_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = this_node_id(this)?;
    let value = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let node = node_id_of_value(&value)
        .ok_or_else(|| JsNativeError::typ().with_message("A DOM node is required"))?;
    let forbidden = {
        let doc = ctx.doc.borrow();
        doc.get_node(node)
            .is_none_or(|node| matches!(node.data, NodeData::Document(_) | NodeData::ShadowRoot(_)))
    };
    if forbidden {
        return Err(dom_error(
            "NotSupportedError",
            "This node cannot be adopted",
            context,
        ));
    }
    if ctx
        .doc
        .borrow()
        .get_node(node)
        .is_some_and(|node| node.parent.is_some())
    {
        super::remove_and_free_node(&ctx, node, context);
    }
    ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    Ok(value)
}

fn import_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let owner = this_node_id(this)?;
    let source = args.first().cloned().unwrap_or_else(JsValue::undefined);
    let deep = args.get(1).cloned().unwrap_or_else(JsValue::undefined);
    let cloned = super::node::clone_node(&source, &[deep], context)?;
    if let Some(node) = node_id_of_value(&cloned) {
        ctx.doc.borrow_mut().adopt_script_subtree(node, owner);
    }
    Ok(cloned)
}

//! The script runtime: owns the Boa [`Context`], registers the DOM globals and
//! dispatches events / timers into JavaScript.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::LazyLock;

use blitz_dom::NodeId;
use blitz_dom::BaseDocument;
use blitz_traits::events::{BlitzPointerId, DomEvent, DomEventData, EventState};
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::value::JsValue;
use boa_engine::{Context, JsNativeError, JsResult, JsString, NativeFunction, Source, js_string};
use boa_gc::{Finalize, Trace};
use boa_runtime::Console;
use boa_runtime::console::{ConsoleState, DefaultLogger, Logger};
use url::Url;
use web_time::{Duration, Instant};

use crate::dom::event::{EventRef, create_event, create_event_for_dom_event, set_event_path};
use crate::dom::{define_accessor, dom_ctx, node_wrapper};
use crate::state::{DomCtx, Listener};

const DIAGNOSTIC_CAPACITY: usize = 1_000;

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "debug-control"), allow(dead_code))]
pub(crate) struct DiagnosticEntry {
    pub sequence: u64,
    pub level: String,
    pub message: String,
    pub stack: String,
}

#[derive(Debug, Default)]
struct RuntimeDiagnostics {
    next_console_sequence: u64,
    next_error_sequence: u64,
    console: VecDeque<DiagnosticEntry>,
    errors: VecDeque<DiagnosticEntry>,
}

impl RuntimeDiagnostics {
    fn push_console(&mut self, level: &str, message: String) {
        self.next_console_sequence += 1;
        push_bounded(
            &mut self.console,
            DiagnosticEntry {
                sequence: self.next_console_sequence,
                level: level.into(),
                message,
                stack: String::new(),
            },
        );
    }

    fn push_error(&mut self, what: &str, error: &boa_engine::JsError) {
        self.next_error_sequence += 1;
        push_bounded(
            &mut self.errors,
            DiagnosticEntry {
                sequence: self.next_error_sequence,
                level: "error".into(),
                message: format!("Uncaught JS error in {what}: {error}"),
                stack: format!("{error:?}"),
            },
        );
    }
}

fn push_bounded(queue: &mut VecDeque<DiagnosticEntry>, entry: DiagnosticEntry) {
    if queue.len() == DIAGNOSTIC_CAPACITY {
        queue.pop_front();
    }
    queue.push_back(entry);
}

#[derive(Debug, Trace, Finalize)]
struct CapturingLogger {
    #[unsafe_ignore_trace]
    diagnostics: Rc<RefCell<RuntimeDiagnostics>>,
}

impl Logger for CapturingLogger {
    fn log(&self, msg: String, state: &ConsoleState, context: &mut Context) -> JsResult<()> {
        self.diagnostics
            .borrow_mut()
            .push_console("log", msg.clone());
        DefaultLogger.log(msg, state, context)
    }

    fn info(&self, msg: String, state: &ConsoleState, context: &mut Context) -> JsResult<()> {
        self.diagnostics
            .borrow_mut()
            .push_console("info", msg.clone());
        DefaultLogger.info(msg, state, context)
    }

    fn warn(&self, msg: String, state: &ConsoleState, context: &mut Context) -> JsResult<()> {
        self.diagnostics
            .borrow_mut()
            .push_console("warn", msg.clone());
        DefaultLogger.warn(msg, state, context)
    }

    fn error(&self, msg: String, state: &ConsoleState, context: &mut Context) -> JsResult<()> {
        self.diagnostics
            .borrow_mut()
            .push_console("error", msg.clone());
        DefaultLogger.error(msg, state, context)
    }
}

/// Print and retain an unhandled JavaScript error.
fn report_js_error(
    diagnostics: &Rc<RefCell<RuntimeDiagnostics>>,
    what: &str,
    error: &boa_engine::JsError,
) {
    diagnostics.borrow_mut().push_error(what, error);
    #[cfg(feature = "tracing")]
    tracing::error!("Uncaught JS error in {what}: {error}");
    eprintln!("Uncaught JS error in {what}: {error}");
}

fn listener_error_context(
    event_name: &str,
    target: &str,
    callback: &JsObject,
    context: &mut Context,
) -> String {
    let source = JsValue::from(callback.clone())
        .to_string(context)
        .map(|source| source.to_std_string_escaped())
        .unwrap_or_else(|_| "<source unavailable>".to_owned());
    let source = source.chars().take(500).collect::<String>();
    format!("{event_name} event listener on {target}: {source}")
}

pub(crate) struct ScriptRuntime {
    pub context: Context,
    pub ctx: DomCtx,
    diagnostics: Rc<RefCell<RuntimeDiagnostics>>,
}

impl ScriptRuntime {
    pub fn new(doc: Rc<RefCell<BaseDocument>>, base_url: Option<&Url>) -> Self {
        let mut context = Context::default();
        let ctx = DomCtx::new(doc);
        context.insert_data(ctx.clone());
        let diagnostics = Rc::new(RefCell::new(RuntimeDiagnostics::default()));

        Console::register_with_logger(
            CapturingLogger {
                diagnostics: Rc::clone(&diagnostics),
            },
            &mut context,
        )
        .expect("failed to register console");

        crate::dom::init_protos(&ctx, &mut context);

        // `document`
        let root_id = ctx.doc.borrow().root_node().id;
        let document_wrapper = node_wrapper(&ctx, root_id, &mut context);
        register_global(&mut context, "document", document_wrapper.into());

        // `window` and friends (aliases for the global object)
        let global: JsValue = context.global_object().into();
        register_global(&mut context, "window", global.clone());
        register_global(&mut context, "self", global);
        let global_object = context.global_object().clone();
        define_accessor(
            &global_object,
            "innerWidth",
            Some(window_inner_width),
            None,
            &mut context,
        );
        define_accessor(
            &global_object,
            "innerHeight",
            Some(window_inner_height),
            None,
            &mut context,
        );
        define_accessor(
            &global_object,
            "outerWidth",
            Some(window_inner_width),
            None,
            &mut context,
        );
        define_accessor(
            &global_object,
            "outerHeight",
            Some(window_inner_height),
            None,
            &mut context,
        );
        define_accessor(
            &global_object,
            "devicePixelRatio",
            Some(window_device_pixel_ratio),
            None,
            &mut context,
        );

        // Tauri and other webview embedders conventionally provide this object. It is inert
        // until the embedder installs a callback on `ScriptDocument`.
        let ipc = ObjectInitializer::new(&mut context)
            .function(
                NativeFunction::from_fn_ptr(ipc_post_message),
                js_string!("postMessage"),
                1,
            )
            .build();
        register_global(&mut context, "ipc", ipc.into());

        // `location`
        let location = build_location(base_url, &mut context);
        register_global(&mut context, "location", location);

        // `navigator`
        let navigator = ObjectInitializer::new(&mut context)
            .property(
                js_string!("userAgent"),
                js_string!("Mozilla/5.0 (compatible; Blitz)"),
                Attribute::all(),
            )
            .build();
        register_global(&mut context, "navigator", navigator.into());

        // `performance`
        let performance = ObjectInitializer::new(&mut context)
            .function(
                NativeFunction::from_fn_ptr(performance_now),
                js_string!("now"),
                0,
            )
            .build();
        register_global(&mut context, "performance", performance.into());

        // Timers and window event listeners
        register_global_fn(&mut context, "setTimeout", 2, set_timeout);
        register_global_fn(&mut context, "clearTimeout", 1, clear_timer);
        register_global_fn(&mut context, "setInterval", 2, set_interval);
        register_global_fn(&mut context, "clearInterval", 1, clear_timer);
        register_global_fn(
            &mut context,
            "requestAnimationFrame",
            1,
            request_animation_frame,
        );
        register_global_fn(&mut context, "cancelAnimationFrame", 1, clear_timer);
        register_global_fn(
            &mut context,
            "addEventListener",
            2,
            window_add_event_listener,
        );
        register_global_fn(
            &mut context,
            "removeEventListener",
            2,
            window_remove_event_listener,
        );
        register_global_fn(&mut context, "__blitzRandomU32", 0, random_u32);

        let mut runtime = Self {
            context,
            ctx,
            diagnostics,
        };

        // Small JS bootstrap for APIs that are easiest to define in JS
        runtime.eval_internal(
            r##"
            if (typeof globalThis.queueMicrotask !== "function") {
                globalThis.queueMicrotask = function (callback) {
                    Promise.resolve().then(callback);
                };
            }
            if (typeof globalThis.structuredClone !== "function") {
                globalThis.structuredClone = function (input, options) {
                    if (options && options.transfer && options.transfer.length) {
                        throw new TypeError("structuredClone transfer is not supported");
                    }

                    const seen = new Map();
                    const clone = function (value) {
                        if (value === null || typeof value !== "object") {
                            if (typeof value === "function" || typeof value === "symbol") {
                                throw new TypeError("value cannot be structured-cloned");
                            }
                            return value;
                        }
                        if (value === globalThis || typeof value.nodeType === "number") {
                            throw new TypeError("value cannot be structured-cloned");
                        }
                        if (seen.has(value)) return seen.get(value);

                        let copy;
                        if (Array.isArray(value)) {
                            copy = [];
                        } else if (value instanceof Date) {
                            return new Date(value.getTime());
                        } else if (value instanceof RegExp) {
                            return new RegExp(value.source, value.flags);
                        } else if (value instanceof Map) {
                            copy = new Map();
                            seen.set(value, copy);
                            for (const [key, entry] of value) copy.set(clone(key), clone(entry));
                            return copy;
                        } else if (value instanceof Set) {
                            copy = new Set();
                            seen.set(value, copy);
                            for (const entry of value) copy.add(clone(entry));
                            return copy;
                        } else if (typeof ArrayBuffer !== "undefined" && value instanceof ArrayBuffer) {
                            return value.slice(0);
                        } else if (typeof ArrayBuffer !== "undefined" && ArrayBuffer.isView(value)) {
                            const buffer = clone(value.buffer);
                            if (typeof DataView !== "undefined" && value instanceof DataView) {
                                return new DataView(buffer, value.byteOffset, value.byteLength);
                            }
                            return new value.constructor(buffer, value.byteOffset, value.length);
                        } else if (value instanceof WeakMap || value instanceof WeakSet || value instanceof Promise) {
                            throw new TypeError("value cannot be structured-cloned");
                        } else {
                            copy = Object.create(Object.getPrototypeOf(value) === null ? null : Object.prototype);
                        }

                        seen.set(value, copy);
                        for (const key of Object.keys(value)) copy[key] = clone(value[key]);
                        return copy;
                    };
                    return clone(input);
                };
            }
            if (typeof globalThis.crypto !== "object" || globalThis.crypto === null) {
                Object.defineProperty(globalThis, "crypto", {
                    value: {},
                    writable: false,
                    enumerable: true,
                    configurable: true,
                });
            }
            if (typeof globalThis.crypto.getRandomValues !== "function") {
                const randomU32 = globalThis.__blitzRandomU32;
                Object.defineProperty(globalThis.crypto, "getRandomValues", {
                    value: function (array) {
                        if (typeof ArrayBuffer === "undefined" || !ArrayBuffer.isView(array)) {
                            throw new TypeError("getRandomValues requires an integer TypedArray");
                        }
                        const supported = [
                            Int8Array,
                            Uint8Array,
                            Uint8ClampedArray,
                            Int16Array,
                            Uint16Array,
                            Int32Array,
                            Uint32Array,
                        ];
                        if (!supported.some((Type) => array instanceof Type)) {
                            throw new TypeError("getRandomValues requires an integer TypedArray");
                        }
                        if (array.byteLength > 65536) {
                            throw new TypeError("getRandomValues quota exceeded");
                        }
                        for (let index = 0; index < array.length; index += 1) {
                            array[index] = randomU32();
                        }
                        return array;
                    },
                    writable: true,
                    enumerable: true,
                    configurable: true,
                });
            }
            if (typeof globalThis.history !== "object" || globalThis.history === null) {
                const entries = [{ state: null, url: globalThis.location.href }];
                let index = 0;
                const applyUrl = function (url) {
                    if (url === undefined || url === null) return;
                    const value = String(url);
                    let pathname = globalThis.location.pathname;
                    let search = globalThis.location.search;
                    let hash = globalThis.location.hash;

                    if (value.startsWith("#")) {
                        hash = value;
                    } else if (value.startsWith("?")) {
                        const hashAt = value.indexOf("#");
                        search = hashAt < 0 ? value : value.slice(0, hashAt);
                        hash = hashAt < 0 ? "" : value.slice(hashAt);
                    } else {
                        let relative = value;
                        const schemeAt = relative.indexOf("://");
                        if (schemeAt >= 0) {
                            const pathAt = relative.indexOf("/", schemeAt + 3);
                            relative = pathAt < 0 ? "/" : relative.slice(pathAt);
                        }
                        if (!relative.startsWith("/")) {
                            const slashAt = pathname.lastIndexOf("/");
                            relative = pathname.slice(0, slashAt + 1) + relative;
                        }
                        const hashAt = relative.indexOf("#");
                        hash = hashAt < 0 ? "" : relative.slice(hashAt);
                        const withoutHash = hashAt < 0 ? relative : relative.slice(0, hashAt);
                        const searchAt = withoutHash.indexOf("?");
                        search = searchAt < 0 ? "" : withoutHash.slice(searchAt);
                        pathname = searchAt < 0 ? withoutHash : withoutHash.slice(0, searchAt);
                    }

                    globalThis.location.pathname = pathname || "/";
                    globalThis.location.search = search;
                    globalThis.location.hash = hash;
                    globalThis.location.href = globalThis.location.protocol + "//" +
                        globalThis.location.host + globalThis.location.pathname + search + hash;
                };
                const history = {
                    scrollRestoration: "auto",
                    get length() { return entries.length; },
                    get state() { return entries[index].state; },
                    pushState(state, _unused, url) {
                        const entry = { state: structuredClone(state), url: url ?? globalThis.location.href };
                        entries.splice(index + 1, entries.length, entry);
                        index += 1;
                        applyUrl(url);
                    },
                    replaceState(state, _unused, url) {
                        entries[index] = { state: structuredClone(state), url: url ?? entries[index].url };
                        applyUrl(url);
                    },
                    go(delta) {
                        const next = Math.max(0, Math.min(entries.length - 1, index + Number(delta || 0)));
                        if (next === index) return;
                        index = next;
                        applyUrl(entries[index].url);
                    },
                    back() { this.go(-1); },
                    forward() { this.go(1); },
                };
                Object.defineProperty(globalThis, "history", {
                    value: history,
                    writable: false,
                    enumerable: true,
                    configurable: true,
                });
            }
            const defineDomInterface = function (name, matches) {
                if (typeof globalThis[name] === "function") return;
                const Interface = function () {
                    throw new TypeError("Illegal constructor");
                };
                Object.defineProperty(Interface, "name", { value: name, configurable: true });
                Object.defineProperty(Interface, Symbol.hasInstance, {
                    value: matches,
                    configurable: true,
                });
                Object.defineProperty(globalThis, name, {
                    value: Interface,
                    writable: true,
                    enumerable: false,
                    configurable: true,
                });
            };
            const isNode = function (value) {
                return value !== null && typeof value === "object" && typeof value.nodeType === "number";
            };
            const isElement = function (value) { return isNode(value) && value.nodeType === 1; };
            const isTag = function (tagName) {
                return function (value) { return isElement(value) && value.tagName === tagName; };
            };
            defineDomInterface("Node", isNode);
            Object.defineProperties(globalThis.Node, {
                ELEMENT_NODE: { value: 1 },
                ATTRIBUTE_NODE: { value: 2 },
                TEXT_NODE: { value: 3 },
                CDATA_SECTION_NODE: { value: 4 },
                PROCESSING_INSTRUCTION_NODE: { value: 7 },
                COMMENT_NODE: { value: 8 },
                DOCUMENT_NODE: { value: 9 },
                DOCUMENT_TYPE_NODE: { value: 10 },
                DOCUMENT_FRAGMENT_NODE: { value: 11 },
                DOCUMENT_POSITION_DISCONNECTED: { value: 0x01 },
                DOCUMENT_POSITION_PRECEDING: { value: 0x02 },
                DOCUMENT_POSITION_FOLLOWING: { value: 0x04 },
                DOCUMENT_POSITION_CONTAINS: { value: 0x08 },
                DOCUMENT_POSITION_CONTAINED_BY: { value: 0x10 },
                DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: { value: 0x20 },
            });
            defineDomInterface("Element", isElement);
            defineDomInterface("HTMLElement", isElement);
            defineDomInterface("Document", function (value) { return isNode(value) && value.nodeType === 9; });
            defineDomInterface("HTMLDocument", function (value) { return isNode(value) && value.nodeType === 9; });
            defineDomInterface("HTMLHeadElement", isTag("HEAD"));
            defineDomInterface("HTMLBodyElement", isTag("BODY"));
            defineDomInterface("HTMLAnchorElement", isTag("A"));
            defineDomInterface("HTMLButtonElement", isTag("BUTTON"));
            defineDomInterface("HTMLFormElement", isTag("FORM"));
            defineDomInterface("HTMLImageElement", isTag("IMG"));
            defineDomInterface("HTMLInputElement", isTag("INPUT"));
            defineDomInterface("HTMLOptionElement", isTag("OPTION"));
            defineDomInterface("HTMLScriptElement", isTag("SCRIPT"));
            defineDomInterface("HTMLSelectElement", isTag("SELECT"));
            defineDomInterface("HTMLStyleElement", isTag("STYLE"));
            defineDomInterface("HTMLTemplateElement", isTag("TEMPLATE"));
            defineDomInterface("HTMLTextAreaElement", isTag("TEXTAREA"));
            delete globalThis.__blitzRandomU32;
            "##,
            "<blitz-bootstrap>",
        );

        runtime
    }

    /// Evaluate a script, logging (but not propagating) any uncaught errors,
    /// then drain the microtask queue.
    pub fn eval(&mut self, code: &str, description: &str) {
        self.eval_internal(code, description);
        self.run_jobs(description);
    }

    pub fn eval_json(
        &mut self,
        code: &str,
        description: &str,
    ) -> Result<serde_json::Value, String> {
        match self.context.eval(Source::from_bytes(code)) {
            Ok(value) => {
                self.run_jobs(description);
                value
                    .to_json(&mut self.context)
                    .map(|value| value.unwrap_or(serde_json::Value::Null))
                    .map_err(|error| error.to_string())
            }
            Err(error) => {
                report_js_error(&self.diagnostics, description, &error);
                Err(error.to_string())
            }
        }
    }

    #[cfg(feature = "debug-control")]
    pub fn console_entries_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        self.diagnostics
            .borrow()
            .console
            .iter()
            .filter(|entry| entry.sequence > sequence)
            .cloned()
            .collect()
    }

    #[cfg(feature = "debug-control")]
    pub fn runtime_errors_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        self.diagnostics
            .borrow()
            .errors
            .iter()
            .filter(|entry| entry.sequence > sequence)
            .cloned()
            .collect()
    }

    fn eval_internal(&mut self, code: &str, description: &str) {
        if let Err(error) = self.context.eval(Source::from_bytes(code)) {
            report_js_error(&self.diagnostics, description, &error);
        }
    }

    /// Run pending promise jobs (microtasks)
    pub fn run_jobs(&mut self, description: &str) {
        if let Err(error) = self.context.run_jobs() {
            report_js_error(&self.diagnostics, description, &error);
        }
    }

    /// The deadline of the soonest pending timer (if any)
    pub fn next_timer_deadline(&self) -> Option<Instant> {
        self.ctx.state.borrow().timers.next_deadline()
    }

    /// Run all timers that are currently due. Returns `true` if any JavaScript was run.
    pub fn run_due_timers(&mut self) -> bool {
        let timers_started = std::time::Instant::now();
        let due = self.ctx.state.borrow_mut().timers.take_due(Instant::now());
        if due.is_empty() {
            return false;
        }
        for timer in due {
            if let Err(error) =
                timer
                    .callback
                    .call(&JsValue::undefined(), &timer.args, &mut self.context)
            {
                report_js_error(&self.diagnostics, "timer callback", &error);
            }
        }
        self.run_jobs("timer microtasks");
        crate::script_stats::record_work("timers", timers_started.elapsed());
        true
    }

    /// Dispatch a Blitz DOM event to JavaScript event listeners registered on
    /// the nodes in `chain` (which is ordered target-first).
    ///
    /// Returns `true` if any listener was invoked.
    pub fn dispatch_dom_event(
        &mut self,
        chain: &[NodeId],
        event: &DomEvent,
        event_state: &mut EventState,
    ) -> bool {
        // Attributed by event name. A poll costing 16ms says nothing about what
        // to fix; "scroll cost 14ms of it" names the handler.
        let dispatch_started = std::time::Instant::now();
        let ran = self.dispatch_dom_event_timed(chain, event, event_state);
        crate::script_stats::record_work(
            &format!("event:{}", event.data.name()),
            dispatch_started.elapsed(),
        );
        ran
    }

    fn dispatch_dom_event_timed(
        &mut self,
        chain: &[NodeId],
        event: &DomEvent,
        event_state: &mut EventState,
    ) -> bool {
        let pointer_id = match &event.data {
            DomEventData::PointerMove(pointer)
            | DomEventData::PointerDown(pointer)
            | DomEventData::PointerUp(pointer)
            | DomEventData::PointerCancel(pointer) => Some(match pointer.id {
                BlitzPointerId::Mouse => 1,
                BlitzPointerId::Pen => 2,
                BlitzPointerId::Finger(id) => id.saturating_add(3),
            }),
            _ => None,
        };
        let captured_node =
            pointer_id.and_then(|id| self.ctx.state.borrow().pointer_capture.get(&id).copied());
        let captured_chain = captured_node.map(|target| {
            let doc = self.ctx.doc.borrow();
            let mut chain = Vec::new();
            let mut current = Some(target);
            while let Some(node_id) = current {
                chain.push(node_id);
                current = doc.get_node(node_id).and_then(|node| node.parent);
            }
            chain
        });
        let chain = captured_chain.as_deref().unwrap_or(chain);

        let name = event.name().to_string();
        let mut any_called = self.dispatch_event_inner(
            chain,
            &name,
            event.bubbles,
            |ctx, target, context| {
                create_event_for_dom_event(
                    ctx,
                    &event.data,
                    event.bubbles,
                    event.cancelable,
                    target,
                    context,
                )
            },
            event_state,
        );

        // Browsers fire a `change` event after `input` events on checkbox/radio
        // inputs. Blitz only generates `input` events, so synthesise the `change`
        // event here.
        if matches!(event.data, DomEventData::Input(_))
            && self.target_is_checkbox_or_radio(event.target)
        {
            let mut change_state = EventState::default();
            any_called |= self.dispatch_event_inner(
                chain,
                "change",
                true,
                |ctx, target, context| create_event(ctx, "change", true, false, target, context),
                &mut change_state,
            );
            if change_state.redraw_is_requested() {
                event_state.request_redraw();
            }
        }

        if any_called {
            self.run_jobs("event microtasks");
        }

        if matches!(
            event.data,
            DomEventData::PointerUp(_) | DomEventData::PointerCancel(_)
        ) && let Some(pointer_id) = pointer_id
        {
            self.ctx
                .state
                .borrow_mut()
                .pointer_capture
                .remove(&pointer_id);
        }

        any_called
    }

    fn target_is_checkbox_or_radio(&self, node_id: NodeId) -> bool {
        let doc = self.ctx.doc.borrow();
        doc.get_node(node_id)
            .and_then(|node| node.element_data())
            .is_some_and(|element| {
                element.name.local == blitz_dom::local_name!("input")
                    && matches!(
                        element.attr(blitz_dom::local_name!("type")),
                        Some("checkbox") | Some("radio")
                    )
            })
    }

    /// Dispatch an event named `name` along `chain`, using `make_event` to lazily
    /// construct the JS event object. Returns `true` if any listener was invoked.
    fn dispatch_event_inner(
        &mut self,
        chain: &[NodeId],
        name: &str,
        bubbles: bool,
        make_event: impl FnOnce(&DomCtx, &JsValue, &mut Context) -> JsObject,
        event_state: &mut EventState,
    ) -> bool {
        let ctx = self.ctx.clone();
        let context = &mut self.context;
        let on_name = JsString::from(format!("on{name}"));

        // Fast path: bail if no listener of this type could possibly be registered
        let may_have_listeners = {
            let state = ctx.state.borrow();
            let registry_hit = chain.iter().any(|node_id| {
                state
                    .node_listeners
                    .get(node_id)
                    .and_then(|map| map.get(name))
                    .is_some_and(|listeners| !listeners.is_empty())
            }) || state
                .window_listeners
                .get(name)
                .is_some_and(|listeners| !listeners.is_empty());
            // `on<event>` handlers can only exist on nodes that script has touched
            // (i.e. nodes with a cached wrapper)
            let wrapper_hit = chain
                .iter()
                .any(|node_id| state.node_wrappers.contains_key(node_id));
            registry_hit || wrapper_hit
        };
        if !may_have_listeners {
            return false;
        }

        let target: JsValue = node_wrapper(&ctx, chain[0], context).into();
        let event_obj = make_event(&ctx, &target, context);
        let mut event_path: Vec<JsObject> = chain
            .iter()
            .map(|&node_id| node_wrapper(&ctx, node_id, context))
            .collect();
        event_path.push(context.global_object().clone());
        set_event_path(&event_obj, event_path);
        let event_ref = |event_obj: &JsObject, f: &dyn Fn(&EventRef) -> bool| -> bool {
            event_obj
                .downcast_ref::<EventRef>()
                .map(|event| f(&event))
                .unwrap_or(false)
        };

        let mut any_called = false;

        'chain: for &node_id in chain {
            // Gather listeners for this node: `addEventListener` listeners plus
            // an `on<event>` property handler (if any)
            let mut callbacks: Vec<JsObject> = Vec::new();
            {
                let mut state = ctx.state.borrow_mut();
                if let Some(listeners) = state
                    .node_listeners
                    .get_mut(&node_id)
                    .and_then(|map| map.get_mut(name))
                {
                    callbacks.extend(listeners.iter().map(|l| l.callback.clone()));
                    // `once` listeners are removed at dispatch time
                    listeners.retain(|l| !l.once);
                }
            }
            let wrapper = ctx.state.borrow().node_wrappers.get(&node_id).cloned();
            if let Some(wrapper) = wrapper {
                if let Ok(handler) = wrapper.get(on_name.clone(), context) {
                    if let Some(handler) = handler.as_object() {
                        if handler.is_callable() {
                            callbacks.push(handler);
                        }
                    }
                }
            }

            if callbacks.is_empty() {
                if !bubbles {
                    break;
                }
                continue;
            }

            let current_target: JsValue = node_wrapper(&ctx, node_id, context).into();
            crate::dom::define_value(&event_obj, "currentTarget", current_target.clone(), context);

            for callback in callbacks {
                any_called = true;
                if let Err(error) =
                    callback.call(&current_target, &[event_obj.clone().into()], context)
                {
                    let what = listener_error_context(
                        name,
                        &format!("node {node_id}"),
                        &callback,
                        context,
                    );
                    report_js_error(&self.diagnostics, &what, &error);
                }
                if event_ref(&event_obj, &|event| event.stopped_immediate.get()) {
                    break 'chain;
                }
            }

            if !bubbles || event_ref(&event_obj, &|event| event.stopped.get()) {
                break;
            }
        }

        // Window-level listeners
        if bubbles && !event_ref(&event_obj, &|event| event.stopped.get()) {
            let listeners: Vec<Listener> = {
                let mut state = ctx.state.borrow_mut();
                match state.window_listeners.get_mut(name) {
                    Some(listeners) => {
                        let cloned = listeners.clone();
                        listeners.retain(|l| !l.once);
                        cloned
                    }
                    None => Vec::new(),
                }
            };
            if !listeners.is_empty() {
                let global: JsValue = context.global_object().into();
                crate::dom::define_value(&event_obj, "currentTarget", global.clone(), context);
                for listener in listeners {
                    any_called = true;
                    if let Err(error) =
                        listener
                            .callback
                            .call(&global, &[event_obj.clone().into()], context)
                    {
                        let what =
                            listener_error_context(name, "window", &listener.callback, context);
                        report_js_error(&self.diagnostics, &what, &error);
                    }
                    if event_ref(&event_obj, &|event| event.stopped_immediate.get()) {
                        break;
                    }
                }
            }
        }

        crate::dom::define_value(&event_obj, "currentTarget", JsValue::null(), context);

        // Feed `preventDefault` / `stopPropagation` back into Blitz
        if event_ref(&event_obj, &|event| event.prevented.get()) {
            event_state.prevent_default();
        }
        if event_ref(&event_obj, &|event| event.stopped.get()) {
            event_state.stop_propagation();
        }
        if any_called {
            event_state.request_redraw();
        }

        any_called
    }

    /// Dispatch a simple event (e.g. `DOMContentLoaded`) targeting the document node
    pub fn dispatch_document_event(&mut self, name: &str) -> bool {
        let root_id = self.ctx.doc.borrow().root_node().id;
        let mut event_state = EventState::default();
        let ran = self.dispatch_event_inner(
            &[root_id],
            name,
            true,
            |ctx, target, context| create_event(ctx, name, true, false, target, context),
            &mut event_state,
        );
        if ran {
            self.run_jobs("event microtasks");
        }
        ran
    }

    /// Dispatch a simple event (e.g. `load`) targeting the window
    pub fn dispatch_window_event(&mut self, name: &str) -> bool {
        let ctx = self.ctx.clone();
        let context = &mut self.context;

        let listeners: Vec<Listener> = {
            let mut state = ctx.state.borrow_mut();
            match state.window_listeners.get_mut(name) {
                Some(listeners) => {
                    let cloned = listeners.clone();
                    listeners.retain(|l| !l.once);
                    cloned
                }
                None => Vec::new(),
            }
        };

        let global: JsValue = context.global_object().into();
        let event_obj = create_event(&ctx, name, false, false, &global, context);
        crate::dom::define_value(&event_obj, "currentTarget", global.clone(), context);

        let mut any_called = false;
        for listener in listeners {
            any_called = true;
            if let Err(error) =
                listener
                    .callback
                    .call(&global, &[event_obj.clone().into()], context)
            {
                let what = listener_error_context(name, "window", &listener.callback, context);
                report_js_error(&self.diagnostics, &what, &error);
            }
        }

        // `window.onload = ...` style handler
        let on_name = JsString::from(format!("on{name}"));
        if let Ok(handler) = context.global_object().get(on_name, context) {
            if let Some(handler) = handler.as_object() {
                if handler.is_callable() {
                    any_called = true;
                    if let Err(error) = handler.call(&global, &[event_obj.into()], context) {
                        let what = listener_error_context(name, "window.on*", &handler, context);
                        report_js_error(&self.diagnostics, &what, &error);
                    }
                }
            }
        }

        if any_called {
            self.run_jobs("event microtasks");
        }
        any_called
    }
}

fn ipc_post_message(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let body = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_lossy();
    let handler = dom_ctx(context)?.state.borrow().ipc_handler.clone();
    if let Some(handler) = handler {
        handler(body);
    }
    Ok(JsValue::undefined())
}

/// The spec's time origin is document creation. A first-use instant is close
/// enough for the elapsed-time measurements callers actually take, and sharing
/// one origin across documents keeps their readings comparable.
static TIME_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

fn performance_now(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::new(TIME_ORIGIN.elapsed().as_secs_f64() * 1000.0))
}

fn random_u32(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    getrandom::u32().map(JsValue::new).map_err(|error| {
        JsNativeError::error()
            .with_message(format!("secure random generation failed: {error}"))
            .into()
    })
}

fn register_global(context: &mut Context, name: &str, value: JsValue) {
    context
        .register_global_property(
            JsString::from(name),
            value,
            Attribute::WRITABLE.union(Attribute::CONFIGURABLE),
        )
        .expect("failed to register global");
}

fn register_global_fn(
    context: &mut Context,
    name: &str,
    length: usize,
    body: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
) {
    context
        .register_global_callable(
            JsString::from(name),
            length,
            NativeFunction::from_fn_ptr(body),
        )
        .expect("failed to register global function");
}

fn build_location(base_url: Option<&Url>, context: &mut Context) -> JsValue {
    let (href, protocol, host, pathname, search, hash) = match base_url {
        Some(url) => (
            url.to_string(),
            format!("{}:", url.scheme()),
            url.host_str().unwrap_or_default().to_string(),
            url.path().to_string(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default(),
            url.fragment().map(|f| format!("#{f}")).unwrap_or_default(),
        ),
        None => (
            "about:blank".to_string(),
            "about:".to_string(),
            String::new(),
            "blank".to_string(),
            String::new(),
            String::new(),
        ),
    };
    ObjectInitializer::new(context)
        .property(js_string!("href"), JsString::from(href), Attribute::all())
        .property(
            js_string!("protocol"),
            JsString::from(protocol),
            Attribute::all(),
        )
        .property(
            js_string!("host"),
            JsString::from(host.clone()),
            Attribute::all(),
        )
        .property(
            js_string!("hostname"),
            JsString::from(host),
            Attribute::all(),
        )
        .property(
            js_string!("pathname"),
            JsString::from(pathname),
            Attribute::all(),
        )
        .property(
            js_string!("search"),
            JsString::from(search),
            Attribute::all(),
        )
        .property(js_string!("hash"), JsString::from(hash), Attribute::all())
        .build()
        .into()
}

fn window_inner_width(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let viewport = ctx.doc.borrow().get_viewport();
    Ok(JsValue::from(
        viewport.window_size.0 as f64 / viewport.scale_f64(),
    ))
}

fn window_inner_height(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let viewport = ctx.doc.borrow().get_viewport();
    Ok(JsValue::from(
        viewport.window_size.1 as f64 / viewport.scale_f64(),
    ))
}

fn window_device_pixel_ratio(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    Ok(JsValue::from(ctx.doc.borrow().get_viewport().scale_f64()))
}

// === Timer + window listener native functions ===

fn timer_args(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<Option<(JsObject, Duration, Vec<JsValue>)>> {
    let Some(callback) = args
        .first()
        .and_then(|value| value.as_object())
        .filter(|obj| obj.is_callable())
    else {
        return Ok(None);
    };
    let delay_ms = match args.get(1) {
        Some(value) => value.to_number(context)?,
        None => 0.0,
    };
    let delay_ms = if delay_ms.is_finite() && delay_ms > 0.0 {
        delay_ms
    } else {
        0.0
    };
    let rest: Vec<JsValue> = args.iter().skip(2).cloned().collect();
    Ok(Some((
        callback,
        Duration::from_secs_f64(delay_ms / 1000.0),
        rest,
    )))
}

fn set_timeout(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let Some((callback, delay, rest)) = timer_args(args, context)? else {
        return Ok(JsValue::from(0));
    };
    let id = ctx
        .state
        .borrow_mut()
        .timers
        .add(delay, None, callback, rest);
    Ok(JsValue::from(id as f64))
}

fn set_interval(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let Some((callback, delay, rest)) = timer_args(args, context)? else {
        return Ok(JsValue::from(0));
    };
    let id = ctx
        .state
        .borrow_mut()
        .timers
        .add(delay, Some(delay), callback, rest);
    Ok(JsValue::from(id as f64))
}

fn request_animation_frame(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let Some(callback) = args
        .first()
        .and_then(|value| value.as_object())
        .filter(|obj| obj.is_callable())
    else {
        return Ok(JsValue::from(0));
    };
    // Approximate the next frame as ~16ms away
    let timestamp = JsValue::from(16.0);
    let id = ctx.state.borrow_mut().timers.add(
        Duration::from_millis(16),
        None,
        callback,
        vec![timestamp],
    );
    Ok(JsValue::from(id as f64))
}

fn clear_timer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = match args.first() {
        Some(value) => value.to_number(context)?,
        None => return Ok(JsValue::undefined()),
    };
    if id.is_finite() && id >= 0.0 {
        ctx.state.borrow_mut().timers.remove(id as u64);
    }
    Ok(JsValue::undefined())
}

fn window_add_event_listener(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let event_type =
        crate::dom::to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(callback) = args
        .get(1)
        .and_then(|value| value.as_object())
        .filter(|obj| obj.is_callable())
    else {
        return Ok(JsValue::undefined());
    };

    let mut state = ctx.state.borrow_mut();
    let listeners = state.window_listeners.entry(event_type).or_default();
    if !listeners
        .iter()
        .any(|l| JsObject::equals(&l.callback, &callback))
    {
        listeners.push(Listener {
            callback,
            capture: false,
            once: false,
        });
    }
    Ok(JsValue::undefined())
}

fn window_remove_event_listener(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let event_type =
        crate::dom::to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let Some(callback) = args.get(1).and_then(|value| value.as_object()) else {
        return Ok(JsValue::undefined());
    };

    let mut state = ctx.state.borrow_mut();
    if let Some(listeners) = state.window_listeners.get_mut(&event_type) {
        listeners.retain(|l| !JsObject::equals(&l.callback, &callback));
    }
    Ok(JsValue::undefined())
}

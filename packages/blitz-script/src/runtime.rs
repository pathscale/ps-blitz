//! The script runtime: owns the Boa [`Context`], registers the DOM globals and
//! dispatches events / timers into JavaScript.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

use blitz_dom::BaseDocument;
use blitz_dom::NodeId;
use blitz_traits::events::{BlitzPointerId, DomEvent, DomEventData, EventState};
use boa_engine::builtins::promise::PromiseState;
use boa_engine::object::builtins::JsPromise;
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::value::JsValue;
use boa_engine::{
    Context, JsError, JsNativeError, JsResult, JsString, Module, NativeFunction, Source, js_string,
};
use boa_gc::{Finalize, Trace};
use boa_runtime::Console;
use boa_runtime::console::{ConsoleState, DefaultLogger, Logger};
use url::Url;
use web_time::{Duration, Instant};

use crate::dom::event::{EventRef, create_event, create_event_for_dom_event, set_event_path};
use crate::dom::{define_accessor, dom_ctx, node_wrapper};
use crate::module::{BlitzModuleLoader, SharedFetcher};
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
    module_loader: Rc<BlitzModuleLoader>,
    /// Module evaluations that had not settled when their script ran.
    ///
    /// Top-level `await` makes this ordinary rather than exceptional: a module
    /// that opens with `await fetch(...)` is pending until a timer or a network
    /// completion resolves it, which happens on a later poll. Treating that as
    /// a failure at evaluation time would report an error against every module
    /// on the modern web that waits for anything.
    pending_modules: Vec<(JsPromise, String)>,
}

impl ScriptRuntime {
    pub fn new(
        doc: Rc<RefCell<BaseDocument>>,
        base_url: Option<&Url>,
        fetcher: SharedFetcher,
    ) -> Self {
        // A module loader can only be given to a context at construction, so
        // this cannot be `Context::default()` any more. The default is not
        // merely "no loader" either: it is a filesystem loader rooted at the
        // process's working directory, which for a browser is both useless and
        // the wrong thing to expose to a page.
        let module_loader = Rc::new(BlitzModuleLoader::new(fetcher, base_url.cloned()));
        let mut context = Context::builder()
            .module_loader(Rc::clone(&module_loader))
            .build()
            .expect("building the script context should not fail");
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

        // On the global rather than only on `window`: pages call it bare as
        // often as they qualify it.
        let computed_style =
            NativeFunction::from_fn_ptr(get_computed_style).to_js_function(context.realm());
        register_global(&mut context, "getComputedStyle", computed_style.into());

        // `navigator`, including the async clipboard.
        //
        // Without `navigator.clipboard` every "copy" button in an embedding app
        // fails silently: the usual shape is `navigator.clipboard.writeText(t)`
        // inside a try/catch that falls back to `document.execCommand("copy")`,
        // and neither existed here, so the catch swallowed a TypeError and the
        // fallback returned nothing. The shell already speaks to the system
        // clipboard for text selection; this is the same channel.
        let clipboard = ObjectInitializer::new(&mut context)
            .function(
                NativeFunction::from_fn_ptr(clipboard_write_text),
                js_string!("writeText"),
                1,
            )
            .function(
                NativeFunction::from_fn_ptr(clipboard_read_text),
                js_string!("readText"),
                0,
            )
            .build();
        let navigator = ObjectInitializer::new(&mut context)
            .property(
                js_string!("userAgent"),
                js_string!("Mozilla/5.0 (compatible; Blitz)"),
                Attribute::all(),
            )
            .property(js_string!("clipboard"), clipboard, Attribute::all())
            .build();
        register_global(&mut context, "navigator", navigator.into());

        // `customElements`
        //
        // Absent entirely before this, so `customElements.define(...)` threw a
        // ReferenceError out of whatever module ran it. A framework that
        // registers its components at import time loses that whole module, and
        // the page renders as unstyled markup or not at all.
        let custom_elements = ObjectInitializer::new(&mut context)
            .function(
                NativeFunction::from_fn_ptr(crate::dom::custom_elements::define),
                js_string!("define"),
                2,
            )
            .function(
                NativeFunction::from_fn_ptr(crate::dom::custom_elements::get),
                js_string!("get"),
                1,
            )
            .function(
                NativeFunction::from_fn_ptr(crate::dom::custom_elements::get_name),
                js_string!("getName"),
                1,
            )
            .build();
        register_global(&mut context, "customElements", custom_elements.into());

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
            module_loader,
            pending_modules: Vec::new(),
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
        self.eval_at(code, None, description);
    }

    /// Evaluate a classic script that has a URL of its own.
    ///
    /// The URL is not decoration: a classic script may still call dynamic
    /// `import()`, and the specifier in that call resolves against the script,
    /// not the document. Without it, `import("./chunk.js")` from a bundle
    /// served out of `/assets/` would look for the chunk beside the HTML.
    pub fn eval_at(&mut self, code: &str, url: Option<&Url>, description: &str) {
        let path = url.map(|url| url.as_str().to_owned());
        let source = Source::from_bytes(code.as_bytes());
        let source = match &path {
            Some(path) => source.with_path(Path::new(path)),
            None => source,
        };

        if let Err(error) = self.context.eval(source) {
            report_js_error(&self.diagnostics, description, &error);
        }
        self.run_jobs(description);
    }

    /// Install the page's `<script type="importmap">`.
    ///
    /// Bare specifiers (`import "preact"`) have no meaning without one, so a
    /// page that ships unbundled modules stops at its first dependency until
    /// this is set.
    pub fn set_import_map(&self, json: &str, base_url: Option<&Url>) {
        self.module_loader
            .set_import_map(crate::module::ImportMap::parse(json, base_url));
    }

    /// Evaluate a `<script type="module">`.
    ///
    /// `url` is the module's own URL: the resolved `src` for an external
    /// module, or the document's URL for an inline one. It is what relative
    /// imports and `import.meta.url` resolve against, so a module without one
    /// can still run but cannot import anything relative.
    pub fn eval_module(&mut self, code: &str, url: Option<&Url>, description: &str) {
        let path = url.map(|url| url.as_str().to_owned());
        let source = Source::from_bytes(code.as_bytes());
        let source = match &path {
            Some(path) => source.with_path(Path::new(path)),
            None => source,
        };

        let module = match Module::parse(source, None, &mut self.context) {
            Ok(module) => module,
            Err(error) => {
                report_js_error(&self.diagnostics, description, &error);
                return;
            }
        };

        // Registered under its own URL before it runs, so that a module which
        // is both loaded by a `<script src>` and imported by a sibling is one
        // module with one set of bindings, not two.
        if let Some(url) = url {
            self.module_loader.register(url, module.clone());
        }

        // Loading, linking and evaluating are all asynchronous, and the
        // returned promise only settles once the job queue has been drained.
        // Draining it here keeps a module script as synchronous from the
        // document's point of view as the classic script it replaces.
        let promise = module.load_link_evaluate(&mut self.context);
        self.run_jobs(description);

        match promise.state() {
            PromiseState::Fulfilled(_) => {}
            PromiseState::Rejected(error) => {
                report_js_error(&self.diagnostics, description, &JsError::from_opaque(error));
            }
            // Not a failure. A module with top-level `await` is pending until
            // whatever it waits on completes, and that happens on a later poll.
            // Kept so that when it does reject, the rejection is still
            // attributed to the module rather than vanishing.
            PromiseState::Pending => self.pending_modules.push((promise, description.to_owned())),
        }
    }

    /// Report module evaluations that have since failed, and forget those that
    /// have since succeeded.
    ///
    /// Without this, a module whose top-level `await` eventually rejects fails
    /// silently: the page half-mounts and there is nothing anywhere saying why.
    pub fn poll_module_evaluations(&mut self) {
        if self.pending_modules.is_empty() {
            return;
        }

        let mut still_pending = Vec::new();
        for (promise, description) in std::mem::take(&mut self.pending_modules) {
            match promise.state() {
                PromiseState::Pending => still_pending.push((promise, description)),
                PromiseState::Fulfilled(_) => {}
                PromiseState::Rejected(error) => {
                    report_js_error(
                        &self.diagnostics,
                        &description,
                        &JsError::from_opaque(error),
                    );
                }
            }
        }
        self.pending_modules = still_pending;
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

    /// Free detached nodes whose wrappers script no longer holds.
    ///
    /// Removal cannot judge this at the time: the wrapper is alive at the
    /// instant of the call that removed the node, because that call went
    /// through it. Deciding later, once the collector has had its say, is what
    /// stops a document growing by one abandoned subtree per removed row.
    pub fn sweep_detached_nodes(&mut self) {
        crate::dom::sweep_detached_nodes(&self.ctx);
    }

    /// Run all timers that are currently due. Returns `true` if any JavaScript was run.
    pub fn run_due_timers(&mut self, profiling: bool) -> bool {
        let timers_started = profiling.then(std::time::Instant::now);
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
        if let Some(started) = timers_started {
            crate::script_stats::record_work("timers", started.elapsed());
        }
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
        profiling: bool,
    ) -> bool {
        // Attributed by event name. A poll costing 16ms says nothing about what
        // to fix; "scroll cost 14ms of it" names the handler.
        let dispatch_started = profiling.then(std::time::Instant::now);
        let ran = self.dispatch_dom_event_timed(chain, event, event_state);
        if let Some(started) = dispatch_started {
            crate::script_stats::record_work(
                &format!("event:{}", event.data.name()),
                started.elapsed(),
            );
        }
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
                    callbacks.extend(
                        listeners
                            .iter()
                            .filter_map(|listener| listener.callback.upgrade()),
                    );
                    // `once` listeners are removed at dispatch time
                    listeners.retain(|l| !l.once);
                }
            }
            crate::dom::node::sync_node_listener_callbacks(&ctx, node_id, context);
            // Upgraded: the cache is weak, and a wrapper the collector has
            // taken cannot be carrying an `on<event>` handler, because holding
            // one would have kept it alive.
            let wrapper = ctx
                .state
                .borrow()
                .node_wrappers
                .get(&node_id)
                .and_then(boa_engine::object::WeakJsObject::upgrade);
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

    /// Dispatch a simple, non-bubbling event at one node.
    ///
    /// For the resource events a script element reports on itself, `load` and
    /// `error`. Non-bubbling because that is what the DOM specifies for them,
    /// and a loader listening on the element would otherwise also hear every
    /// image on the page finish.
    pub fn dispatch_node_event(&mut self, node_id: NodeId, name: &str) -> bool {
        let mut event_state = EventState::default();
        let ran = self.dispatch_event_inner(
            &[node_id],
            name,
            false,
            |ctx, target, context| create_event(ctx, name, false, false, target, context),
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

/// `getComputedStyle(element)`.
///
/// Backed by the engine's own computed values rather than a stub returning
/// empty strings. That distinction matters: a page reading `display` off a stub
/// gets `""`, believes the element is not hidden, and lays itself out wrongly —
/// harder to diagnose than the `getComputedStyle is not defined` this replaces,
/// which at least stopped loudly. Three sites in a hundred-site corpus died on
/// that error.
///
/// The returned object carries a fixed set of properties as own keys and a
/// `getPropertyValue` that reads them. A property outside the set returns the
/// empty string, which is what a real browser returns for one it does not
/// recognise, so a caller cannot tell an unsupported property from an unset
/// one. That is the honest limit of this: it answers well for what it covers
/// and says nothing for the rest.
fn get_computed_style(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(node_id) = args.first().and_then(crate::dom::node_id_of_value) else {
        return Err(JsNativeError::typ()
            .with_message("getComputedStyle expects an element")
            .into());
    };

    let properties = {
        let ctx = dom_ctx(context)?;
        let doc = ctx.doc.borrow();
        doc.get_node(node_id)
            .and_then(|node| node.computed_style_properties())
            .unwrap_or_default()
    };

    let mut declaration = ObjectInitializer::new(context);
    for (name, value) in &properties {
        // Both spellings, because scripts read `style.fontSize` as often as
        // they call `getPropertyValue("font-size")`.
        let camel: String = {
            let mut out = String::with_capacity(name.len());
            let mut upper = false;
            for ch in name.chars() {
                if ch == '-' {
                    upper = true;
                } else if upper {
                    out.extend(ch.to_uppercase());
                    upper = false;
                } else {
                    out.push(ch);
                }
            }
            out
        };
        declaration.property(
            js_string!(*name),
            JsValue::from(js_string!(value.as_str())),
            Attribute::all(),
        );
        if camel != *name {
            declaration.property(
                js_string!(camel.as_str()),
                JsValue::from(js_string!(value.as_str())),
                Attribute::all(),
            );
        }
    }
    declaration.function(
        NativeFunction::from_fn_ptr(get_property_value),
        js_string!("getPropertyValue"),
        1,
    );
    Ok(declaration.build().into())
}

/// `CSSStyleDeclaration.getPropertyValue(name)` over the object built above.
fn get_property_value(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?;
    let Some(object) = this.as_object() else {
        return Ok(JsValue::from(js_string!("")));
    };
    match object.get(name, context) {
        Ok(value) if !value.is_undefined() => Ok(value),
        // A property this does not carry reads as unset, which is what a real
        // browser answers for one it does not recognise.
        _ => Ok(JsValue::from(js_string!(""))),
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

/// `navigator.clipboard.writeText(text)`.
///
/// The spec returns a promise, and callers await it, so a plain value would be
/// awaited as an already-resolved one and hide a failed write. Resolve on a
/// successful shell write and reject otherwise, which is what the API's own
/// error path looks like.
fn clipboard_write_text(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_lossy();
    let wrote = dom_ctx(context)?
        .doc
        .borrow()
        .shell_provider
        .set_clipboard_text(text)
        .is_ok();
    let promise = if wrote {
        JsPromise::resolve(JsValue::undefined(), context)
    } else {
        JsPromise::reject(
            JsNativeError::error().with_message("the clipboard is unavailable"),
            context,
        )
    };
    Ok(JsValue::from(promise?))
}

/// `navigator.clipboard.readText()`. Rejects when the shell has no clipboard,
/// rather than resolving to "" — an empty clipboard and an absent one are
/// different answers and callers branch on them differently.
fn clipboard_read_text(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let read = dom_ctx(context)?
        .doc
        .borrow()
        .shell_provider
        .get_clipboard_text();
    let promise = match read {
        Ok(text) => JsPromise::resolve(JsValue::from(JsString::from(text)), context),
        Err(_) => JsPromise::reject(
            JsNativeError::error().with_message("the clipboard is unavailable"),
            context,
        ),
    };
    Ok(JsValue::from(promise?))
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

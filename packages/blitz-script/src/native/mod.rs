//! Bounded Rust script guest operating directly on a borrowed Blitz document.
//!
//! Source is parsed into retained syntax trees. Embedder resources cross the
//! typed bridge; no source strings are synthesized for host operations.

use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Waker;

use crate::DiagnosticEntry;
use crate::native_host::NativeIntrinsic;
use blitz_dom::{BaseDocument, NodeId};

mod builtins;
mod dom;
mod events;
mod exec;
mod proxy;
mod regexp;
mod runtime;
pub mod syntax;
mod value;
mod web;
pub use runtime::NativeStorageBridge;
pub use value::{Env, Environment, Error, Function, Object, Property, Result, Value};

/// A rooted, already parsed callback ready for execution on its owning realm.
#[derive(Clone)]
pub struct HostJob {
    pub callback: Value,
    pub this: Value,
    pub args: Vec<Value>,
}

/// Embedder-owned resources used by one guest and its live document.
///
/// Returning `None` leaves an operation to the standard guest bindings. The
/// embedder queues callbacks instead of re-entering an already borrowed guest.
pub trait RuntimeBridge: 'static {
    fn install(&mut self, _global: &Value) -> Result<()> {
        Ok(())
    }
    fn get(
        &mut self,
        _document: &mut BaseDocument,
        _receiver: &Value,
        _key: &str,
    ) -> Result<Option<Value>> {
        Ok(None)
    }
    fn set(
        &mut self,
        _document: &mut BaseDocument,
        _receiver: &Value,
        _key: &str,
        _value: Value,
    ) -> Result<bool> {
        Ok(false)
    }
    fn call(
        &mut self,
        _document: &mut BaseDocument,
        _name: &str,
        _this: Value,
        _arguments: Vec<Value>,
        _construct: bool,
    ) -> Result<Option<Value>> {
        Ok(None)
    }
    fn poll(
        &mut self,
        _document: &mut BaseDocument,
        _waker: Option<&Waker>,
        _run_timers: bool,
    ) -> Result<Vec<HostJob>> {
        Ok(Vec::new())
    }
    fn has_pending_jobs(&self) -> bool {
        false
    }
    fn pending_task_count(&self) -> u64 {
        u64::from(self.has_pending_jobs())
    }
    fn wait_for_work(&mut self) -> Result<()> {
        Ok(())
    }
    fn teardown(&mut self) {}
}

/// A persistent realm with bounded script entry and typed embedder bindings.
pub struct Guest<B: RuntimeBridge> {
    heap: Rc<std::cell::RefCell<value::RealmHeap>>,
    pub bridge: B,
    pub global: Value,
    pub env: Env,
    pub nodes: BTreeMap<NodeId, Value>,
    pub jobs: VecDeque<HostJob>,
    pub callbacks: BTreeMap<u64, Value>,
    pub next_callback: u64,
    pub scope_id: u64,
    pub generation: u64,
    pub steps: usize,
    await_resumptions: usize,
    pub depth: usize,
    call_stack: Vec<String>,
    pub disposed: bool,
    pub visible: bool,
    pub frozen: bool,
    pub ready_state: String,
    pub console: Vec<DiagnosticEntry>,
    pub errors: Vec<DiagnosticEntry>,
    pub ipc_handler: Option<Box<dyn Fn(String)>>,
    pub intrinsics: BTreeMap<String, NativeIntrinsic<Self>>,
}

/// A statically typed owner used by the document runtime driver.
pub struct GuestHost<B: RuntimeBridge>(pub Guest<B>);

static NEXT_REALM: AtomicU64 = AtomicU64::new(1);
impl<B: RuntimeBridge> Guest<B> {
    pub fn new(bridge: B) -> Self {
        let scope_id = NEXT_REALM.fetch_add(1, Ordering::Relaxed);
        let heap = Rc::new(std::cell::RefCell::new(value::RealmHeap::default()));
        heap.borrow_mut().realm = scope_id;
        let _scope = value::HeapScope::enter(&heap);
        let global = Value::object("Global");
        let mut guest = Self {
            heap,
            bridge,
            global: global.clone(),
            env: Environment::new(None),
            nodes: BTreeMap::new(),
            jobs: VecDeque::new(),
            callbacks: BTreeMap::new(),
            next_callback: 1,
            scope_id,
            generation: 1,
            steps: 0,
            await_resumptions: 0,
            depth: 0,
            call_stack: Vec::new(),
            disposed: false,
            visible: true,
            frozen: false,
            ready_state: "loading".into(),
            console: Vec::new(),
            errors: Vec::new(),
            ipc_handler: None,
            intrinsics: BTreeMap::new(),
        };
        for name in ["globalThis", "window", "self", "top", "parent"] {
            global.put(name, global.clone());
        }
        global.put("frameElement", Value::Null);
        global.put("undefined", Value::Undefined);
        global.put("NaN", Value::Number(f64::NAN));
        global.put("Infinity", Value::Number(f64::INFINITY));
        guest.install_builtins();
        guest.install_web();
        guest.install_dom();
        guest.install_events();
        guest.global.put("RegExp", Value::native("RegExp"));
        guest.global.put("WeakMap", Value::native("WeakMap"));
        guest.global.put("WeakSet", Value::native("WeakSet"));
        guest
            .global
            .put("queueMicrotask", Value::native("Guest.queueMicrotask"));
        let console = Value::object("Console");
        for method in ["log", "info", "warn", "error", "debug"] {
            console.put(method, Value::native(format!("Console.{method}")));
        }
        guest.global.put("console", console);
        guest
    }
    pub(super) fn begin_execution(&mut self) {
        self.steps = 0;
        self.await_resumptions = 0;
    }
    pub(super) fn resume_after_await(&mut self) -> Result<()> {
        // A settled await resumes a new turn, bounded separately from the
        // synchronous instruction limit so repeated awaits cannot renew it forever.
        if self.await_resumptions >= 1_024 {
            return Err(Error::new(
                "RangeError",
                "await resumption budget exhausted",
            ));
        }
        self.await_resumptions += 1;
        self.steps = 0;
        Ok(())
    }
    pub fn tick(&mut self) -> Result<()> {
        if self.heap.borrow_mut().exhausted() {
            return Err(Error::new(
                "QuotaExceededError",
                "realm allocation limit exceeded",
            ));
        }
        if self.disposed {
            return Err(Error::new(
                "InvalidStateError",
                "script realm has been retired",
            ));
        }
        self.steps = self.steps.saturating_add(1);
        if self.steps > 2_000_000 {
            return Err(Error::new(
                "RangeError",
                "script instruction budget exhausted",
            ));
        }
        Ok(())
    }
    pub fn node(&mut self, id: NodeId) -> Value {
        self.nodes
            .entry(id)
            .or_insert_with(|| {
                let node = Value::object("Node");
                if let Some(object) = node.as_object() {
                    let mut object = object.borrow_mut();
                    object.payload = Some(Rc::new(id));
                    object.props.insert(
                        "__node".into(),
                        Property {
                            value: Value::string(id.as_u64().to_string()),
                            get: None,
                            set: None,
                            enumerable: false,
                        },
                    );
                }
                node
            })
            .clone()
    }
    pub fn queue(&mut self, callback: Value, this: Value, args: Vec<Value>) {
        self.jobs.push_back(HostJob {
            callback,
            this,
            args,
        });
    }
    pub fn get_property(
        &mut self,
        document: &mut BaseDocument,
        receiver: &Value,
        key: &str,
    ) -> Result<Value> {
        self.tick()?;
        if let Some(value) = self.proxy_get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(object) = receiver.as_object() {
            let property = object.borrow().props.get(key).cloned();
            if let Some(property) = property {
                if let Some(get) = property.get {
                    return self.call(document, get, receiver.clone(), Vec::new(), false);
                }
                return Ok(property.value);
            }
        }
        if key.parse::<usize>().is_ok() {
            match receiver {
                Value::String(units) => {
                    let index = key.parse::<usize>().unwrap_or(usize::MAX);
                    return Ok(units
                        .get(index)
                        .map_or(Value::Undefined, |unit| Value::String(Rc::from([*unit]))));
                }
                Value::Object(object)
                    if !object.borrow().indexed.is_empty()
                        || matches!(object.borrow().kind.as_str(), "Array" | "NodeList") =>
                {
                    return Ok(receiver.get(key));
                }
                _ => {}
            }
        }
        if let Some(value) = self.bridge.get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(value) = self.dom_get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(value) = self.web_get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(value) = self.events_get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(value) = self.regexp_get(receiver, key) {
            return Ok(value);
        }
        if let Some(value) = self.weak_get(receiver, key) {
            return Ok(value);
        }
        if let Some(value) = self.builtin_get(document, receiver, key)? {
            return Ok(value);
        }
        if let Some(object) = receiver.as_object() {
            let mut prototype = object.borrow().prototype.clone();
            let mut remaining = 256usize;
            while let Some(value) = prototype {
                if remaining == 0 {
                    return Err(Error::new("RangeError", "prototype chain limit exceeded"));
                }
                remaining -= 1;
                let Some(object) = value.as_object() else {
                    break;
                };
                let property = object.borrow().props.get(key).cloned();
                if let Some(property) = property {
                    return if let Some(get) = property.get {
                        self.call(document, get, receiver.clone(), Vec::new(), false)
                    } else {
                        Ok(property.value)
                    };
                }
                prototype = object.borrow().prototype.clone();
            }
        }
        if matches!(receiver, Value::Null | Value::Undefined) {
            return Err(Error::type_error(format!(
                "cannot read {key} of {}",
                receiver.text()
            )));
        }
        Ok(Value::Undefined)
    }
    pub fn set_property(
        &mut self,
        document: &mut BaseDocument,
        receiver: &Value,
        key: &str,
        value: Value,
    ) -> Result<()> {
        self.tick()?;
        if receiver.kind() == "Proxy" {
            if !self.proxy_set(document, receiver, key, value)? {
                return Err(Error::type_error("Proxy set trap rejected assignment"));
            }
            return Ok(());
        }
        if let Some(object) = receiver.as_object() {
            let property = object.borrow().props.get(key).cloned();
            if let Some(property) = property {
                if let Some(set) = property.set {
                    self.call(document, set, receiver.clone(), vec![value], false)?;
                    return Ok(());
                }
                if property.get.is_some() {
                    return Err(Error::type_error("property has no setter"));
                }
            }
        }
        if self.bridge.set(document, receiver, key, value.clone())?
            || self.dom_set(document, receiver, key, value.clone())?
            || self.web_set(document, receiver, key, value.clone())?
            || self.events_set(document, receiver, key, value.clone())?
        {
            return Ok(());
        }
        if let Some(object) = receiver.as_object() {
            let prototype = object.borrow().prototype.clone();
            if let Some(prototype) = prototype {
                if let Some(object) = prototype.as_object() {
                    let setter = object.borrow().props.get(key).and_then(|p| p.set.clone());
                    if let Some(setter) = setter {
                        self.call(document, setter, receiver.clone(), vec![value], false)?;
                        return Ok(());
                    }
                }
            }
            receiver.put(key, value.clone());
            if receiver.same(&self.global) && self.env.borrow().bindings.contains_key(key) {
                self.env.borrow_mut().bindings.insert(key.into(), value);
            }
            return Ok(());
        }
        Err(Error::type_error("property assignment requires an object"))
    }
    pub fn call(
        &mut self,
        document: &mut BaseDocument,
        callee: Value,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Value> {
        let label = if callee.kind() == "Native" {
            callee.get("__native").text()
        } else {
            let name = callee.get("name").text();
            let location = callee
                .as_object()
                .and_then(|object| object.borrow().function.clone())
                .and_then(|function| function.source_offset);
            match location {
                Some(offset) => format!(
                    "{} at byte {offset}",
                    if name == "undefined" || name.is_empty() {
                        "<anonymous>"
                    } else {
                        &name
                    }
                ),
                None => name,
            }
        };
        self.call_stack.push(label.clone());
        let result = self.call_inner(document, callee, this, args, construct);
        if let Ok(value) = &result {
            if matches!(
                label.as_str(),
                "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError"
            ) {
                value.put_hidden(
                    "\0error_stack",
                    Value::array(
                        self.call_stack
                            .iter()
                            .rev()
                            .take(64)
                            .map(Value::string)
                            .collect(),
                    ),
                );
            }
        }
        self.call_stack.pop();
        result.map_err(|mut error| {
            if error.stack.len() < 64 {
                error
                    .stack
                    .push(if label == "undefined" || label.is_empty() {
                        String::from("<anonymous>")
                    } else {
                        label
                    });
            }
            error
        })
    }

    fn call_inner(
        &mut self,
        document: &mut BaseDocument,
        callee: Value,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Value> {
        let _scope = value::HeapScope::enter(&self.heap);
        self.tick()?;
        if let Some(value) =
            self.proxy_apply(document, &callee, this.clone(), args.clone(), construct)?
        {
            return Ok(value);
        }
        if callee.get("__native").text() == "Function.bound" {
            let target = callee.get("__bound_target");
            let mut arguments = callee.get("__bound_args").items();
            arguments.extend(args);
            return self.call(
                document,
                target,
                callee.get("__bound_this"),
                arguments,
                construct,
            );
        }
        let function = callee
            .as_object()
            .and_then(|object| object.borrow().function.clone());
        if let Some(function) = function {
            if callee
                .as_object()
                .and_then(|object| object.borrow().realm)
                .is_some_and(|realm| realm != self.scope_id)
            {
                return Err(Error::new(
                    "InvalidStateError",
                    "function belongs to another realm",
                ));
            }
            let this = if construct {
                let instance = Value::object("Object");
                if let Some(object) = instance.as_object() {
                    let prototype = callee.get("prototype");
                    if matches!(prototype, Value::Object(_)) {
                        object.borrow_mut().prototype = Some(prototype);
                    }
                }
                instance
            } else {
                this
            };
            return self.call_function(document, function, this, args, construct);
        }
        let name = callee.get("__native");
        if matches!(name, Value::String(_)) {
            let name = name.text();
            if let Some(level) = name.strip_prefix("Console.") {
                self.record_console(
                    level,
                    args.iter().map(Value::text).collect::<Vec<_>>().join(" "),
                );
                return Ok(Value::Undefined);
            }
            if name == "Guest.queueMicrotask" {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback.kind().as_str(), "Function" | "Native") {
                    return Err(Error::type_error("microtask callback must be callable"));
                }
                self.queue(callback, self.global.clone(), Vec::new());
                return Ok(Value::Undefined);
            }
            if name == "IPC.postMessage" {
                let Some(Value::String(message)) = args.first() else {
                    return Err(Error::type_error("IPC message must be a string"));
                };
                let handler = self
                    .ipc_handler
                    .as_ref()
                    .ok_or_else(|| Error::new("InvalidStateError", "IPC is not attached"))?;
                handler(String::from_utf16_lossy(message));
                return Ok(Value::Undefined);
            }
            if let Some(intrinsic) = self.intrinsics.get(&name).copied() {
                return intrinsic(self, &args);
            }
            if let Some(value) =
                self.bridge
                    .call(document, &name, this.clone(), args.clone(), construct)?
            {
                return Ok(value);
            }
            if let Some(value) =
                self.dom_call(document, &name, this.clone(), args.clone(), construct)?
            {
                return Ok(value);
            }
            if let Some(value) =
                self.web_call(document, &name, this.clone(), args.clone(), construct)?
            {
                return Ok(value);
            }
            if let Some(value) =
                self.events_call(document, &name, this.clone(), args.clone(), construct)?
            {
                return Ok(value);
            }
            if let Some(value) = self.regexp_call(document, &name, this.clone(), args.clone())? {
                return Ok(value);
            }
            if let Some(value) = self.weak_call(&name, this.clone(), args.clone())? {
                return Ok(value);
            }
            if let Some(value) = self.builtin_call(document, &name, this, args, construct)? {
                return Ok(value);
            }
            return Err(Error::type_error(format!(
                "native operation {name} is not admitted"
            )));
        }
        Err(Error::type_error(format!(
            "{} is not callable",
            callee.text()
        )))
    }
    pub fn report_error(&mut self, error: Error) {
        let sequence = self.errors.last().map_or(1, |entry| entry.sequence + 1);
        self.errors.push(DiagnosticEntry {
            sequence,
            order: sequence,
            level: "error".into(),
            message: format!("{}: {}", error.name, error.message),
            stack: error.stack.join("\n"),
        });
        if self.errors.len() > 1024 {
            self.errors.remove(0);
        }
    }
    pub fn record_console(&mut self, level: &str, message: String) {
        let sequence = self.console.last().map_or(1, |entry| entry.sequence + 1);
        self.console.push(DiagnosticEntry {
            sequence,
            order: sequence,
            level: level.into(),
            message,
            stack: String::new(),
        });
        if self.console.len() > 1024 {
            self.console.remove(0);
        }
    }
}

impl<B: RuntimeBridge> Drop for Guest<B> {
    fn drop(&mut self) {
        self.bridge.teardown();
        self.jobs.clear();
        self.callbacks.clear();
        self.nodes.clear();
        self.heap.borrow_mut().release();
    }
}

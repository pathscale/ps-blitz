//! Native guest trait implementations for the bounded Rust AST runtime.
//!
//! The document is borrowed for each entry into script or host work. No
//! document pointer is retained between calls.

use std::task::Context;

use blitz_dom::{BaseDocument, NodeId};
use blitz_traits::events::{BlitzPointerEvent, DomEvent, UiEvent};

use crate::diagnostics::{DiagnosticBatch, DiagnosticEntry, DiagnosticStreamBatch};
use crate::document_scripts::ScriptKind;
use crate::native_document::{NativeDocumentRuntime, TypedNativeGuest};
use crate::native_host::{NativeGuest, NativeHost, RealmScope, RetainedCallback};

use super::*;

const DIAGNOSTIC_CAPACITY: usize = 1024;
const POLL_JOB_LIMIT: usize = 4096;

/// Explicit typed-storage contract for embedders that own persistent storage.
/// The document runtime never guesses at storage through a stringly bridge call.
pub trait NativeStorageBridge: RuntimeBridge {
    fn storage_get(&mut self, key: &str) -> Result<Option<Value>>;
    fn storage_set(&mut self, key: &str, value: Value) -> Result<()>;
    fn storage_remove(&mut self, key: &str) -> Result<()>;
    fn storage_clear(&mut self) -> Result<()>;
}

fn stream_batch(entries: &[DiagnosticEntry], after: u64, limit: usize) -> DiagnosticStreamBatch {
    let first = entries
        .first()
        .map_or(after.saturating_add(1), |entry| entry.sequence);
    let lost = first.saturating_sub(after.saturating_add(1));
    let selected = entries
        .iter()
        .filter(|entry| entry.sequence > after)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_sequence = selected.last().map_or(after, |entry| entry.sequence);
    DiagnosticStreamBatch {
        entries: selected,
        lost,
        next_sequence,
    }
}

impl<B: RuntimeBridge> Guest<B> {
    /// Invoke a retained function while the owning document is borrowed.
    pub fn invoke_retained_callback_with_document(
        &mut self,
        document: &mut BaseDocument,
        callback: &RetainedCallback<u64, u64>,
        arguments: &[Value],
    ) -> Result<Value> {
        if !callback.is_current_in(&self.scope()) {
            return Err(Error::new("InvalidStateError", "stale retained callback"));
        }
        let value = self
            .callbacks
            .get(callback.callback())
            .cloned()
            .ok_or_else(|| Error::new("InvalidStateError", "unknown retained callback"))?;
        let function = value
            .as_object()
            .and_then(|object| object.borrow().function.clone())
            .ok_or_else(|| Error::type_error("retained value is no longer callable"))?;
        self.begin_execution();
        self.call_function(
            document,
            function,
            Value::Undefined,
            arguments.to_vec(),
            false,
        )
    }

    fn bridge_poll(
        &mut self,
        document: &mut BaseDocument,
        context: Option<&Context<'_>>,
        run_timers: bool,
    ) -> Result<bool> {
        if self.disposed || self.frozen {
            return Ok(false);
        }
        let _scope = value::HeapScope::enter(&self.heap);
        self.begin_execution();
        document.handle_messages();
        self.web_poll(document)?;
        let resource_work = self.dispatch_resource_events(document)?;
        let outcomes = document.take_image_load_outcomes();
        let queued_resources = !outcomes.is_empty();
        for (node, _generation, loaded) in outcomes {
            let Some(element) = document.get_node(node).and_then(|node| node.element_data()) else {
                continue;
            };
            let source = element
                .attr(blitz_dom::local_name!("src"))
                .unwrap_or("")
                .to_owned();
            self.queue_resource_event(
                document,
                node,
                &source,
                if loaded { "load" } else { "error" },
            );
        }
        let jobs =
            self.bridge
                .poll(document, context.map(|current| current.waker()), run_timers)?;
        let did_work = !jobs.is_empty() || !self.jobs.is_empty();
        self.jobs.extend(jobs);
        let drained = self.drain_jobs(document, POLL_JOB_LIMIT)?;
        self.web_poll(document)?;
        Ok(resource_work || queued_resources || did_work || drained)
    }

    fn dispatch_lifecycle_event(&mut self, document: &mut BaseDocument, name: &str) -> Result<()> {
        let event = Value::object("Event");
        event.put("type", Value::string(name));
        event.put("bubbles", Value::Bool(false));
        event.put("cancelable", Value::Bool(false));
        let document_value = self.global.get("document");
        let _ = self.dispatch_event(document, document_value, event.clone())?;
        let _ = self.dispatch_event(document, self.global.clone(), event)?;
        Ok(())
    }

    fn sync_document_state(&mut self) {
        let document = self.global.get("document");
        if document.kind() != "Undefined" {
            document.put("readyState", Value::string(&self.ready_state));
            document.put(
                "visibilityState",
                Value::string(if self.visible { "visible" } else { "hidden" }),
            );
        }
    }
}

impl<B: RuntimeBridge> NativeGuest for Guest<B> {
    type Value = Value;
    type RealmId = u64;
    type CallbackId = u64;
    type Error = Error;

    fn scope(&self) -> RealmScope<Self::RealmId> {
        RealmScope::new(self.scope_id, self.generation)
    }

    fn retain_callback(
        &mut self,
        callback: &Self::Value,
    ) -> Result<RetainedCallback<Self::RealmId, Self::CallbackId>, Self::Error> {
        if self.disposed
            || callback
                .as_object()
                .and_then(|object| object.borrow().realm)
                .is_some_and(|realm| realm != self.scope_id)
        {
            return Err(Error::new(
                "InvalidStateError",
                "callback does not belong to the active realm",
            ));
        }
        let callable = callback.kind() == "Function"
            || callback
                .as_object()
                .is_some_and(|object| object.borrow().function.is_some());
        if !callable {
            return Err(Error::type_error("retained callback must be callable"));
        }
        let id = self.next_callback;
        self.next_callback = id.checked_add(1).ok_or_else(|| {
            Error::new("QuotaExceededError", "callback identifier space exhausted")
        })?;
        self.callbacks.insert(id, callback.clone());
        Ok(RetainedCallback::new(self.scope_id, self.generation, id))
    }

    fn release_callback(
        &mut self,
        callback: &RetainedCallback<Self::RealmId, Self::CallbackId>,
    ) -> Result<(), Self::Error> {
        if !callback.is_current_in(&self.scope()) {
            return Err(Error::new("InvalidStateError", "stale retained callback"));
        }
        self.callbacks
            .remove(callback.callback())
            .map(|_| ())
            .ok_or_else(|| Error::new("InvalidStateError", "unknown retained callback"))
    }

    fn register_intrinsic(
        &mut self,
        name: &'static str,
        _arity: u8,
        callback: NativeIntrinsic<Self>,
    ) -> Result<(), Self::Error> {
        self.intrinsics.insert(name.to_owned(), callback);
        Ok(())
    }

    fn invoke_retained_callback(
        &mut self,
        callback: &RetainedCallback<Self::RealmId, Self::CallbackId>,
        _arguments: &[Self::Value],
    ) -> Result<Self::Value, Self::Error> {
        if !callback.is_current_in(&self.scope()) {
            return Err(Error::new("InvalidStateError", "stale retained callback"));
        }
        if !self.callbacks.contains_key(callback.callback()) {
            return Err(Error::new("InvalidStateError", "unknown retained callback"));
        }
        Err(Error::new(
            "InvalidStateError",
            "retained guest callback requires an explicit live document borrow",
        ))
    }
}

impl<B: RuntimeBridge + NativeStorageBridge> TypedNativeGuest for Guest<B> {
    fn supports_intl(&self) -> bool {
        self.global.get("Intl").kind() == "Intl"
            && self.global.get("Intl").get("NumberFormat").kind() == "Native"
    }

    fn supports_import_maps(&self) -> bool {
        false
    }

    fn supports_modules(&self) -> bool {
        true
    }

    fn install_document(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error> {
        if self.disposed {
            return Err(Error::new("InvalidStateError", "guest has been disposed"));
        }
        self.bridge.install(&self.global)?;
        // install_dom owns the Document-kind realm object. Attach the live root
        // identity to that object so DOM dispatch keeps its Document semantics.
        let document_value = self.global.get("document");
        if document_value.kind() != "Document" {
            return Err(Error::new(
                "InvalidStateError",
                "DOM document binding was not installed",
            ));
        }
        document_value.put_hidden(
            "__node",
            Value::string(document.root_node().id.as_u64().to_string()),
        );
        self.nodes
            .insert(document.root_node().id, document_value.clone());
        self.global.put("window", self.global.clone());
        self.global.put("self", self.global.clone());
        self.global.put("globalThis", self.global.clone());
        self.ready_state = "loading".to_owned();
        self.sync_document_state();
        Ok(())
    }

    fn execute_source(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
        _source_url: Option<&url::Url>,
        kind: &ScriptKind,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        match kind {
            ScriptKind::Classic => self.evaluate(document, source).map(|_| ()),
            // The bounded parser admits module bodies that use its supported
            // statement grammar. Imports and exports remain syntax errors.
            ScriptKind::Module => self.evaluate(document, source).map(|_| ()),
            ScriptKind::ImportMap => {
                Err(Error::syntax("import maps are not supported by this guest"))
            }
        }
    }

    fn install_import_map(
        &mut self,
        _source: &str,
        _base_url: Option<&url::Url>,
    ) -> Result<(), Self::Error> {
        Err(Error::syntax("import maps are not supported by this guest"))
    }

    fn dispatch_ui_event(
        &mut self,
        document: &mut BaseDocument,
        event: UiEvent,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.dispatch_input(document, event, None)
    }

    fn poll(
        &mut self,
        document: &mut BaseDocument,
        context: Option<Context<'_>>,
    ) -> Result<bool, Self::Error> {
        self.bridge_poll(document, context.as_ref(), true)
    }

    fn poll_for_settle(
        &mut self,
        document: &mut BaseDocument,
        context: Option<Context<'_>>,
        run_timers: bool,
    ) -> Result<bool, Self::Error> {
        self.bridge_poll(document, context.as_ref(), run_timers)
    }

    fn prepare_teardown(&mut self, _document: &mut BaseDocument) -> Result<(), Self::Error> {
        self.frozen = true;
        Ok(())
    }

    fn dispose_document(&mut self, _document: &mut BaseDocument) -> Result<(), Self::Error> {
        if !self.disposed {
            self.disposed = true;
            self.bridge.teardown();
            self.jobs.clear();
            self.callbacks.clear();
            self.nodes.clear();
            self.ipc_handler = None;
            self.global = Value::Undefined;
        }
        Ok(())
    }

    fn finish_loading(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error> {
        self.begin_execution();
        self.ready_state = "interactive".to_owned();
        self.sync_document_state();
        self.dispatch_lifecycle_event(document, "DOMContentLoaded")?;
        self.ready_state = "complete".to_owned();
        self.sync_document_state();
        self.dispatch_lifecycle_event(document, "load")
    }

    fn set_page_visible(
        &mut self,
        document: &mut BaseDocument,
        visible: bool,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.visible = visible;
        self.sync_document_state();
        self.dispatch_lifecycle_event(document, "visibilitychange")
    }

    fn freeze(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error> {
        self.begin_execution();
        self.dispatch_lifecycle_event(document, "freeze")?;
        self.frozen = true;
        Ok(())
    }

    fn finish_restore(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error> {
        self.begin_execution();
        self.frozen = false;
        self.dispatch_lifecycle_event(document, "pageshow")
    }

    fn resume(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error> {
        self.begin_execution();
        self.frozen = false;
        self.dispatch_lifecycle_event(document, "resume")?;
        let _ = self.bridge_poll(document, None, false)?;
        Ok(())
    }

    fn storage_get(&mut self, key: &str) -> Result<Option<Self::Value>, Self::Error> {
        self.bridge.storage_get(key)
    }

    fn storage_set(&mut self, key: &str, value: Self::Value) -> Result<(), Self::Error> {
        self.bridge.storage_set(key, value)
    }

    fn storage_remove(&mut self, key: &str) -> Result<(), Self::Error> {
        self.bridge.storage_remove(key)
    }

    fn storage_clear(&mut self) -> Result<(), Self::Error> {
        self.bridge.storage_clear()
    }
}

impl<B: RuntimeBridge + NativeStorageBridge> NativeDocumentRuntime for Guest<B> {
    fn eval_named(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
        _name: &str,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.evaluate(document, source).map(|_| ())
    }

    fn eval_json(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
    ) -> Result<serde_json::Value, Self::Error> {
        self.begin_execution();
        self.evaluate(document, source)?.to_json()
    }

    fn has_pending_jobs(&self) -> bool {
        !self.jobs.is_empty() || self.bridge.has_pending_jobs()
    }

    fn pending_task_count(&self) -> u64 {
        self.jobs.len() as u64 + self.bridge.pending_task_count()
    }

    fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> Result<DiagnosticBatch, Self::Error> {
        let limit = limit.min(DIAGNOSTIC_CAPACITY);
        Ok(DiagnosticBatch {
            console: stream_batch(&self.console, console_after, limit),
            errors: stream_batch(&self.errors, errors_after, limit),
        })
    }

    fn dispatch_dom_event(
        &mut self,
        document: &mut BaseDocument,
        event: DomEvent,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.dispatch_native_event(document, event)
    }

    fn handle_pointer_move_to_node(
        &mut self,
        document: &mut BaseDocument,
        event: BlitzPointerEvent,
        node: NodeId,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.dispatch_input(document, UiEvent::PointerMove(event), Some(node))
    }

    fn handle_ui_event_to_node(
        &mut self,
        document: &mut BaseDocument,
        event: UiEvent,
        node: NodeId,
    ) -> Result<(), Self::Error> {
        self.begin_execution();
        self.dispatch_input(document, event, Some(node))
    }

    fn dispatch_node_event(
        &mut self,
        document: &mut BaseDocument,
        node: NodeId,
        name: &str,
    ) -> Result<bool, Self::Error> {
        self.begin_execution();
        if document.get_node(node).is_none() {
            return Err(Error::new(
                "NotFoundError",
                "event target node no longer exists",
            ));
        }
        let event = Value::object("Event");
        event.put("type", Value::string(name));
        event.put("bubbles", Value::Bool(true));
        event.put("cancelable", Value::Bool(true));
        let target = self.node(node);
        self.dispatch_event(document, target, event)
    }

    fn set_ipc_handler(&mut self, handler: Box<dyn Fn(String)>) -> Result<(), Self::Error> {
        if self.disposed {
            return Err(Error::new("InvalidStateError", "guest has been disposed"));
        }
        self.ipc_handler = Some(handler);
        let ipc = Value::object("IPC");
        ipc.put("postMessage", Value::native("IPC.postMessage"));
        self.global.put("ipc", ipc);
        Ok(())
    }
}

impl<B: RuntimeBridge> NativeHost for GuestHost<B> {
    type Guest = Guest<B>;

    fn guest(&self) -> &Self::Guest {
        &self.0
    }

    fn guest_mut(&mut self) -> &mut Self::Guest {
        &mut self.0
    }
}

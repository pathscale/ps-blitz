//! Typed runtime driver for a document bound to a native guest and host.
//!
//! This wrapper delegates execution and document lifecycle to
//! [`BoundScriptDocument`]. It does not create or emulate a script backend.

use std::sync::Arc;
use std::task::Context;

use blitz_dom::{BaseDocument, DocGuard, DocGuardMut, Document, DocumentConfig, NodeId};
use blitz_traits::events::{BlitzPointerEvent, DomEvent, UiEvent};

use crate::ScriptDocumentBackend;
use crate::diagnostics::DiagnosticBatch;
use crate::native_document::{BoundScriptDocument, DocumentPollHook, NativeDocumentRuntime};
use crate::native_host::NativeHost;

/// A document runtime backed by one caller supplied native guest and host.
pub struct RuntimeDriver<G, H>
where
    G: NativeDocumentRuntime,
    H: NativeHost<Guest = G> + 'static,
{
    document: BoundScriptDocument<G, H>,
}

impl<G, H> RuntimeDriver<G, H>
where
    G: NativeDocumentRuntime,
    H: NativeHost<Guest = G> + 'static,
{
    /// Parse HTML and bind the supplied host and its guest.
    pub fn new_bound(html: Arc<str>, config: DocumentConfig, host: H) -> Result<Self, String> {
        BoundScriptDocument::new_bound(html, config, host).and_then(Self::from_document)
    }

    /// Wrap an already parsed and bound document.
    pub fn from_document(document: BoundScriptDocument<G, H>) -> Result<Self, String> {
        if document.guest().is_none() {
            return Err(String::from("a runtime driver requires a live bound guest"));
        }
        Ok(Self { document })
    }

    /// Borrow the bound document.
    pub fn document(&self) -> &BoundScriptDocument<G, H> {
        &self.document
    }

    /// Mutably borrow the bound document.
    pub fn document_mut(&mut self) -> &mut BoundScriptDocument<G, H> {
        &mut self.document
    }

    /// Consume the driver and return its bound document.
    pub fn into_document(self) -> BoundScriptDocument<G, H> {
        self.document
    }

    /// Execute the document's scripts through its bound guest.
    pub fn execute_scripts(&mut self) -> Result<(), String> {
        self.document.execute_scripts()
    }

    /// Evaluate source through the bound guest.
    pub fn eval(&mut self, source: &str) -> Result<(), String> {
        self.document.eval(source)
    }

    /// Evaluate named source through the bound guest.
    pub fn eval_named(&mut self, source: &str, name: &str) -> Result<(), String> {
        self.document.eval_named(source, name)
    }

    /// Evaluate source and return its JSON-compatible result.
    pub fn eval_json(&mut self, source: &str) -> Result<serde_json::Value, String> {
        self.document.eval_json(source)
    }

    /// Whether the guest reports pending jobs for this document.
    pub fn has_pending_jobs(&self) -> bool {
        self.document.has_pending_jobs()
    }

    /// Return a bounded batch of diagnostics from the bound guest.
    pub fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> Result<DiagnosticBatch, String> {
        self.document
            .diagnostic_batch(console_after, errors_after, limit)
    }

    /// Dispatch an event to a document node through the bound guest.
    pub fn dispatch_node_event(&mut self, node: NodeId, name: &str) -> Result<bool, String> {
        self.document.dispatch_node_event(node, name)
    }

    /// Access the native guest owned by this document.
    pub fn guest(&self) -> Option<&G> {
        self.document.guest()
    }

    /// Mutably access the native guest owned by this document.
    pub fn guest_mut(&mut self) -> Option<&mut G> {
        self.document.guest_mut()
    }

    /// Run a closure with mutable access to the native guest.
    pub fn with_guest_mut<R>(
        &mut self,
        callback: impl FnOnce(&mut G, &mut BaseDocument) -> R,
    ) -> Result<R, String> {
        self.document.with_guest_mut(callback)
    }

    pub fn pending_task_count(&self) -> u64 {
        self.document.pending_task_count()
    }

    pub fn dispatch_dom_event(&mut self, event: DomEvent) -> Result<(), String> {
        self.document.dispatch_dom_event(event)
    }

    pub fn handle_pointer_move_to_node(
        &mut self,
        event: BlitzPointerEvent,
        node: NodeId,
    ) -> Result<(), String> {
        self.document.handle_pointer_move_to_node(event, node)
    }

    pub fn handle_ui_event_to_node(&mut self, event: UiEvent, node: NodeId) -> Result<(), String> {
        self.document.handle_ui_event_to_node(event, node)
    }

    pub fn set_ipc_handler(&mut self, handler: Box<dyn Fn(String)>) -> Result<(), String> {
        self.document.set_ipc_handler(handler)
    }

    pub fn add_poll_hook(&mut self, hook: DocumentPollHook) -> Result<(), String> {
        self.document.add_poll_hook(hook)
    }
}

impl<G, H> Document for RuntimeDriver<G, H>
where
    G: NativeDocumentRuntime,
    H: NativeHost<Guest = G> + 'static,
{
    fn inner(&self) -> DocGuard<'_> {
        self.document.inner()
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        self.document.inner_mut()
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        self.document.handle_ui_event(event);
    }

    fn poll(&mut self, context: Option<Context>) -> bool {
        self.document.poll(context)
    }

    fn poll_for_settle(&mut self, context: Option<Context>, run_timers: bool) -> bool {
        self.document.poll_for_settle(context, run_timers)
    }

    fn prepare_teardown(&mut self) {
        self.document.prepare_teardown();
    }

    fn dispose(self: Box<Self>) -> Arc<blitz_dom::release::Teardown> {
        let Self { document } = *self;
        Box::new(document).dispose()
    }

    fn set_page_visible(&mut self, visible: bool) {
        self.document.set_page_visible(visible);
    }

    fn freeze(&mut self) {
        self.document.freeze();
    }

    fn finish_restore(&mut self) {
        self.document.finish_restore();
    }

    fn resume(&mut self) {
        self.document.resume();
    }
}

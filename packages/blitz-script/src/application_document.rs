use std::sync::Arc;
use std::task::Context;

use blitz_dom::{DocGuard, DocGuardMut, Document, DocumentConfig};
use blitz_traits::events::UiEvent;

/// A script-capable document backend constructible from HTML.
pub trait ScriptDocumentBackend: Document + Sized {
    fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String>;
    fn execute_scripts(&mut self) -> Result<(), String>;
}

#[repr(transparent)]
/// A statically dispatched document wrapper with the backend's layout and ownership.
pub struct ApplicationDocument<D: ScriptDocumentBackend>(D);

impl<D: ScriptDocumentBackend> ApplicationDocument<D> {
    pub fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String> {
        D::from_html(html, config).map(Self)
    }

    pub fn backend(&self) -> &D {
        &self.0
    }

    pub fn backend_mut(&mut self) -> &mut D {
        &mut self.0
    }

    pub fn into_backend(self) -> D {
        self.0
    }

    pub fn execute_scripts(&mut self) -> Result<(), String> {
        self.0.execute_scripts()
    }
}

impl<D: ScriptDocumentBackend> Document for ApplicationDocument<D> {
    fn inner(&self) -> DocGuard<'_> {
        self.0.inner()
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        self.0.inner_mut()
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        self.0.handle_ui_event(event)
    }

    fn poll(&mut self, context: Option<Context>) -> bool {
        self.0.poll(context)
    }

    fn poll_for_settle(&mut self, context: Option<Context>, timers: bool) -> bool {
        self.0.poll_for_settle(context, timers)
    }

    fn prepare_teardown(&mut self) {
        self.0.prepare_teardown();
    }

    fn dispose(self: Box<Self>) -> Arc<blitz_dom::release::Teardown> {
        Box::new(self.0).dispose()
    }

    fn set_page_visible(&mut self, visible: bool) {
        self.0.set_page_visible(visible);
    }

    fn freeze(&mut self) {
        self.0.freeze();
    }

    fn finish_restore(&mut self) {
        self.0.finish_restore();
    }

    fn resume(&mut self) {
        self.0.resume();
    }
}

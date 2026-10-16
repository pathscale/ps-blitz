use std::sync::Arc;
use std::task::Context;

use blitz_dom::{DocGuard, DocGuardMut, Document, DocumentConfig};
use blitz_traits::events::UiEvent;

/// A script-capable document backend constructible from HTML.
pub trait ScriptDocumentBackend: Document + Sized {
    fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String>;
    fn execute_scripts(&mut self) -> Result<(), String>;
}

/// Opt-in support for attaching an engine-native host to a script document.
///
/// Attachment is a separate, fallible step so callers can complete it after
/// constructing the document and before calling `execute_scripts`. This
/// extension does not change the requirements for [`ScriptDocumentBackend`].
pub trait NativeHostDocumentBackend: ScriptDocumentBackend {
    /// The engine-specific host owned by this backend after attachment.
    type Host;

    /// Attach `host` to this document before executing its scripts.
    ///
    /// Ownership of `host` transfers to the backend when this method is
    /// called. On success, the backend keeps it for the attached document's
    /// lifetime and is responsible for releasing it during teardown or
    /// disposal. On failure, the backend must release any partially attached
    /// resources and must not retain `host` as an attached host. This method
    /// does not execute scripts.
    fn attach_native_host(&mut self, host: Self::Host) -> Result<(), String>;
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

impl<D: NativeHostDocumentBackend> ApplicationDocument<D> {
    /// Attaches the backend's native host before script execution.
    ///
    /// Ownership and failure cleanup follow [`NativeHostDocumentBackend`].
    pub fn attach_native_host(&mut self, host: D::Host) -> Result<(), String> {
        self.0.attach_native_host(host)
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

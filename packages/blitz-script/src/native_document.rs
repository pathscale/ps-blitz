use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::Context;

use blitz_dom::{BaseDocument, DocGuard, DocGuardMut, Document, DocumentConfig, NodeId};
use blitz_html::HtmlDocument;
use blitz_traits::events::{BlitzPointerEvent, DomEvent, UiEvent};
use url::Url;

use crate::document_scripts::{DocumentScript, ScriptKind, document_scripts};
use crate::fetch::{DefaultScriptFetcher, ScriptFetcher};
use crate::native_host::{NativeGuest, NativeHost};
use crate::{NativeHostDocumentBackend, ScriptDocumentBackend};

/// A real native guest capable of running source and operating on a Blitz DOM.
///
/// Implementations provide the engine and host intrinsics. This adapter does
/// not manufacture a guest, evaluate source, or emulate DOM operations.
pub trait TypedNativeGuest: NativeGuest<Error: Debug> + 'static {
    /// Whether the guest implements the Intl APIs required by this runtime.
    fn supports_intl(&self) -> bool {
        false
    }

    /// Whether the guest can install and apply import maps.
    fn supports_import_maps(&self) -> bool {
        false
    }

    /// Whether the guest can execute ECMAScript modules.
    fn supports_modules(&self) -> bool {
        false
    }

    /// Install a parsed document and its host bindings into this guest.
    fn install_document(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    /// Execute source through this guest. Implementations must return an error
    /// when the selected script kind is unsupported.
    fn execute_source(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
        source_url: Option<&Url>,
        kind: &ScriptKind,
    ) -> Result<(), Self::Error>;

    /// Install an import map using the guest's native import-map machinery.
    fn install_import_map(
        &mut self,
        source: &str,
        base_url: Option<&Url>,
    ) -> Result<(), Self::Error>;

    /// Dispatch an input event through this guest's DOM event implementation.
    fn dispatch_ui_event(
        &mut self,
        document: &mut BaseDocument,
        event: UiEvent,
    ) -> Result<(), Self::Error>;

    /// Poll guest jobs and host work associated with this document.
    fn poll(
        &mut self,
        document: &mut BaseDocument,
        context: Option<Context>,
    ) -> Result<bool, Self::Error>;

    /// Drain immediately runnable work according to the document settle policy.
    fn poll_for_settle(
        &mut self,
        document: &mut BaseDocument,
        context: Option<Context>,
        run_timers: bool,
    ) -> Result<bool, Self::Error>;

    /// Notify the guest that this document is preparing for teardown.
    fn prepare_teardown(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    /// Notify the guest that this document is being disposed.
    fn dispose_document(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    /// Complete ready-state transitions and loading events after script bootstrap.
    fn finish_loading(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    /// Forward visibility and page lifecycle changes to the guest.
    fn set_page_visible(
        &mut self,
        document: &mut BaseDocument,
        visible: bool,
    ) -> Result<(), Self::Error>;

    fn freeze(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    fn finish_restore(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    fn resume(&mut self, document: &mut BaseDocument) -> Result<(), Self::Error>;

    /// Access the guest's native storage without stringifying guest values.
    fn storage_get(&mut self, key: &str) -> Result<Option<Self::Value>, Self::Error>;
    fn storage_set(&mut self, key: &str, value: Self::Value) -> Result<(), Self::Error>;
    fn storage_remove(&mut self, key: &str) -> Result<(), Self::Error>;
    fn storage_clear(&mut self) -> Result<(), Self::Error>;
}

/// Required runtime operations supplied by a native guest.
///
/// Basic document guests implement only `TypedNativeGuest`; executable
/// application hosts additionally implement these runtime operations.
pub trait NativeDocumentRuntime: TypedNativeGuest {
    fn eval_named(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
        name: &str,
    ) -> Result<(), Self::Error>;

    fn eval_json(
        &mut self,
        document: &mut BaseDocument,
        source: &str,
    ) -> Result<serde_json::Value, Self::Error>;

    fn has_pending_jobs(&self) -> bool;

    fn pending_task_count(&self) -> u64;

    fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> Result<crate::DiagnosticBatch, Self::Error>;

    fn dispatch_dom_event(
        &mut self,
        document: &mut BaseDocument,
        event: DomEvent,
    ) -> Result<(), Self::Error>;

    fn handle_pointer_move_to_node(
        &mut self,
        document: &mut BaseDocument,
        event: BlitzPointerEvent,
        node: NodeId,
    ) -> Result<(), Self::Error>;

    fn handle_ui_event_to_node(
        &mut self,
        document: &mut BaseDocument,
        event: UiEvent,
        node: NodeId,
    ) -> Result<(), Self::Error>;

    fn dispatch_node_event(
        &mut self,
        document: &mut BaseDocument,
        node: NodeId,
        name: &str,
    ) -> Result<bool, Self::Error>;

    fn set_ipc_handler(&mut self, handler: Box<dyn Fn(String)>) -> Result<(), Self::Error>;
}

/// Work invoked by a backend during its document poll, on the document thread.
pub type DocumentPollHook =
    Box<dyn FnMut(&mut dyn Document, Option<&Context<'_>>) -> bool + 'static>;

/// The installed host extension owns a concrete native host, which in turn
/// owns the guest used for all execution and document operations.
pub struct DocumentHostExtension<G: TypedNativeGuest> {
    host: Box<dyn NativeHost<Guest = G>>,
    teardown_started: bool,
    disposed: bool,
}

impl<G: TypedNativeGuest> DocumentHostExtension<G> {
    fn new<H>(host: H) -> Self
    where
        H: NativeHost<Guest = G> + 'static,
    {
        Self {
            host: Box::new(host),
            teardown_started: false,
            disposed: false,
        }
    }

    fn install(&mut self, document: &mut BaseDocument) -> Result<(), String> {
        self.host
            .guest_mut()
            .install_document(document)
            .map_err(|error| format!("failed to install native document: {error:?}"))?;
        Ok(())
    }

    fn guest(&self) -> &G {
        self.host.guest()
    }

    fn guest_mut(&mut self) -> &mut G {
        self.host.guest_mut()
    }

    fn prepare_teardown(&mut self, document: &mut BaseDocument) -> Result<(), String> {
        if self.teardown_started {
            return Ok(());
        }
        self.teardown_started = true;
        self.guest_mut()
            .prepare_teardown(document)
            .map_err(|error| format!("native guest teardown preparation failed: {error:?}"))
    }

    fn dispose(&mut self, document: &mut BaseDocument) -> Result<(), String> {
        if self.disposed {
            return Ok(());
        }
        self.disposed = true;
        self.guest_mut()
            .dispose_document(document)
            .map_err(|error| format!("native guest disposal failed: {error:?}"))
    }
}

/// A script document backed by an actual guest and host supplied by its caller.
///
/// `H` is retained as the host's concrete type for API clarity. The host is
/// owned behind the engine-neutral NativeHost boundary once attached.
pub struct BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    document: BaseDocument,
    scripts: Vec<DocumentScript>,
    base_url: Option<Url>,
    fetcher: Box<dyn ScriptFetcher>,
    extension: Option<DocumentHostExtension<G>>,
    last_error: Option<String>,
    execution: Option<Result<(), String>>,
    poll_hooks: Vec<DocumentPollHook>,
    retired: bool,
    _host: PhantomData<fn() -> H>,
}

impl<G, H> BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    /// Parse HTML and bind a real host before returning an executable document.
    pub fn new_bound(html: Arc<str>, config: DocumentConfig, host: H) -> Result<Self, String> {
        Self::new_bound_with_fetcher(html, config, host, DefaultScriptFetcher)
    }

    /// Variant of new_bound with an embedder-provided external script fetcher.
    pub fn new_bound_with_fetcher<F>(
        html: Arc<str>,
        config: DocumentConfig,
        host: H,
        fetcher: F,
    ) -> Result<Self, String>
    where
        F: ScriptFetcher,
    {
        let mut document = Self::parse(html, config, Box::new(fetcher))?;
        document.attach_native_host(host)?;
        Ok(document)
    }

    /// Return the most recent error from a Document callback, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Drain a callback failure so an application can publish it once.
    pub fn take_last_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    /// Add a poll hook without replacing hooks already registered.
    pub fn add_poll_hook(&mut self, hook: DocumentPollHook) -> Result<(), String> {
        self.ensure_live()?;
        self.poll_hooks.push(hook);
        Ok(())
    }

    /// Borrow the native guest while the document is live and attached.
    pub fn guest(&self) -> Option<&G> {
        if self.ensure_live().is_err() {
            return None;
        }
        self.extension.as_ref().map(DocumentHostExtension::guest)
    }

    /// Mutably borrow the native guest while the document is live and attached.
    pub fn guest_mut(&mut self) -> Option<&mut G> {
        if self.ensure_live().is_err() {
            return None;
        }
        self.extension
            .as_mut()
            .map(DocumentHostExtension::guest_mut)
    }

    /// Run a callback with mutable access to the guest and its document.
    pub fn with_guest_mut<R>(
        &mut self,
        callback: impl FnOnce(&mut G, &mut BaseDocument) -> R,
    ) -> Result<R, String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        Ok(callback(extension.guest_mut(), &mut self.document))
    }

    /// Read a native guest storage value.
    pub fn storage_get(&mut self, key: &str) -> Result<Option<G::Value>, String> {
        self.ensure_live()?;
        self.extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?
            .guest_mut()
            .storage_get(key)
            .map_err(|error| format!("native storage read failed: {error:?}"))
    }

    /// Write a native guest storage value.
    pub fn storage_set(&mut self, key: &str, value: G::Value) -> Result<(), String> {
        self.ensure_live()?;
        self.extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?
            .guest_mut()
            .storage_set(key, value)
            .map_err(|error| format!("native storage write failed: {error:?}"))
    }

    pub fn storage_remove(&mut self, key: &str) -> Result<(), String> {
        self.ensure_live()?;
        self.extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?
            .guest_mut()
            .storage_remove(key)
            .map_err(|error| format!("native storage removal failed: {error:?}"))
    }

    pub fn storage_clear(&mut self) -> Result<(), String> {
        self.ensure_live()?;
        self.extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?
            .guest_mut()
            .storage_clear()
            .map_err(|error| format!("native storage clear failed: {error:?}"))
    }

    fn parse(
        html: Arc<str>,
        config: DocumentConfig,
        fetcher: Box<dyn ScriptFetcher>,
    ) -> Result<Self, String> {
        let base_url = config
            .base_url
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|error| format!("invalid document base URL: {error}"))?;
        let parsed = HtmlDocument::from_html(&html, config);
        let document = parsed.into_inner();
        let scripts = document_scripts(&document);
        Ok(Self {
            document,
            scripts,
            base_url,
            fetcher,
            extension: None,
            last_error: None,
            execution: None,
            poll_hooks: Vec::new(),
            retired: false,
            _host: PhantomData,
        })
    }

    fn execute_all_scripts(&mut self) -> Result<(), String> {
        let extension = self.extension.as_mut().ok_or_else(|| {
            String::from("cannot execute scripts before a native host is attached")
        })?;

        #[cfg(feature = "intl")]
        if !extension.guest_mut().supports_intl() {
            return Err(String::from(
                "the attached native guest does not provide Intl support",
            ));
        }

        for script in &self.scripts {
            let source_url = script
                .src
                .as_deref()
                .map(|src| resolve_script_url(self.base_url.as_ref(), src))
                .transpose()?;
            let source = match (script.src.as_deref(), source_url.as_ref()) {
                (Some(_), Some(url)) => self
                    .fetcher
                    .fetch_for_document(url, self.base_url.as_ref())
                    .map_err(|error| format!("failed to fetch script {url}: {error}"))?,
                _ => script.source.clone(),
            };

            if matches!(&script.kind, ScriptKind::ImportMap) {
                let guest = extension.guest_mut();
                if !guest.supports_import_maps() {
                    return Err(String::from(
                        "the attached native guest does not support import maps",
                    ));
                }
                guest
                    .install_import_map(&source, source_url.as_ref().or(self.base_url.as_ref()))
                    .map_err(|error| format!("failed to install import map: {error:?}"))?;
                continue;
            }

            if matches!(&script.kind, ScriptKind::Module) {
                let guest = extension.guest_mut();
                if !guest.supports_modules() {
                    return Err(String::from(
                        "the attached native guest does not support modules",
                    ));
                }
            }

            extension
                .guest_mut()
                .execute_source(
                    &mut self.document,
                    &source,
                    source_url.as_ref().or(self.base_url.as_ref()),
                    &script.kind,
                )
                .map_err(|error| format!("native script execution failed: {error:?}"))?;
        }
        extension
            .guest_mut()
            .finish_loading(&mut self.document)
            .map_err(|error| format!("native loading completion failed: {error:?}"))
    }

    fn ensure_live(&self) -> Result<(), String> {
        if self.retired || self.document.page_retiring {
            Err(String::from("native document has been retired"))
        } else {
            Ok(())
        }
    }

    fn run_guest_callback(
        &mut self,
        callback: impl FnOnce(&mut G, &mut BaseDocument) -> Result<(), G::Error>,
    ) {
        if self.ensure_live().is_err() {
            return;
        }
        let Some(extension) = self.extension.as_mut() else {
            return;
        };
        if let Err(error) = callback(extension.guest_mut(), &mut self.document) {
            self.last_error = Some(format!("native document callback failed: {error:?}"));
        }
    }

    fn run_poll_hooks(&mut self, context: Option<&Context<'_>>) -> bool {
        if self.ensure_live().is_err() {
            return false;
        }
        let mut hooks = std::mem::take(&mut self.poll_hooks);
        let mut work_remains = false;
        for hook in &mut hooks {
            if self.document.page_frozen {
                break;
            }
            work_remains |= hook(self, context);
            if self.ensure_live().is_err() {
                self.poll_hooks.clear();
                return false;
            }
        }
        hooks.append(&mut self.poll_hooks);
        self.poll_hooks = hooks;
        work_remains
    }
}

impl<G, H> BoundScriptDocument<G, H>
where
    G: NativeDocumentRuntime,
    H: NativeHost<Guest = G> + 'static,
{
    /// Evaluate source through the bound native guest.
    pub fn eval(&mut self, source: &str) -> Result<(), String> {
        self.eval_named(source, "<eval>")
    }

    /// Evaluate named source through the bound native guest.
    pub fn eval_named(&mut self, source: &str, name: &str) -> Result<(), String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .eval_named(&mut self.document, source, name)
            .map_err(|error| format!("native source evaluation failed: {error:?}"))
    }

    /// Evaluate source and return its JSON-compatible result.
    pub fn eval_json(&mut self, source: &str) -> Result<serde_json::Value, String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .eval_json(&mut self.document, source)
            .map_err(|error| format!("native JSON evaluation failed: {error:?}"))
    }

    /// Whether the live guest reports pending jobs for this document.
    pub fn has_pending_jobs(&self) -> bool {
        self.ensure_live().is_ok()
            && self
                .extension
                .as_ref()
                .is_some_and(|extension| extension.guest().has_pending_jobs())
    }

    /// Return the live guest's pending task count.
    pub fn pending_task_count(&self) -> u64 {
        if self.ensure_live().is_err() {
            return 0;
        }
        self.extension
            .as_ref()
            .map_or(0, |extension| extension.guest().pending_task_count())
    }

    /// Return a bounded batch of diagnostics from the bound guest.
    pub fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> Result<crate::DiagnosticBatch, String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_ref()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest()
            .diagnostic_batch(console_after, errors_after, limit)
            .map_err(|error| format!("native diagnostic read failed: {error:?}"))
    }

    /// Dispatch a DOM event through the bound native guest.
    pub fn dispatch_dom_event(&mut self, event: DomEvent) -> Result<(), String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .dispatch_dom_event(&mut self.document, event)
            .map_err(|error| format!("native DOM event dispatch failed: {error:?}"))
    }

    /// Move pointer state to a caller-resolved node through the bound guest.
    pub fn handle_pointer_move_to_node(
        &mut self,
        event: BlitzPointerEvent,
        node: NodeId,
    ) -> Result<(), String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .handle_pointer_move_to_node(&mut self.document, event, node)
            .map_err(|error| format!("native pointer dispatch failed: {error:?}"))
    }

    /// Dispatch UI input to a caller-resolved node through the bound guest.
    pub fn handle_ui_event_to_node(&mut self, event: UiEvent, node: NodeId) -> Result<(), String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .handle_ui_event_to_node(&mut self.document, event, node)
            .map_err(|error| format!("native UI event dispatch failed: {error:?}"))
    }

    /// Dispatch a named event at a node through the bound guest.
    pub fn dispatch_node_event(&mut self, node: NodeId, name: &str) -> Result<bool, String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .dispatch_node_event(&mut self.document, node, name)
            .map_err(|error| format!("native node event dispatch failed: {error:?}"))
    }

    /// Install an IPC handler through the bound native guest.
    pub fn set_ipc_handler(&mut self, handler: Box<dyn Fn(String)>) -> Result<(), String> {
        self.ensure_live()?;
        let extension = self
            .extension
            .as_mut()
            .ok_or_else(|| String::from("native host is not attached"))?;
        extension
            .guest_mut()
            .set_ipc_handler(handler)
            .map_err(|error| format!("native IPC handler installation failed: {error:?}"))
    }
}

fn resolve_script_url(base_url: Option<&Url>, source: &str) -> Result<Url, String> {
    if let Some(base_url) = base_url {
        base_url
            .join(source)
            .map_err(|error| format!("invalid script URL {source}: {error}"))
    } else {
        Url::parse(source)
            .map_err(|error| format!("relative script URL has no document base URL: {error}"))
    }
}

impl<G, H> ScriptDocumentBackend for BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    fn from_html(html: Arc<str>, config: DocumentConfig) -> Result<Self, String> {
        Self::parse(html, config, Box::new(DefaultScriptFetcher))
    }

    fn execute_scripts(&mut self) -> Result<(), String> {
        self.ensure_live()?;
        if self.extension.is_none() {
            return Err(String::from(
                "cannot execute scripts before a native host is attached",
            ));
        }
        if let Some(result) = &self.execution {
            return result.clone();
        }
        let result = self.execute_all_scripts();
        self.execution = Some(result.clone());
        result
    }
}

impl<G, H> NativeHostDocumentBackend for BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    type Host = H;

    fn attach_native_host(&mut self, host: Self::Host) -> Result<(), String> {
        self.ensure_live()?;
        if self.extension.is_some() {
            return Err(String::from("a native host is already attached"));
        }
        let mut extension = DocumentHostExtension::new(host);
        if let Err(error) = extension.install(&mut self.document) {
            let _ = extension.prepare_teardown(&mut self.document);
            let _ = extension.dispose(&mut self.document);
            return Err(error);
        }
        self.extension = Some(extension);
        Ok(())
    }
}

impl<G, H> Document for BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    fn inner(&self) -> DocGuard<'_> {
        self.document.inner()
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        self.document.inner_mut()
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        if self.document.page_frozen || self.document.page_input_blocked {
            return;
        }
        self.run_guest_callback(|guest, document| guest.dispatch_ui_event(document, event));
    }

    fn poll(&mut self, context: Option<Context>) -> bool {
        if self.ensure_live().is_err() || self.document.page_frozen {
            return false;
        }
        let guest_work = if let Some(extension) = self.extension.as_mut() {
            let guest_context = context.as_ref().map(|ctx| Context::from_waker(ctx.waker()));
            match extension
                .guest_mut()
                .poll(&mut self.document, guest_context)
            {
                Ok(work_remains) => work_remains,
                Err(error) => {
                    self.last_error = Some(format!("native document poll failed: {error:?}"));
                    false
                }
            }
        } else {
            return false;
        };
        let hook_work = self.run_poll_hooks(context.as_ref());
        self.ensure_live().is_ok() && (hook_work | guest_work)
    }

    fn poll_for_settle(&mut self, context: Option<Context>, run_timers: bool) -> bool {
        if self.ensure_live().is_err() || self.document.page_frozen {
            return false;
        }
        let guest_work = if let Some(extension) = self.extension.as_mut() {
            match extension.guest_mut().poll_for_settle(
                &mut self.document,
                context.as_ref().map(|ctx| Context::from_waker(ctx.waker())),
                run_timers,
            ) {
                Ok(work_remains) => work_remains,
                Err(error) => {
                    self.last_error = Some(format!("native settle poll failed: {error:?}"));
                    false
                }
            }
        } else {
            return false;
        };
        let hook_work = self.run_poll_hooks(context.as_ref());
        self.ensure_live().is_ok() && (hook_work | guest_work)
    }

    fn prepare_teardown(&mut self) {
        self.retired = true;
        self.poll_hooks.clear();
        if let Some(extension) = self.extension.as_mut() {
            if let Err(error) = extension.prepare_teardown(&mut self.document) {
                self.last_error = Some(error);
            }
        }
        self.document.prepare_teardown();
    }

    fn dispose(mut self: Box<Self>) -> Arc<blitz_dom::release::Teardown> {
        self.prepare_teardown();
        if let Some(extension) = self.extension.as_mut() {
            if let Err(error) = extension.dispose(&mut self.document) {
                self.last_error = Some(error);
            }
        }
        let receipt = Arc::clone(&self.document.inner().teardown);
        drop(self);
        receipt
    }

    fn set_page_visible(&mut self, visible: bool) {
        self.document.set_page_visible(visible);
        self.run_guest_callback(|guest, document| guest.set_page_visible(document, visible));
    }

    fn freeze(&mut self) {
        self.document.freeze();
        self.run_guest_callback(|guest, document| guest.freeze(document));
    }

    fn finish_restore(&mut self) {
        self.document.finish_restore();
        self.run_guest_callback(|guest, document| guest.finish_restore(document));
    }

    fn resume(&mut self) {
        self.document.resume();
        self.run_guest_callback(|guest, document| guest.resume(document));
    }
}

impl<G, H> Drop for BoundScriptDocument<G, H>
where
    G: TypedNativeGuest,
    H: NativeHost<Guest = G> + 'static,
{
    fn drop(&mut self) {
        self.prepare_teardown();
        if let Some(extension) = self.extension.as_mut() {
            let _ = extension.dispose(&mut self.document);
        }
    }
}

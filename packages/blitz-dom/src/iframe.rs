//! Loading of `<iframe>` elements into sub-documents.

use blitz_traits::handoff::EventTarget;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use blitz_traits::net::{AbortController, AbortSignal, RequestDestination, Url};
use blitz_traits::node_id::NodeId;

use crate::document::DocumentEvent;
use crate::net::{DocumentSrcHandler, ResourceHandler, stamped_request};
use crate::{BaseDocument, DocumentConfig, local_name};

/// Maximum nesting depth of documents-within-documents. Iframes nested deeper
/// than this are not loaded (this guards against infinitely recursive
/// self-embedding pages).
pub(crate) const MAX_SUBDOCUMENT_DEPTH: usize = 10;
pub(crate) const MAX_IFRAMES_PER_DOCUMENT: usize = 64;
const MAX_LIVE_IFRAMES: usize = 64;

static RESERVED_IFRAMES: AtomicUsize = AtomicUsize::new(0);

struct IframePermit;

impl IframePermit {
    fn acquire() -> Option<Self> {
        let mut reserved = RESERVED_IFRAMES.load(Ordering::Acquire);
        loop {
            if reserved >= MAX_LIVE_IFRAMES {
                return None;
            }
            match RESERVED_IFRAMES.compare_exchange_weak(
                reserved,
                reserved + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(observed) => reserved = observed,
            }
        }
    }
}

impl Drop for IframePermit {
    fn drop(&mut self) {
        let _ = RESERVED_IFRAMES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Per-iframe load state: the abort controller covering the current
/// "generation" of the iframe's content (the HTML fetch and, once loaded, the
/// sub-document's own sub-resource fetches), plus the id of the in-flight HTML
/// request (if any) so stale responses can be discarded.
pub(crate) struct IframeLoad {
    pub(crate) request_id: Option<usize>,
    pub(crate) abort_controller: AbortController,
    permit: IframePermit,
}

/// A [`NavigationProvider`] given to iframe sub-documents. It routes
/// navigations (e.g. link clicks within the iframe) back to the parent
/// document, which reloads the iframe's sub-document with the new URL.
struct IframeNavigationProvider {
    node_id: NodeId,
    tx: EventTarget<DocumentEvent>,
}

impl NavigationProvider for IframeNavigationProvider {
    fn navigate_to(&self, options: NavigationOptions) {
        let _ = self.tx.publish(Arc::new(DocumentEvent::NavigateIframe {
            node_id: self.node_id,
            url: options.url,
        }));
    }
}

impl BaseDocument {
    /// Build the [`DocumentConfig`] for an iframe sub-document, inheriting
    /// providers and settings from this (parent) document.
    fn iframe_document_config(
        &self,
        node_id: NodeId,
        base_url: Option<String>,
        abort_signal: AbortSignal,
        inherit_origin: bool,
    ) -> Option<DocumentConfig> {
        let net_provider = if inherit_origin {
            Arc::clone(&self.net_provider)
        } else {
            let base_url = base_url.as_ref()?;
            let url = Url::parse(base_url).ok()?;
            self.net_provider.for_child_document(url)?
        };
        Some(DocumentConfig {
            viewport: None,
            base_url,
            ua_stylesheets: None,
            net_provider: Some(net_provider),
            navigation_provider: Some(Arc::new(IframeNavigationProvider {
                node_id,
                tx: self.tx.clone(),
            })),
            shell_provider: Some(self.shell_provider.clone()),
            html_parser_provider: Some(self.html_parser_provider.clone()),
            font_ctx: Some(self.font_ctx.lock().unwrap().clone()),
            media_type: Some(self.media_type.clone()),
            style_threading: self.style_threading,
            incremental: Some(self.incremental_layout),
            abort_signal: Some(abort_signal),
            teardown: None,
            subdocument_depth: self.subdocument_depth + 1,
            // Process-wide, already set by the parent if it had one.
            family_fallback: None,
        })
    }

    /// Abort the current generation of the iframe's content (in-flight HTML
    /// fetch and/or the loaded sub-document's sub-resource fetches) and start
    /// a new generation.
    fn new_iframe_generation(
        &mut self,
        node_id: NodeId,
        request_id: Option<usize>,
    ) -> Option<AbortSignal> {
        if self.subdocument_depth >= MAX_SUBDOCUMENT_DEPTH {
            return None;
        }

        let old_load = self.iframe_loads.remove(&node_id);
        let permit = if let Some(load) = old_load {
            load.abort_controller.abort();
            self.remove_sub_document(node_id);
            load.permit
        } else {
            if self.iframe_loads.len() >= MAX_IFRAMES_PER_DOCUMENT {
                return None;
            }
            let permit = IframePermit::acquire()?;
            self.remove_sub_document(node_id);
            permit
        };

        let abort_controller = AbortController::default();
        let signal = abort_controller.signal.clone();
        self.iframe_loads.insert(
            node_id,
            IframeLoad {
                request_id,
                abort_controller,
                permit,
            },
        );
        Some(signal)
    }

    /// Parse `html` and attach the resulting document as `node_id`'s
    /// sub-document.
    pub(crate) fn attach_iframe_document(
        &mut self,
        node_id: NodeId,
        html: &str,
        base_url: Option<String>,
        abort_signal: AbortSignal,
        inherit_origin: bool,
    ) {
        let Some(config) =
            self.iframe_document_config(node_id, base_url, abort_signal, inherit_origin)
        else {
            if let Some(load) = self.iframe_loads.remove(&node_id) {
                load.abort_controller.abort();
            }
            return;
        };
        let sub_doc = self
            .html_parser_provider
            .clone()
            .parse_document(html, config);
        self.set_sub_document(node_id, sub_doc);
        self.shell_provider.request_redraw();
    }

    /// Attach a sub-document parsed from the iframe's `srcdoc` attribute.
    /// Relative URLs within a `srcdoc` document resolve against the parent
    /// document's base URL.
    pub(crate) fn load_iframe_srcdoc(&mut self, node_id: NodeId, srcdoc: &str) {
        let Some(signal) = self.new_iframe_generation(node_id, None) else {
            return;
        };
        let base_url = Some(self.url.to_string());
        self.attach_iframe_document(node_id, srcdoc, base_url, signal, true);
    }

    /// Start fetching HTML for an iframe from `url`. The parsed document is
    /// attached when the response arrives (via [`DocumentEvent::ResourceLoad`]).
    pub(crate) fn start_iframe_load(&mut self, node_id: NodeId, url: Url) {
        let handler = ResourceHandler::new(
            self.tx.clone(),
            self.id(),
            Some(node_id),
            self.shell_provider.clone(),
            DocumentSrcHandler,
        );
        let Some(signal) = self.new_iframe_generation(node_id, Some(handler.request_id())) else {
            return;
        };
        self.net_provider.fetch(
            self.id(),
            stamped_request(url, Some(&signal)).destination(RequestDestination::Iframe),
            Box::new(handler),
        );
    }

    /// Handle a navigation originating from within an iframe's sub-document
    /// (e.g. a link click) by loading the new URL into the iframe.
    pub(crate) fn navigate_iframe(&mut self, node_id: NodeId, url: Url) {
        let Some(node) = self.get_node(node_id) else {
            return;
        };
        if !node.flags.is_in_document() {
            return;
        }
        self.start_iframe_load(node_id, url);
    }

    /// Apply fetched iframe HTML, discarding stale responses (the iframe may
    /// have been removed or re-navigated since the request was issued).
    pub(crate) fn apply_iframe_html(
        &mut self,
        node_id: NodeId,
        request_id: usize,
        resolved_url: Option<String>,
        html: &str,
    ) {
        let is_current = self
            .iframe_loads
            .get(&node_id)
            .is_some_and(|load| load.request_id == Some(request_id));
        if !is_current {
            return;
        }
        let node_is_iframe = self.get_node(node_id).is_some_and(|node| {
            node.flags.is_in_document()
                && node
                    .element_data()
                    .is_some_and(|el| el.name.local == local_name!("iframe"))
        });
        if !node_is_iframe {
            if let Some(load) = self.iframe_loads.remove(&node_id) {
                load.abort_controller.abort();
            }
            return;
        }

        let load = self.iframe_loads.get_mut(&node_id).unwrap();
        load.request_id = None;
        let signal = load.abort_controller.signal.clone();

        self.attach_iframe_document(node_id, html, resolved_url, signal, false);
    }
}

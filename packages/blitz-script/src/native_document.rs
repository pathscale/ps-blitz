//! Native script document. Its BaseDocument is stored directly in its engine.

use std::collections::HashSet;
use std::sync::Arc;
use std::task::Context;

use blitz_dom::{
    BaseDocument, DEFAULT_CSS, DocGuard, DocGuardMut, Document, DocumentConfig, NodeId,
    StyleThreading,
};
use blitz_html::DocumentHtmlParser;
use ekojit::page::{NativeReceipt, PageError, PageSource};
use markup5ever::namespace_url;

use crate::engine::Engine;
use crate::native::{EkojitScriptEngine, NativePageDefect};

pub struct EkojitDocument {
    runtime: Engine<EkojitScriptEngine>,
    executed: HashSet<NodeId>,
    started: bool,
}

impl EkojitDocument {
    pub fn from_html(
        html: Arc<str>,
        mut config: DocumentConfig,
        defect: NativePageDefect,
    ) -> Result<Self, PageError> {
        if DocumentHtmlParser::is_xhtml_document(&html) {
            return Err(PageError(Arc::from("EKO_PAGE_UNSUPPORTED: XHTML")));
        }
        if let Some(styles) = &mut config.ua_stylesheets {
            if !styles.iter().any(|style| style == DEFAULT_CSS) {
                styles.push(DEFAULT_CSS.into());
            }
        } else {
            config.ua_stylesheets = Some(vec![DEFAULT_CSS.into()]);
        }
        config.style_threading = StyleThreading::Sequential;
        let mut document = BaseDocument::new(config);
        DocumentHtmlParser::parse_into_mutator(&mut document.mutate(), &html);
        Ok(Self {
            runtime: Engine::new(EkojitScriptEngine::new(document, defect)),
            executed: HashSet::new(),
            started: false,
        })
    }

    /// Native startup scans the parsed owner DOM. External source admission
    /// belongs to the host's resource policy and is explicitly unavailable in
    /// this first inline-only path, rather than invoking an unscoped fetcher.
    pub fn execute_scripts(&mut self) -> Result<(), PageError> {
        let _slice = self.runtime.begin_job_slice();
        let scripts = {
            let document = &self.runtime.backend().host.document;
            let mut scripts = Vec::new();
            let mut stack = vec![document.root_node().id];
            while let Some(id) = stack.pop() {
                let Some(node) = document.get_node(id) else {
                    continue;
                };
                if let Some(element) = node.element_data() {
                    if element.name.ns == markup5ever::ns!(html)
                        && element.name.local == blitz_dom::local_name!("script")
                    {
                        if node
                            .flags
                            .contains(blitz_dom::node::NodeFlags::IS_PARSER_INERT_SCRIPT)
                        {
                            continue;
                        }
                        let kind = element
                            .attr(blitz_dom::local_name!("type"))
                            .unwrap_or("")
                            .trim()
                            .to_ascii_lowercase();
                        if kind == "module"
                            || kind == "importmap"
                            || element.attr(blitz_dom::local_name!("src")).is_some()
                        {
                            return Err(PageError(Arc::from(
                                "EKO_PAGE_UNSUPPORTED: module, import map or external script",
                            )));
                        }
                        if matches!(
                            kind.as_str(),
                            "" | "text/javascript" | "application/javascript"
                        ) && element.attr(blitz_dom::local_name!("nomodule")).is_none()
                            && !self.executed.contains(&id)
                        {
                            scripts.push((id, Arc::<str>::from(node.text_content())));
                        }
                        continue;
                    }
                }
                stack.extend(node.children.iter().rev().copied());
            }
            scripts
        };
        self.started = true;
        for (id, text) in scripts {
            self.executed.insert(id);
            if text.trim().is_empty() {
                continue;
            }
            self.runtime
                .backend_mut()
                .eval_source(Arc::new(PageSource {
                    text,
                    url: Arc::from(format!("<native inline {}>", id.as_u64())),
                }))?;
        }
        self.runtime.set_document_ready_state("complete");
        self.runtime
            .backend()
            .host
            .document
            .shell_provider
            .request_redraw();
        Ok(())
    }

    pub fn eval_source(
        &mut self,
        source: Arc<PageSource>,
    ) -> Result<Arc<NativeReceipt>, PageError> {
        self.runtime.backend_mut().eval_source(source)
    }
    pub fn native_receipts(&self) -> &[Arc<NativeReceipt>] {
        self.runtime.backend().receipts()
    }
    pub fn title(&self) -> String {
        self.runtime.backend().host.title_text()
    }
    pub fn console_entries(&self) -> Vec<crate::DiagnosticEntry> {
        self.runtime.backend().console()
    }
    pub fn runtime_errors(&self) -> Vec<crate::DiagnosticEntry> {
        self.runtime.backend().errors()
    }
    pub fn page_source(&self) -> String {
        self.runtime
            .backend()
            .host
            .document
            .root_element()
            .outer_html()
    }
}

impl Document for EkojitDocument {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::Ref(&self.runtime.backend().host.document)
    }
    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::Ref(&mut self.runtime.backend_mut().host.document)
    }
    fn poll(&mut self, context: Option<Context>) -> bool {
        let startup = !self.started;
        if startup {
            self.started = true;
            if let Err(error) = self.execute_scripts() {
                eprintln!("blitz-script: {error}");
            }
        }
        let changed = self.runtime.backend_mut().host.document.poll(context);
        startup || changed
    }
}

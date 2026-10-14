//! Owner-local DOM capabilities for the bounded native page backend.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{BaseDocument, NodeId, local_name};
use blitz_traits::events::{DomEvent, EventState};
use ekojit::page::{HostString, NativePage, NativeReceipt, PageError, PageHost, PageSource};
use url::Url;
use web_time::{Duration, Instant};

use blitz_script::DiagnosticEntry;
use blitz_script::engine::ScriptEngine;
use blitz_script::engine::{JobBudget, JobSlice};
use blitz_script::{DiagnosticBatch, DiagnosticStreamBatch};

/// The explicit defect changes the real host append, without changing errors
/// or the witness verdict. Applications select None unless proving the gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativePageDefect {
    #[default]
    None,
    HostTextAppendNoop,
}

#[derive(Default)]
struct Diagnostics {
    order: u64,
    console_sequence: u64,
    error_sequence: u64,
    console: VecDeque<DiagnosticEntry>,
    errors: VecDeque<DiagnosticEntry>,
}

impl Diagnostics {
    fn push(&mut self, message: String, error: bool) {
        self.order += 1;
        let (sequence, queue, level) = if error {
            self.error_sequence += 1;
            (self.error_sequence, &mut self.errors, "error")
        } else {
            self.console_sequence += 1;
            (self.console_sequence, &mut self.console, "log")
        };
        if queue.len() == 1000 {
            queue.pop_front();
        }
        queue.push_back(DiagnosticEntry {
            sequence,
            order: self.order,
            level: level.into(),
            message,
            stack: String::new(),
        });
    }
}

pub(crate) struct DomHost {
    pub(crate) document: BaseDocument,
    detached: HashSet<NodeId>,
    diagnostics: RefCell<Diagnostics>,
    defect: NativePageDefect,
}

impl DomHost {
    fn find(&self, name: blitz_dom::LocalName) -> Option<NodeId> {
        let mut stack = vec![self.document.root_node().id];
        while let Some(id) = stack.pop() {
            let Some(node) = self.document.get_node(id) else {
                continue;
            };
            if node.element_data().is_some_and(|element| {
                element.name.ns == markup5ever::ns!(html) && element.name.local == name
            }) {
                return Some(id);
            }
            stack.extend(node.children.iter().rev().copied());
        }
        None
    }

    pub(crate) fn title_text(&self) -> String {
        self.find(local_name!("title"))
            .and_then(|id| self.document.get_node(id))
            .map(|node| node.text_content())
            .unwrap_or_default()
    }

    fn sweep(&mut self) {
        let mut mutator = self.document.mutate();
        for id in self.detached.drain() {
            mutator.remove_node_if_unparented(id);
        }
    }
}

impl PageHost for DomHost {
    type TextNode = NodeId;

    fn title(&self) -> Result<HostString, PageError> {
        Ok(Arc::from(
            self.title_text().encode_utf16().collect::<Vec<_>>(),
        ))
    }

    fn set_title(&mut self, title: HostString) -> Result<(), PageError> {
        let text = String::from_utf16_lossy(&title);
        let existing = self.find(local_name!("title"));
        let head = self.find(local_name!("head"));
        let mut mutator = self.document.mutate();
        let id = match (existing, head) {
            (Some(id), _) => id,
            (None, Some(head)) => {
                let id = mutator.create_element(
                    markup5ever::QualName::new(None, markup5ever::ns!(html), local_name!("title")),
                    Vec::new(),
                );
                mutator.append_children(head, &[id]);
                id
            }
            (None, None) => return Ok(()),
        };
        mutator.remove_and_drop_all_children(id);
        let child = mutator.create_text_node(&text);
        mutator.append_children(id, &[child]);
        drop(mutator);
        self.document.shell_provider.set_window_title(text);
        Ok(())
    }

    fn console_log(&mut self, message: HostString) -> Result<(), PageError> {
        let text = String::from_utf16_lossy(&message);
        eprintln!("{text}");
        self.diagnostics.borrow_mut().push(text, false);
        Ok(())
    }

    fn create_text_node(&mut self, text: HostString) -> Result<NodeId, PageError> {
        let id = self
            .document
            .mutate()
            .create_text_node(&String::from_utf16_lossy(&text));
        self.detached.insert(id);
        Ok(id)
    }

    fn append_to_body(&mut self, node: &NodeId) -> Result<(), PageError> {
        if !self.detached.contains(node) {
            return Err(PageError(Arc::from(
                "EKO_DOM_NODE: foreign or already appended text node",
            )));
        }
        let body = self
            .find(local_name!("body"))
            .ok_or_else(|| PageError(Arc::from("EKO_DOM_BODY: document has no HTML body")))?;
        if self.defect == NativePageDefect::HostTextAppendNoop {
            return Ok(());
        }
        self.document.mutate().append_children(body, &[*node]);
        self.detached.remove(node);
        self.document.shell_provider.request_redraw();
        Ok(())
    }
}

pub(crate) struct EkojitScriptEngine {
    pub(crate) host: DomHost,
    budget: Rc<JobBudget>,
    ready_state: Cell<&'static str>,
    receipts: Vec<Arc<NativeReceipt>>,
}

impl EkojitScriptEngine {
    pub(crate) fn new(document: BaseDocument, defect: NativePageDefect) -> Self {
        Self {
            host: DomHost {
                document,
                detached: HashSet::new(),
                diagnostics: RefCell::new(Diagnostics::default()),
                defect,
            },
            budget: Rc::new(JobBudget::default()),
            ready_state: Cell::new("loading"),
            receipts: Vec::new(),
        }
    }

    pub(crate) fn eval_source(
        &mut self,
        source: Arc<PageSource>,
    ) -> Result<Arc<NativeReceipt>, PageError> {
        let result = NativePage::<DomHost>::compile(source, Duration::from_secs(1))
            .and_then(|mut page| page.execute(&mut self.host));
        self.host.sweep();
        match result {
            Ok(receipt) => {
                if self.receipts.len() == 1000 {
                    self.receipts.remove(0);
                }
                self.receipts.push(Arc::clone(&receipt));
                Ok(receipt)
            }
            Err(error) => {
                self.host
                    .diagnostics
                    .borrow_mut()
                    .push(error.to_string(), true);
                Err(error)
            }
        }
    }

    pub(crate) fn receipts(&self) -> &[Arc<NativeReceipt>] {
        &self.receipts
    }
    pub(crate) fn console(&self) -> Vec<DiagnosticEntry> {
        self.host
            .diagnostics
            .borrow()
            .console
            .iter()
            .cloned()
            .collect()
    }
    pub(crate) fn errors(&self) -> Vec<DiagnosticEntry> {
        self.host
            .diagnostics
            .borrow()
            .errors
            .iter()
            .cloned()
            .collect()
    }

    fn reject(&mut self, description: &str, capability: &str) {
        self.host.diagnostics.borrow_mut().push(
            format!("{description}: EKO_PAGE_UNSUPPORTED: {capability}"),
            true,
        );
    }
}

fn stream(queue: &VecDeque<DiagnosticEntry>, after: u64, limit: usize) -> DiagnosticStreamBatch {
    let entries: Vec<_> = queue
        .iter()
        .filter(|entry| entry.sequence > after)
        .take(limit.clamp(1, 1000))
        .cloned()
        .collect();
    let next_sequence = entries.last().map_or(after, |entry| entry.sequence);
    let lost = queue.front().map_or(0, |entry| {
        entry.sequence.saturating_sub(after.saturating_add(1))
    });
    DiagnosticStreamBatch {
        entries,
        lost,
        next_sequence,
    }
}

// The compiler admits no producers for jobs, timers, modules or listeners.
// Empty queue answers below therefore describe the complete admitted subset.
// Attempts to install those producers are rejected at compilation, never run
// through Boa and never retained as unexecutable work.
impl ScriptEngine for EkojitScriptEngine {
    type JobScope = JobSlice;
    fn eval(&mut self, code: &str, description: &str) {
        self.eval_at(code, None, description);
    }
    fn eval_at(&mut self, code: &str, url: Option<&Url>, description: &str) {
        let _result = self.eval_source(Arc::new(PageSource {
            text: Arc::from(code),
            url: Arc::from(url.map_or(description, Url::as_str)),
        }));
    }
    fn eval_internal(&mut self, code: &str, description: &str) {
        self.eval(code, description);
    }
    fn eval_module(&mut self, _code: &str, _url: Option<&Url>, description: &str) {
        self.reject(description, "modules");
    }
    fn eval_json(&mut self, _code: &str, description: &str) -> Result<serde_json::Value, String> {
        self.reject(description, "general evaluation results");
        Err(format!(
            "{description}: EKO_PAGE_UNSUPPORTED: general evaluation results"
        ))
    }
    fn set_import_map(&self, _json: &str, _base_url: Option<&Url>) {
        self.host
            .diagnostics
            .borrow_mut()
            .push("EKO_PAGE_UNSUPPORTED: import maps".into(), true);
    }
    fn poll_module_evaluations(&mut self) {
        assert!(!self.has_pending_module_evaluations());
    }
    fn has_pending_module_evaluations(&self) -> bool {
        false
    }
    fn has_pending_module_loads(&self) -> bool {
        false
    }
    fn begin_job_slice(&self) -> JobSlice {
        self.budget.begin()
    }
    fn job_budget(&self) -> Duration {
        self.budget.duration()
    }
    fn set_job_budget(&self, duration: Duration) {
        self.budget.set_duration(duration);
    }
    fn pending_task_count(&self) -> u64 {
        0
    }
    fn job_slice_exhausted(&self) -> bool {
        self.budget.exhausted()
    }
    fn jobs_remain(&self) -> bool {
        self.pending_task_count() != 0
    }
    fn run_jobs(&mut self, _description: &str) {
        self.host.sweep();
    }
    fn run_jobs_with_progress(&mut self, description: &str) -> bool {
        let had_detached_nodes = !self.host.detached.is_empty();
        self.run_jobs(description);
        had_detached_nodes
    }
    fn next_timer_deadline(&self) -> Option<Instant> {
        None
    }
    fn run_due_timers(&mut self, _profiling: bool) -> bool {
        self.next_timer_deadline().is_some()
    }
    fn dispatch_dom_event(
        &mut self,
        _chain: &[NodeId],
        _event: &DomEvent,
        _state: &mut EventState,
        _profiling: bool,
    ) -> bool {
        false
    }
    fn dispatch_document_event(&mut self, _name: &str) -> bool {
        false
    }
    fn dispatch_node_event(&mut self, _node_id: NodeId, _name: &str) -> bool {
        false
    }
    fn dispatch_window_event(&mut self, _name: &str) -> bool {
        false
    }
    fn set_document_ready_state(&mut self, next: &'static str) -> bool {
        self.ready_state.replace(next) != next
    }
    fn sweep_detached_nodes(&mut self) {
        self.host.sweep();
    }
    fn poll_domc(&mut self) -> bool {
        self.run_jobs_with_progress("native host cleanup")
    }
    fn console_entries_after(&self, after: u64) -> Vec<DiagnosticEntry> {
        self.console()
            .into_iter()
            .filter(|entry| entry.sequence > after)
            .collect()
    }
    fn runtime_errors_after(&self, after: u64) -> Vec<DiagnosticEntry> {
        self.errors()
            .into_iter()
            .filter(|entry| entry.sequence > after)
            .collect()
    }
    fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> DiagnosticBatch {
        let diagnostics = self.host.diagnostics.borrow();
        DiagnosticBatch {
            console: stream(&diagnostics.console, console_after, limit),
            errors: stream(&diagnostics.errors, errors_after, limit),
        }
    }
}

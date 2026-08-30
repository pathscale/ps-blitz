//! [`ScriptDocument`]: a [`Document`] implementation with JavaScript support

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Waker};

use blitz_dom::{
    BaseDocument, DEFAULT_CSS, DocGuard, DocGuardMut, Document, DocumentConfig, EventDriver, NodeId,
};
use blitz_html::{DocumentHtmlParser, HtmlProvider};
use blitz_traits::events::{BlitzPointerEvent, DomEvent, UiEvent};
use url::Url;
use web_time::Instant;

use crate::event_handler::ScriptEventHandler;
use crate::fetch::{DefaultScriptFetcher, ScriptFetcher};
use crate::runtime::ScriptRuntime;

type PollHook =
    Box<dyn for<'a> FnMut(&mut ScriptDocument, Option<&TaskContext<'a>>) -> bool + 'static>;

/// A `<script>` element found in the document
struct PendingScript {
    node_id: NodeId,
    src: Option<String>,
    inline_text: String,
}

/// A [`Document`] which executes the JavaScript contained in the document's
/// `<script>` tags, exposing DOM APIs backed by `blitz-dom` to the scripts.
///
/// Construct with [`ScriptDocument::from_html`], then call
/// [`execute_scripts`](ScriptDocument::execute_scripts) (this also happens
/// automatically on the first [`poll`](Document::poll)). UI events pushed via
/// [`handle_ui_event`](Document::handle_ui_event) are dispatched to JavaScript
/// event listeners before Blitz's default actions run.
pub struct ScriptDocument {
    inner: Rc<RefCell<BaseDocument>>,
    runtime: ScriptRuntime,
    base_url: Option<Url>,
    fetcher: Box<dyn ScriptFetcher>,
    scripts_executed: bool,
    /// Which `<script>` nodes have already run.
    ///
    /// By node rather than a single "done" flag, because scripts keep arriving
    /// after the first pass: a loader that appends its bundle, an analytics
    /// snippet, a lazily inserted chunk. Keyed this way, a rescan can tell a
    /// script it has run from one it has not without running anything twice.
    executed_scripts: std::collections::HashSet<NodeId>,
    poll_hook: Option<PollHook>,

    // Timer wakeups: a background thread which wakes the event loop (via the
    // `Waker` passed to `poll`) when the next JS timer is due.
    waker: Arc<Mutex<Option<Waker>>>,
    timer_thread: Option<Sender<Instant>>,
}

impl ScriptDocument {
    /// Parse HTML into a [`ScriptDocument`].
    ///
    /// Note: this does *not* execute any scripts yet. Call
    /// [`execute_scripts`](Self::execute_scripts) to do so (or rely on the
    /// first `poll` doing it automatically).
    pub fn from_html(html: &str, mut config: DocumentConfig) -> Self {
        if let Some(ss) = &mut config.ua_stylesheets {
            if !ss.iter().any(|s| s == DEFAULT_CSS) {
                ss.push(String::from(DEFAULT_CSS));
            }
        } else {
            config.ua_stylesheets = Some(vec![String::from(DEFAULT_CSS)]);
        }
        if config.html_parser_provider.is_none() {
            config.html_parser_provider = Some(Arc::new(HtmlProvider));
        }

        let base_url = config
            .base_url
            .as_deref()
            .and_then(|url| Url::parse(url).ok());

        let mut doc = BaseDocument::new(config);
        let mut mutr = doc.mutate();
        DocumentHtmlParser::parse_into_mutator(&mut mutr, html);
        drop(mutr);

        let inner = Rc::new(RefCell::new(doc));
        let runtime = ScriptRuntime::new(Rc::clone(&inner), base_url.as_ref());

        Self {
            inner,
            runtime,
            base_url,
            fetcher: Box::new(DefaultScriptFetcher),
            scripts_executed: false,
            executed_scripts: std::collections::HashSet::new(),
            poll_hook: None,
            waker: Arc::new(Mutex::new(None)),
            timer_thread: None,
        }
    }

    /// Override the [`ScriptFetcher`] used to load external (`src="..."`) scripts.
    /// The default fetcher supports `file:` and `data:` URLs.
    pub fn with_fetcher(mut self, fetcher: impl ScriptFetcher) -> Self {
        self.fetcher = Box::new(fetcher);
        self
    }

    /// Install the host callback invoked by `window.ipc.postMessage(body)`.
    ///
    /// The callback may forward work to another thread, but JavaScript and DOM state remain on
    /// the document's owning thread. Replacing the callback is supported before script startup.
    pub fn set_ipc_handler(&mut self, handler: impl Fn(String) + 'static) {
        self.runtime.ctx.state.borrow_mut().ipc_handler = Some(Rc::new(handler));
    }

    /// Install work that an embedder needs to run on the document thread during polling.
    ///
    /// The hook runs after document scripts and due timers. Returning `true` requests a redraw.
    pub fn set_poll_hook(
        &mut self,
        hook: impl for<'a> FnMut(&mut ScriptDocument, Option<&TaskContext<'a>>) -> bool + 'static,
    ) {
        self.poll_hook = Some(Box::new(hook));
    }

    /// Append document-thread work without discarding an embedder's existing
    /// poll lifecycle.
    pub fn add_poll_hook(
        &mut self,
        mut hook: impl for<'a> FnMut(&mut ScriptDocument, Option<&TaskContext<'a>>) -> bool + 'static,
    ) {
        let Some(mut existing) = self.poll_hook.take() else {
            self.poll_hook = Some(Box::new(hook));
            return;
        };
        self.poll_hook = Some(Box::new(move |document, task_context| {
            let ran_existing = existing(document, task_context);
            let ran_added = hook(document, task_context);
            ran_existing | ran_added
        }));
    }

    /// Execute the document's `<script>` elements in document order, then fire
    /// the `DOMContentLoaded` and `load` events.
    ///
    /// Does nothing if scripts have already been executed.
    pub fn execute_scripts(&mut self) {
        let _profiling = self.runtime.ctx.enter_profiling_boundary();
        if self.scripts_executed {
            return;
        }
        self.scripts_executed = true;

        self.run_pending_scripts();

        self.runtime.dispatch_document_event("DOMContentLoaded");
        self.runtime.dispatch_window_event("load");

        self.request_redraw();
        self.arm_timer_thread();
    }

    /// The resolved URLs of the document's external (`<script src="...">`) scripts,
    /// in document order.
    ///
    /// The [`ScriptFetcher`] API is synchronous, so embedders with asynchronous
    /// networking can use this to prefetch script sources before calling
    /// [`execute_scripts`](Self::execute_scripts), and then serve them from memory
    /// via a custom fetcher (see [`with_fetcher`](Self::with_fetcher)).
    pub fn external_script_urls(&self) -> Vec<Url> {
        self.collect_scripts()
            .iter()
            .filter_map(|script| script.src.as_deref())
            .filter_map(|src| self.resolve_script_url(src))
            .collect()
    }

    /// Resolve a script `src` attribute against the document's base URL
    fn resolve_script_url(&self, src: &str) -> Option<Url> {
        match &self.base_url {
            Some(base) => base.join(src).ok(),
            None => Url::parse(src).ok(),
        }
    }

    /// Evaluate arbitrary JavaScript code in the document's script context
    pub fn eval(&mut self, code: &str) {
        let _profiling = self.runtime.ctx.enter_profiling_boundary();
        self.runtime.eval(code, "<eval>");
        self.request_redraw();
        self.arm_timer_thread();
    }

    /// Evaluate JavaScript and convert its result to JSON.
    ///
    /// Embedders use this for APIs that return an evaluation result, such as Tauri's
    /// `eval_script_with_callback`. JavaScript exceptions are recorded in the runtime diagnostics
    /// and returned as an error.
    pub fn eval_json(&mut self, code: &str) -> Result<serde_json::Value, String> {
        let _profiling = self.runtime.ctx.enter_profiling_boundary();
        let result = self.runtime.eval_json(code, "<eval with result>");
        self.request_redraw();
        self.arm_timer_thread();
        result
    }

    #[cfg(feature = "debug-control")]
    pub(crate) fn console_entries_after(
        &self,
        sequence: u64,
    ) -> Vec<crate::runtime::DiagnosticEntry> {
        self.runtime.console_entries_after(sequence)
    }

    #[cfg(feature = "debug-control")]
    pub(crate) fn runtime_errors_after(
        &self,
        sequence: u64,
    ) -> Vec<crate::runtime::DiagnosticEntry> {
        self.runtime.runtime_errors_after(sequence)
    }

    /// Current document URL for automation and diagnostics.
    pub fn current_url(&self) -> Option<&Url> {
        self.base_url.as_ref()
    }

    /// Serialize the current document tree as HTML.
    pub fn page_source(&self) -> String {
        self.inner.borrow().root_element().outer_html()
    }

    /// Dispatch a synthetic DOM event (e.g. a click created with
    /// [`Node::synthetic_click_event`](blitz_dom::Node::synthetic_click_event))
    /// through the document's event driver. The event is exposed to JavaScript
    /// event listeners, and Blitz's default actions run unless prevented.
    pub fn dispatch_dom_event(&mut self, event: DomEvent) {
        let profiling_boundary = self.runtime.ctx.enter_profiling_boundary();
        let profiling = profiling_boundary.enabled();
        let handler = ScriptEventHandler {
            runtime: &mut self.runtime,
            profiling,
        };
        let mut driver = EventDriver::new(&mut self.inner, handler);
        driver.handle_dom_event(event);

        self.request_redraw();
        self.arm_timer_thread();
    }

    /// Run every `<script>` that has appeared since the last pass.
    ///
    /// Called on each poll as well as at startup, because script elements are
    /// not only a parser product: a page can build one and append it, and that
    /// is how most real loaders bring in their bundle. Running only the markup
    /// the parser produced meant those were fetched by nobody and the page
    /// simply never started.
    ///
    /// A `src` script reports `load` when it runs and `error` when it does not.
    /// Loaders wait on those before continuing — nofilter.io keeps the body
    /// hidden until the bundle's `load` arrives — so a script that ran silently
    /// would still leave the page blank.
    fn run_pending_scripts(&mut self) {
        // Collect first, then run: executing a script can append more, and the
        // borrow on the document has to be released before any of them runs.
        let pending: Vec<PendingScript> = self
            .collect_scripts()
            .into_iter()
            .filter(|script| !self.executed_scripts.contains(&script.node_id))
            .collect();

        for script in pending {
            // Marked before running, not after: a script that appends another
            // copy of itself, or that throws, must not be retried on every
            // poll for the life of the page.
            self.executed_scripts.insert(script.node_id);
            match script.src {
                Some(src) => {
                    let Some(url) = self.resolve_script_url(&src) else {
                        eprintln!("blitz-script: could not resolve script URL {src:?}");
                        self.runtime.dispatch_node_event(script.node_id, "error");
                        continue;
                    };
                    match self.fetcher.fetch(&url) {
                        Ok(code) => {
                            self.runtime.eval(&code, url.as_str());
                            self.runtime.dispatch_node_event(script.node_id, "load");
                        }
                        Err(error) => {
                            eprintln!("blitz-script: failed to fetch script {url}: {error}");
                            self.runtime.dispatch_node_event(script.node_id, "error");
                        }
                    }
                }
                None => {
                    if !script.inline_text.trim().is_empty() {
                        self.runtime.eval(&script.inline_text, "<inline script>");
                    }
                }
            }
        }
    }

    /// Find `<script>` elements in document order
    fn collect_scripts(&self) -> Vec<PendingScript> {
        let doc = self.inner.borrow();
        let mut scripts = Vec::new();
        let mut stack = vec![doc.root_node().id];

        while let Some(node_id) = stack.pop() {
            let Some(node) = doc.get_node(node_id) else {
                continue;
            };

            if let Some(element) = node.element_data() {
                if element.name.local == blitz_dom::local_name!("script") {
                    // Skip non-JavaScript script types (e.g. JSON data blocks).
                    // `module` scripts are treated as classic scripts for now.
                    let script_type = element
                        .attr(blitz_dom::local_name!("type"))
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    let is_js = matches!(
                        script_type.as_str(),
                        "" | "text/javascript" | "application/javascript" | "module"
                    );
                    if is_js {
                        scripts.push(PendingScript {
                            node_id,
                            src: element
                                .attr(blitz_dom::local_name!("src"))
                                .map(str::to_string),
                            inline_text: node.text_content(),
                        });
                    }
                    continue;
                }
            }

            stack.extend(node.children.iter().rev().copied());
        }

        scripts
    }

    fn request_redraw(&self) {
        self.inner.borrow().shell_provider.request_redraw();
    }

    /// Ensure the timer thread is armed to wake the event loop when the next
    /// JS timer is due.
    fn arm_timer_thread(&mut self) {
        let Some(deadline) = self.runtime.next_timer_deadline() else {
            return;
        };

        let sender = self.timer_thread.get_or_insert_with(|| {
            let (tx, rx) = channel::<Instant>();
            let waker = Arc::clone(&self.waker);
            std::thread::Builder::new()
                .name("blitz-script-timers".to_string())
                .spawn(move || timer_thread_main(rx, waker))
                .expect("failed to spawn timer thread");
            tx
        });

        // If the thread has exited (channel disconnected) drop the sender so a
        // new thread is spawned next time.
        if sender.send(deadline).is_err() {
            self.timer_thread = None;
        }
    }
}

/// Background thread which wakes the event loop when JS timers are due
fn timer_thread_main(rx: Receiver<Instant>, waker: Arc<Mutex<Option<Waker>>>) {
    let mut deadline: Option<Instant> = None;

    loop {
        match deadline {
            None => match rx.recv() {
                Ok(new_deadline) => deadline = Some(new_deadline),
                Err(_) => return,
            },
            Some(current) => {
                let now = Instant::now();
                if current <= now {
                    if let Some(waker) = waker.lock().unwrap().as_ref() {
                        waker.wake_by_ref();
                    }
                    deadline = None;
                    continue;
                }
                match rx.recv_timeout(current - now) {
                    Ok(new_deadline) => deadline = Some(new_deadline.min(current)),
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some(waker) = waker.lock().unwrap().as_ref() {
                            waker.wake_by_ref();
                        }
                        deadline = None;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

impl Document for ScriptDocument {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(self.inner.borrow())
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(self.inner.borrow_mut())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        let profiling_boundary = self.runtime.ctx.enter_profiling_boundary();
        let profiling = profiling_boundary.enabled();
        let handler = ScriptEventHandler {
            runtime: &mut self.runtime,
            profiling,
        };
        let mut driver = EventDriver::new(&mut self.inner, handler);
        driver.handle_ui_event(event);

        // JS may have mutated the DOM or scheduled timers
        self.request_redraw();
        self.arm_timer_thread();
    }

    fn poll(&mut self, task_context: Option<TaskContext>) -> bool {
        let profiling_boundary = self.runtime.ctx.enter_profiling_boundary();
        let profiling = profiling_boundary.enabled();
        let poll_started = profiling.then(std::time::Instant::now);
        let ran = self.poll_inner(task_context, profiling);
        if let Some(started) = poll_started {
            crate::script_stats::record_poll(started.elapsed(), ran);
        }
        ran
    }
}

impl ScriptDocument {
    /// Move a semantic automation pointer to the exact resolved DOM node.
    ///
    /// Normal window input remains coordinate hit-tested through
    /// [`Document::handle_ui_event`]. Debug-control callers already selected a
    /// node by id, so routing those coordinates through hit testing again can
    /// silently select an overlapping descendant or retained overlay.
    pub fn handle_pointer_move_to_node(&mut self, event: BlitzPointerEvent, node_id: NodeId) {
        let profiling_boundary = self.runtime.ctx.enter_profiling_boundary();
        let profiling = profiling_boundary.enabled();
        let handler = ScriptEventHandler {
            runtime: &mut self.runtime,
            profiling,
        };
        let mut driver = EventDriver::new(&mut self.inner, handler);
        driver.handle_pointer_move_to_node(&event, node_id);

        self.request_redraw();
        self.arm_timer_thread();
    }

    /// The real poll. Split out so every exit path is timed by the wrapper
    /// above rather than by a stopwatch threaded through each early return.
    fn poll_inner(&mut self, task_context: Option<TaskContext>, profiling: bool) -> bool {
        // Store the waker so the timer thread can wake the event loop
        if let Some(cx) = &task_context {
            let mut waker = self.waker.lock().unwrap();
            let stale = waker
                .as_ref()
                .map(|old| !old.will_wake(cx.waker()))
                .unwrap_or(true);
            if stale {
                *waker = Some(cx.waker().clone());
            }
        }

        // A scripted document may itself be an embedder. Chuzz's Solid chrome
        // is one: its `<web-view>` elements own the page documents. Poll those
        // children at the same outer boundary so their timers, resource
        // completions, and script work continue to make progress.
        let subdocument_changes = self
            .inner
            .borrow_mut()
            .poll_subdocuments(task_context.as_ref().map(TaskContext::waker));

        // Execute scripts on first poll if they haven't been run explicitly
        let mut ran = subdocument_changes;
        if !self.scripts_executed {
            // One-time: parsing and running the application bundle. Separated
            // because it is startup cost, and folding it into the steady-state
            // numbers made every per-poll average meaningless.
            let started = profiling.then(std::time::Instant::now);
            self.execute_scripts();
            if let Some(started) = started {
                crate::script_stats::record_work("startup:execute_scripts", started.elapsed());
            }
            ran = true;
        } else {
            // Steady state: pick up any script the page appended since the last
            // turn. Cheap when there are none, and it is the only path by which
            // a runtime-injected bundle ever runs.
            self.run_pending_scripts();
        }

        ran |= self.runtime.run_due_timers(profiling);

        if let Some(mut hook) = self.poll_hook.take() {
            // The embedder's per-poll work. For a Solid application this is
            // where reactive updates and DOM mutation actually happen, so it is
            // the bucket that matters once startup is excluded.
            let started = profiling.then(std::time::Instant::now);
            ran |= hook(self, task_context.as_ref());
            if let Some(started) = started {
                crate::script_stats::record_work("poll_hook", started.elapsed());
            }
            self.poll_hook = Some(hook);
        }

        // Reclaim what removal could not judge at the time. A node is detached
        // while script may still hold its wrapper, and the answer only becomes
        // knowable once the collector has run; this is the point where asking
        // is cheap and the answer is current.
        self.runtime.sweep_detached_nodes();

        self.arm_timer_thread();
        ran
    }
}

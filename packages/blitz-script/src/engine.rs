//! Execution control seam for script backends.

use std::ops::{Deref, DerefMut};

use blitz_dom::NodeId;
use blitz_traits::events::{DomEvent, EventState};
use url::Url;
use web_time::{Duration, Instant};

pub use crate::job_budget::{JobBudget, JobSlice};
#[cfg(feature = "boa")]
use crate::runtime::BoaScriptEngine;
use crate::{DiagnosticBatch, DiagnosticEntry};

/// Operations used by the document and event driver to enter script execution.
///
/// JobScope is an owned guard, so a checkpoint can share the outer budget
/// without retaining a borrow of the engine during a host callback.
pub trait ScriptEngine {
    type JobScope;
    fn eval(&mut self, code: &str, description: &str);
    fn eval_at(&mut self, code: &str, url: Option<&Url>, description: &str);
    fn eval_internal(&mut self, code: &str, description: &str);
    fn eval_module(&mut self, code: &str, url: Option<&Url>, description: &str);
    fn eval_json(&mut self, code: &str, description: &str) -> Result<serde_json::Value, String>;
    fn set_import_map(&self, json: &str, base_url: Option<&Url>);
    fn poll_module_evaluations(&mut self);
    fn has_pending_module_evaluations(&self) -> bool;
    fn has_pending_module_loads(&self) -> bool;
    fn begin_job_slice(&self) -> Self::JobScope;
    fn job_budget(&self) -> Duration;
    fn set_job_budget(&self, duration: Duration);
    fn pending_task_count(&self) -> u64;
    fn job_slice_exhausted(&self) -> bool;
    fn jobs_remain(&self) -> bool;
    fn run_jobs(&mut self, description: &str);
    fn run_jobs_with_progress(&mut self, description: &str) -> bool;
    fn next_timer_deadline(&self) -> Option<Instant>;
    fn run_due_timers(&mut self, profiling: bool) -> bool;
    fn dispatch_dom_event(
        &mut self,
        chain: &[NodeId],
        event: &DomEvent,
        event_state: &mut EventState,
        profiling: bool,
    ) -> bool;
    fn dispatch_document_event(&mut self, name: &str) -> bool;
    fn dispatch_node_event(&mut self, node_id: NodeId, name: &str) -> bool;
    fn dispatch_window_event(&mut self, name: &str) -> bool;
    fn set_document_ready_state(&mut self, next: &'static str) -> bool;
    fn sweep_detached_nodes(&mut self);
    fn poll_domc(&mut self) -> bool;
    fn console_entries_after(&self, sequence: u64) -> Vec<DiagnosticEntry>;
    fn runtime_errors_after(&self, sequence: u64) -> Vec<DiagnosticEntry>;
    fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> DiagnosticBatch;
}

/// Statically dispatched owner of a script backend.
///
/// Deref is the phase-zero bridge for the existing DOM bootstrap and public
/// Boa context callback. New execution entry points belong on ScriptEngine.
/// Backend-specific document bootstrap uses the explicit backend accessors.
pub struct Engine<E: ScriptEngine> {
    backend: E,
}

impl<E: ScriptEngine> Engine<E> {
    pub fn new(backend: E) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &E {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut E {
        &mut self.backend
    }

    pub fn eval(&mut self, code: &str, description: &str) {
        self.backend.eval(code, description)
    }

    pub fn eval_at(&mut self, code: &str, url: Option<&Url>, description: &str) {
        self.backend.eval_at(code, url, description)
    }

    pub fn eval_internal(&mut self, code: &str, description: &str) {
        self.backend.eval_internal(code, description)
    }

    pub fn eval_module(&mut self, code: &str, url: Option<&Url>, description: &str) {
        self.backend.eval_module(code, url, description)
    }

    pub fn eval_json(
        &mut self,
        code: &str,
        description: &str,
    ) -> Result<serde_json::Value, String> {
        self.backend.eval_json(code, description)
    }

    pub fn set_import_map(&self, json: &str, base_url: Option<&Url>) {
        self.backend.set_import_map(json, base_url)
    }

    pub fn poll_module_evaluations(&mut self) {
        self.backend.poll_module_evaluations()
    }

    pub fn has_pending_module_evaluations(&self) -> bool {
        self.backend.has_pending_module_evaluations()
    }

    pub fn has_pending_module_loads(&self) -> bool {
        self.backend.has_pending_module_loads()
    }

    pub fn begin_job_slice(&self) -> E::JobScope {
        self.backend.begin_job_slice()
    }

    pub fn job_budget(&self) -> Duration {
        self.backend.job_budget()
    }

    pub fn set_job_budget(&self, duration: Duration) {
        self.backend.set_job_budget(duration)
    }

    pub fn pending_task_count(&self) -> u64 {
        self.backend.pending_task_count()
    }

    pub fn job_slice_exhausted(&self) -> bool {
        self.backend.job_slice_exhausted()
    }

    pub fn jobs_remain(&self) -> bool {
        self.backend.jobs_remain()
    }

    pub fn run_jobs(&mut self, description: &str) {
        self.backend.run_jobs(description)
    }

    pub fn run_jobs_with_progress(&mut self, description: &str) -> bool {
        self.backend.run_jobs_with_progress(description)
    }

    pub fn next_timer_deadline(&self) -> Option<Instant> {
        self.backend.next_timer_deadline()
    }

    pub fn run_due_timers(&mut self, profiling: bool) -> bool {
        self.backend.run_due_timers(profiling)
    }

    pub fn dispatch_dom_event(
        &mut self,
        chain: &[NodeId],
        event: &DomEvent,
        event_state: &mut EventState,
        profiling: bool,
    ) -> bool {
        self.backend
            .dispatch_dom_event(chain, event, event_state, profiling)
    }

    pub fn dispatch_document_event(&mut self, name: &str) -> bool {
        self.backend.dispatch_document_event(name)
    }

    pub fn dispatch_node_event(&mut self, node_id: NodeId, name: &str) -> bool {
        self.backend.dispatch_node_event(node_id, name)
    }

    pub fn dispatch_window_event(&mut self, name: &str) -> bool {
        self.backend.dispatch_window_event(name)
    }

    pub fn set_document_ready_state(&mut self, next: &'static str) -> bool {
        self.backend.set_document_ready_state(next)
    }

    pub fn sweep_detached_nodes(&mut self) {
        self.backend.sweep_detached_nodes()
    }

    pub fn poll_domc(&mut self) -> bool {
        self.backend.poll_domc()
    }

    pub fn console_entries_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        self.backend.console_entries_after(sequence)
    }

    pub fn runtime_errors_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        self.backend.runtime_errors_after(sequence)
    }

    pub fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> DiagnosticBatch {
        self.backend
            .diagnostic_batch(console_after, errors_after, limit)
    }
}

impl<E: ScriptEngine> Deref for Engine<E> {
    type Target = E;

    fn deref(&self) -> &E {
        &self.backend
    }
}

impl<E: ScriptEngine> DerefMut for Engine<E> {
    fn deref_mut(&mut self) -> &mut E {
        &mut self.backend
    }
}

#[cfg(feature = "boa")]
impl ScriptEngine for BoaScriptEngine {
    type JobScope = JobSlice;

    fn eval(&mut self, code: &str, description: &str) {
        BoaScriptEngine::eval(self, code, description)
    }

    fn eval_at(&mut self, code: &str, url: Option<&Url>, description: &str) {
        BoaScriptEngine::eval_at(self, code, url, description)
    }

    fn eval_internal(&mut self, code: &str, description: &str) {
        BoaScriptEngine::eval_internal(self, code, description)
    }

    fn eval_module(&mut self, code: &str, url: Option<&Url>, description: &str) {
        BoaScriptEngine::eval_module(self, code, url, description)
    }

    fn eval_json(&mut self, code: &str, description: &str) -> Result<serde_json::Value, String> {
        BoaScriptEngine::eval_json(self, code, description)
    }

    fn set_import_map(&self, json: &str, base_url: Option<&Url>) {
        BoaScriptEngine::set_import_map(self, json, base_url)
    }

    fn poll_module_evaluations(&mut self) {
        BoaScriptEngine::poll_module_evaluations(self)
    }

    fn has_pending_module_evaluations(&self) -> bool {
        BoaScriptEngine::has_pending_module_evaluations(self)
    }

    fn has_pending_module_loads(&self) -> bool {
        BoaScriptEngine::has_pending_module_loads(self)
    }

    fn begin_job_slice(&self) -> Self::JobScope {
        BoaScriptEngine::begin_job_slice(self)
    }

    fn job_budget(&self) -> Duration {
        BoaScriptEngine::job_budget(self)
    }

    fn set_job_budget(&self, duration: Duration) {
        BoaScriptEngine::set_job_budget(self, duration)
    }

    fn pending_task_count(&self) -> u64 {
        BoaScriptEngine::pending_task_count(self)
    }

    fn job_slice_exhausted(&self) -> bool {
        BoaScriptEngine::job_slice_exhausted(self)
    }

    fn jobs_remain(&self) -> bool {
        BoaScriptEngine::jobs_remain(self)
    }

    fn run_jobs(&mut self, description: &str) {
        BoaScriptEngine::run_jobs(self, description)
    }

    fn run_jobs_with_progress(&mut self, description: &str) -> bool {
        BoaScriptEngine::run_jobs_with_progress(self, description)
    }

    fn next_timer_deadline(&self) -> Option<Instant> {
        BoaScriptEngine::next_timer_deadline(self)
    }

    fn run_due_timers(&mut self, profiling: bool) -> bool {
        BoaScriptEngine::run_due_timers(self, profiling)
    }

    fn dispatch_dom_event(
        &mut self,
        chain: &[NodeId],
        event: &DomEvent,
        event_state: &mut EventState,
        profiling: bool,
    ) -> bool {
        BoaScriptEngine::dispatch_dom_event(self, chain, event, event_state, profiling)
    }

    fn dispatch_document_event(&mut self, name: &str) -> bool {
        BoaScriptEngine::dispatch_document_event(self, name)
    }

    fn dispatch_node_event(&mut self, node_id: NodeId, name: &str) -> bool {
        BoaScriptEngine::dispatch_node_event(self, node_id, name)
    }

    fn dispatch_window_event(&mut self, name: &str) -> bool {
        BoaScriptEngine::dispatch_window_event(self, name)
    }

    fn set_document_ready_state(&mut self, next: &'static str) -> bool {
        BoaScriptEngine::set_document_ready_state(self, next)
    }

    fn sweep_detached_nodes(&mut self) {
        BoaScriptEngine::sweep_detached_nodes(self)
    }

    fn poll_domc(&mut self) -> bool {
        BoaScriptEngine::poll_domc(self)
    }

    fn console_entries_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        BoaScriptEngine::console_entries_after(self, sequence)
    }

    fn runtime_errors_after(&self, sequence: u64) -> Vec<DiagnosticEntry> {
        BoaScriptEngine::runtime_errors_after(self, sequence)
    }

    fn diagnostic_batch(
        &self,
        console_after: u64,
        errors_after: u64,
        limit: usize,
    ) -> DiagnosticBatch {
        BoaScriptEngine::diagnostic_batch(self, console_after, errors_after, limit)
    }
}


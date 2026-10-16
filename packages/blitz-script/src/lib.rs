//! Script integration for Blitz.
//!
//! This crate defines the contract for script-capable document backends and
//! shared facilities used by those backends. A backend implements
//! [`ScriptDocumentBackend`] to construct a document from HTML and execute its
//! scripts. The document participates in Blitz through the
//! [`Document`](blitz_dom::Document) interface.
//!
//! Runtime-specific execution stays in the backend implementation. This crate
//! provides a common document wrapper and shared diagnostics, fetching, job
//! budgets, and script statistics.

#![allow(clippy::collapsible_if)]

mod application_document;
#[cfg(feature = "debug-control")]
mod debug_control;
mod diagnostics;
mod document_scripts;
pub mod engine;
mod fetch;
mod job_budget;
pub mod native_document;
pub mod native_host;
pub mod runtime_driver;
pub mod script_stats;

pub use application_document::{
    ApplicationDocument, NativeHostDocumentBackend, ScriptDocumentBackend,
};
#[cfg(feature = "debug-control")]
pub use debug_control::{DebugController, DebugDocument};
pub use diagnostics::{DiagnosticBatch, DiagnosticEntry, DiagnosticStreamBatch};
pub use document_scripts::{ScriptKind, external_script_urls};
pub use fetch::{DefaultScriptFetcher, FetchError, ScriptFetcher};
pub use native_document::{
    BoundScriptDocument, DocumentHostExtension, DocumentPollHook, NativeDocumentRuntime,
    TypedNativeGuest,
};
pub use runtime_driver::RuntimeDriver;

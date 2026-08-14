//! Runtime-agnostic DOM operations over [`blitz_dom`].
//!
//! `blitz-script` is currently the only path from a language runtime to the
//! DOM, and its operations are entangled with Boa: `JsValue` in, `JsResult`
//! out, a `Context` threaded through, and prototype objects holding the
//! registration. A second runtime cannot reuse any of it. This crate is those
//! operations with the runtime removed, so that a binding does argument
//! coercion and result construction and nothing else.
//!
//! Nothing here depends on Boa, and nothing here may. See `tests/no_boa.rs`.
//!
//! # Shape of the API
//!
//! Every operation is a free function taking the document first, then the node
//! the operation is *on* (what a binding would call `this`), then the
//! arguments in the order the DOM method declares them:
//!
//! ```ignore
//! let child = document::create_element(&mut doc, "div")?;
//! node::append_child(&mut doc, parent, child)?;
//! element::set_attribute(&mut doc, child, "class", "panel")?;
//! ```
//!
//! Readers take `&BaseDocument`, mutators take `&mut BaseDocument`. Every
//! operation returns `Result<T, DomError>`, including the ones that cannot
//! fail today, so that a caller written against this API keeps compiling when
//! one of them grows a failure case.
//!
//! # Borrow discipline
//!
//! **No return value keeps a document borrow alive.** Readers that would
//! naturally hand back `&str` (`text_content`, `inner_html`, `get_attribute`,
//! `css_text`, `get_property_value`) return owned `String` instead. That is a
//! deliberate allocation.
//!
//! The reason is re-entrancy. A binding calls out to guest code between
//! operations, and guest code calls back in; if a borrow were still live
//! across that boundary the next mutation would panic inside `RefCell`, at a
//! call site with no relationship to the code that took the borrow. Paying an
//! allocation on every read makes that class of failure unrepresentable. A
//! borrow taken *inside* an operation may span engine-internal work freely; it
//! just may not outlive the call.
//!
//! # What this crate deliberately does not do
//!
//! - **It does not mark layout dirty and it does not request a redraw.**
//!   `blitz-script` routes mutations through `DomCtx::mutate_doc`, which sets
//!   a dirty flag and asks the shell for a frame. Both are properties of the
//!   embedding, not of the operation, so both stay with the binding. A binding
//!   that forgets gets stale geometry reads, which is the exact bug that flag
//!   exists to prevent.
//! - **It does not flush layout.** [`geometry::bounding_client_rect`] reads
//!   whatever layout currently holds. See that function's documentation.
//! - **It does not upgrade custom elements.** `blitz-script` runs
//!   `upgrade_if_defined` after an insertion, which constructs a guest object.
//!   That is a runtime operation.
//! - **It has no events.** Event objects, dispatch, listener registration,
//!   selection, pointer capture and focus are all out of scope: they need a
//!   dispatch model that belongs with the runtime binding. See README.md.
//!
//! # Interning
//!
//! [`atom::Interner`] and [`atom::AtomId`] exist for a guest that cannot
//! cheaply pass strings across its boundary. The binding owns an interner,
//! resolves an incoming `AtomId` to a `&str`, and calls the operation. The
//! resolution happens one level up rather than inside every operation, which
//! is what keeps the signatures here from having to change when a guest with
//! that constraint arrives. See [`atom`] for the ownership rule.

pub mod atom;
pub mod character_data;
pub mod document;
pub mod element;
pub mod error;
pub mod geometry;
pub mod node;
pub mod style;

#[cfg(test)]
mod test_support;

pub use atom::{AtomId, Interner};
pub use error::DomError;
pub use geometry::Rect;

/// Re-exported so a caller does not have to depend on `blitz-dom` directly
/// just to name the id type an operation takes.
pub use blitz_dom::NodeId;

/// Shorthand for the result type every operation returns.
pub type Result<T> = std::result::Result<T, DomError>;

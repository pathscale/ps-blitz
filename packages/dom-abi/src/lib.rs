//! The contracts between a WebAssembly guest and a DOM host.
//!
//! Types, enums and constants. There is no parser here, no DOM, no engine and
//! no emitter; if one arrives, it arrived in the wrong crate.
//!
//! # Why this is a separate crate
//!
//! ```text
//! dom-abi
//!    ├── solid-layouts-oxc          writes templates   (pulls in oxc, napi)
//!    ├── ui-templates               reads them         (pulls in the engine)
//!    ├── blitz-wasm                 host ABI constants
//!    └── blitz-wasm/guest/bindings  same constants, other side
//! ```
//!
//! Put these types in the compiler and a browser engine depends on a
//! JavaScript parser. Put them in the runtime and the compiler depends on the
//! engine. Neither dependency is one anybody would defend; both are what you
//! get by default if the shared vocabulary lives in either place. So it lives
//! in neither. This is the same shape as [`blitz_dom_api`], for the third time,
//! and the shape is load-bearing rather than stylistic.
//!
//! [`blitz_dom_api`]: https://docs.rs/blitz-dom-api
//!
//! # Three modules, three independent versions
//!
//! | Module | Constant | Covers |
//! | --- | --- | --- |
//! | [`template`] | [`template::TEMPLATE_FORMAT_VERSION`] | what a compiler writes and a runtime reads |
//! | [`host`] | [`host::HOST_ABI_VERSION`] | the calling convention across the boundary |
//! | [`runtime`] | [`runtime::RUNTIME_ABI_VERSION`] | how a module declares what it links against |
//!
//! **They are independent, and that is the reason there are three of them.** A
//! calling convention can change without invalidating a single cached
//! template: the templates did not change, so making them look as though they
//! had would throw away every artifact in every cache to describe a change that
//! did not touch them. The converse holds too — a new node variant is not a
//! reason to re-check a linkage that still works.
//!
//! Independence is not a claim, it is a property that has to be maintained: no
//! module may consult another module's version constant, and
//! `tests/versions.rs` reads the source to assert that none does.
//!
//! # The four rules
//!
//! Each rule lives in a doc comment beside the thing it constrains, because a
//! rule in a README is a rule somebody has to remember and a rule on the type
//! is one they trip over. They are collected here only as an index.
//!
//! 1. **No computation.** A binding is an opaque slot. There is no expression
//!    type and there must never be one. See [`template::BindingId`].
//! 2. **Stable ids, not positions.** Templates ship separately from their
//!    guests. See [`template::BindingId`] and [`host::RowId`].
//! 3. **Additive evolution only.** Add variants; never reorder meaning, never
//!    reuse a removed variant's name. See [`template::TEMPLATE_FORMAT_VERSION`].
//! 4. **Nothing data-derived becomes an atom.** Atoms are never released. See
//!    [`host::Atom`] and [`template::Atom`].
//!
//! # What these types do not do
//!
//! **They specify shape, not coherence.** [`template::Show`] carries a
//! [`template::BindingId`], and nothing about the type says that id occurs in
//! the template's binding table. A [`template::ContentHash`] is checked for
//! being a hash and not for naming a component that exists. A well-typed
//! template can be nonsense.
//!
//! So both sides validate, and the runtime validates *independently* rather
//! than trusting that the compiler already did. A template can arrive from a
//! CDN. It did not necessarily come from your compiler, and even when it did,
//! it came from whichever version of your compiler was current when it was
//! cached.
//!
//! # Encoding
//!
//! RON is the encoding these types are written in today. It is not the format.
//! Nothing here may depend on the output being text, so moving to a binary
//! encoding is a serde attribute change in the consumers rather than a second
//! implementation of anything. That is also why the content hash is defined
//! over a structural traversal rather than over encoded bytes; see
//! [`template::canonical`].
//!
//! # Module layout
//!
//! The three modules are re-exported as modules and their contents are *not*
//! flattened into the crate root. [`template::Atom`] and [`host::Atom`] are the
//! same concept in its unresolved and resolved forms, and the module prefix is
//! what keeps the difference visible at the use site instead of leaving it to a
//! reader's memory.

#![forbid(unsafe_code)]

pub mod host;
pub mod runtime;
pub mod template;

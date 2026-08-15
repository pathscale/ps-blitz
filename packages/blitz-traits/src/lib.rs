//! Types and traits to enable interoperability between the other Blitz crates without
//! circular or unnecessary dependencies.

pub mod devtools;
pub mod events;
pub mod navigation;
pub mod net;
pub mod node_id;
pub mod platform;
pub mod profiling;
pub mod shell;

pub use node_id::NodeId;
pub use smol_str::SmolStr;

/// Re-exported because the public fields of [`events::BlitzKeyEvent`] and
/// [`events::BlitzPointerEvent`] are built from these types.
///
/// Without this, anything that wants to synthesise a [`events::UiEvent`] has to
/// take its own dependency on `keyboard-types` and pin it to whatever version
/// this crate happens to resolve. That is not a version a consumer can discover
/// from the API, and getting it wrong produces a type mismatch rather than a
/// version error. Re-exporting makes the event API constructible from the crate
/// that defines it.
pub use keyboard_types;

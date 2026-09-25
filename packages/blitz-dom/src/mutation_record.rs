//! Changes to the document, recorded for a script's `MutationObserver`.
//!
//! Recording happens in [`crate::DocumentMutator`], because every change
//! reaches the tree through it: a script's `appendChild`, `innerHTML` and
//! `setAttribute`, the parser, and the embedder alike. Hooking the script
//! bindings instead would miss whichever path a binding does not own.
//!
//! Two rules keep the log honest and cheap:
//!
//! - It is recorded only while the embedder has switched it on
//!   ([`crate::BaseDocument::set_recording_mutations`]), which a script runtime
//!   does while at least one observer is registered. A page with no observers
//!   pays one branch per change.
//! - Only changes to a node that is in the document are recorded. Building a
//!   detached fragment and then inserting it therefore yields one record for
//!   the insertion, which is what a browser reports, rather than one per node
//!   the fragment was built from. The cost is that an observer watching a
//!   detached node directly hears nothing, which no framework relies on.

use crate::NodeId;

/// One change, in the shape a `MutationRecord` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomMutation {
    /// Children were added to or removed from `target`.
    ChildList {
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    },
    /// An attribute of `target` was set or removed. `old_value` is `None` when
    /// the attribute did not exist before.
    Attributes {
        target: NodeId,
        name: String,
        old_value: Option<String>,
    },
    /// The text of a text or comment node changed.
    CharacterData { target: NodeId, old_value: String },
}

/// The most records held between deliveries. A page that rewrites a large
/// tree while observed without ever yielding to its job queue would otherwise
/// grow this without bound; past the cap, further changes in that turn are
/// dropped rather than stored.
pub(crate) const MUTATION_LOG_CAP: usize = 100_000;

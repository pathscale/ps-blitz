//! The error type every operation returns.

use blitz_dom::NodeId;

use crate::atom::AtomId;

/// Why a DOM operation could not be performed.
///
/// Deliberately narrow. Most of `blitz-script`'s operations are *tolerant*
/// where the DOM specification is strict: reading an attribute off a node that
/// is not an element yields "absent" rather than throwing, and removing a
/// child that is not a child removes it from wherever it actually is. Those
/// behaviours are copied rather than corrected, so they do not appear here.
/// What does appear is every case where `blitz-script` itself raises, plus the
/// tree invariants it asserts with `expect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomError {
    /// The node id does not name a node in this document.
    ///
    /// Only produced where `blitz-script` would panic rather than return.
    NodeNotFound(NodeId),
    /// The document has no root element, so there is nothing to answer with.
    NoRootElement,
    /// A selector string could not be parsed.
    ///
    /// `blitz-script` swallows this: `querySelector` returns `null`, `matches`
    /// returns `false`, `closest` searches an empty match set. This crate
    /// surfaces it instead, and the reparenting diff for each of those call
    /// sites is a trailing `.ok().flatten()` or `.unwrap_or_default()`.
    InvalidSelector(String),
    /// A `classList` token was empty or contained ASCII whitespace.
    ///
    /// `blitz-script` raises a `SyntaxError` here; this is the same case.
    InvalidClassToken(String),
    /// An [`AtomId`] was resolved against an interner that did not mint it.
    UnknownAtom(AtomId),
    /// A structural invariant the operation relies on did not hold.
    ///
    /// `compare_document_position` is the only current source: it walks to a
    /// common ancestor and then indexes into it, and `blitz-script` asserts
    /// both steps with `expect`. A facade must not take the process down over
    /// a malformed tree, so the assertion becomes this.
    TreeInvariant(&'static str),
}

impl std::fmt::Display for DomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeNotFound(id) => write!(f, "no node with id {id:?} in this document"),
            Self::NoRootElement => write!(f, "document has no root element"),
            Self::InvalidSelector(selector) => write!(f, "could not parse selector `{selector}`"),
            Self::InvalidClassToken(token) => write!(
                f,
                "classList token `{token}` must be non-empty and contain no ASCII whitespace"
            ),
            Self::UnknownAtom(atom) => write!(f, "{atom:?} was not produced by this interner"),
            Self::TreeInvariant(what) => write!(f, "document tree invariant violated: {what}"),
        }
    }
}

impl std::error::Error for DomError {}

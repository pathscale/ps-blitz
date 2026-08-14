//! String interning for names that cross a guest boundary.
//!
//! Nothing in this crate requires an [`AtomId`] yet. It exists now because the
//! next binding hosts a guest that cannot cheaply pass strings, and the names
//! it will pass most are the ones with the smallest alphabets: tag names,
//! attribute names, and class values.
//!
//! # Ownership rule
//!
//! An [`Interner`] is owned by the binding, one per document. An [`AtomId`] is
//! only meaningful against the interner that produced it; resolving one
//! against any other interner is [`DomError::UnknownAtom`] at best and a
//! silently wrong name at worst. Do not store an `AtomId` alongside a document
//! without also storing which interner it belongs to.
//!
//! # Why the operations still take `&str`
//!
//! The binding resolves at its own boundary and calls the operation with the
//! resolved string:
//!
//! ```
//! # use blitz_dom_api::atom::Interner;
//! let mut names = Interner::new();
//! let class = names.intern("panel");
//! // ... the guest hands back `class` some time later ...
//! let name = names.resolve(class).unwrap();
//! assert_eq!(name, "panel");
//! ```
//!
//! Threading the interner through every operation instead would put a second
//! parameter on the whole API to serve one caller, and would still not remove
//! the resolve, only move it. Resolving one level up keeps these signatures
//! stable for whichever guest arrives, which is the property that mattered.

use std::collections::HashMap;

use crate::error::DomError;

/// A name interned by an [`Interner`].
///
/// Opaque, cheap to copy, and meaningless on its own. See the module
/// documentation for the ownership rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AtomId(u32);

impl AtomId {
    /// The raw index, for a binding that has to send it across a boundary.
    #[inline]
    pub fn to_u32(self) -> u32 {
        self.0
    }

    /// Rebuild an id from a raw index received back across a boundary.
    ///
    /// Not validated here: [`Interner::resolve`] is what rejects an index the
    /// interner never issued.
    #[inline]
    pub fn from_u32(raw: u32) -> Self {
        Self(raw)
    }
}

/// A set of interned names, owned by the binding.
#[derive(Debug, Default, Clone)]
pub struct Interner {
    strings: Vec<String>,
    index: HashMap<String, AtomId>,
}

impl Interner {
    /// An empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a name, returning the existing id if it is already known.
    pub fn intern(&mut self, name: &str) -> AtomId {
        if let Some(id) = self.index.get(name) {
            return *id;
        }
        let id =
            AtomId(u32::try_from(self.strings.len()).expect("more than u32::MAX interned names"));
        self.strings.push(name.to_owned());
        self.index.insert(name.to_owned(), id);
        id
    }

    /// The id for a name, if it has been interned. Does not intern.
    pub fn get(&self, name: &str) -> Option<AtomId> {
        self.index.get(name).copied()
    }

    /// The name behind an id.
    ///
    /// Errors if this interner did not mint the id, which is the only defence
    /// against an id that crossed a guest boundary and came back wrong.
    pub fn resolve(&self, atom: AtomId) -> Result<&str, DomError> {
        self.strings
            .get(atom.0 as usize)
            .map(String::as_str)
            .ok_or(DomError::UnknownAtom(atom))
    }

    /// How many distinct names have been interned.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether nothing has been interned yet.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_the_same_name_twice_gives_one_id() {
        let mut names = Interner::new();
        let first = names.intern("div");
        let second = names.intern("div");
        assert_eq!(first, second);
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn distinct_names_get_distinct_ids_and_resolve_back() {
        let mut names = Interner::new();
        let div = names.intern("div");
        let span = names.intern("span");
        assert_ne!(div, span);
        assert_eq!(names.resolve(div).unwrap(), "div");
        assert_eq!(names.resolve(span).unwrap(), "span");
    }

    #[test]
    fn get_does_not_intern() {
        let mut names = Interner::new();
        assert_eq!(names.get("div"), None);
        assert!(names.is_empty());
        names.intern("div");
        assert!(names.get("div").is_some());
    }

    /// The ownership rule, as a test: an id from one interner is not valid
    /// against another, even when both hold the same names in a different
    /// order.
    #[test]
    fn an_id_from_another_interner_is_rejected_or_wrong() {
        let mut a = Interner::new();
        let mut b = Interner::new();
        a.intern("div");
        let a_span = a.intern("span");

        assert_eq!(b.resolve(a_span), Err(DomError::UnknownAtom(a_span)));

        b.intern("span");
        b.intern("div");
        // Same names, opposite order: resolvable, and wrong. This is why the
        // rule is "one interner per document", not "ids are portable".
        assert_eq!(b.resolve(a_span).unwrap(), "div");
    }
}

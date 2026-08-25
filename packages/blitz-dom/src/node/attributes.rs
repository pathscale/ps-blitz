use std::ops::{Deref, DerefMut};

use markup5ever::QualName;

/// An attribute's value, interned so identical values are stored once.
///
/// This was a plain `String`, which meant one separate heap allocation per
/// attribute per element with no sharing between them. A census over the
/// application's own transcript markup
/// (`blitz-tests/tests/attribute_value_duplication.rs`) found **777 attribute
/// values of which 54 were distinct**: 14.4x duplication, and 91.3% of the
/// value bytes were a copy of a string already in the tree. One `class` string
/// appeared 24 times. That is what a Tailwind UI looks like in memory, and it
/// is the shape Blink shares through `ElementDataCache` for the same reason
/// (`element_data.h:172`, "very common for many elements to have duplicate
/// sets of attributes (ex. the same classes)").
///
/// `Atom` is the right tool and was already in the dependency graph, because
/// `QualName` above is built from it. It is 8 bytes against `String`'s 24,
/// stores up to 7 bytes inline with no heap allocation at all, and interns
/// anything longer in a refcounted global table with per-bucket locks rather
/// than one global one.
///
/// The trade is a hash and a possible lock acquisition per *write*, against a
/// heap allocation and a memcpy per write today, and equality becoming a
/// pointer comparison rather than a memcmp. Reads are unaffected: this derefs
/// to `str`, so every `&attr.value`, `.as_str()`, `.parse()` and `==` call
/// site continues to compile and mean the same thing.
///
/// `Atom` is generic over a set of strings interned at compile time. We have
/// none to pre-intern: attribute *names* are already atoms via `QualName`, and
/// values are arbitrary author strings, so every one of ours takes the dynamic
/// path. `EmptyStaticAtomSet` is the crate's own declaration of that case.
///
/// Named `AttrAtom` rather than the more obvious `AttrValue`, because stylo
/// already exports an `AttrValue` enum that `document.rs` uses in the same
/// breath as this type. Two different things under one name in one file is how
/// a later reader loses an afternoon.
pub type AttrAtom = string_cache::Atom<string_cache::EmptyStaticAtomSet>;

/// A tag attribute, e.g. `class="test"` in `<div class="test" ...>`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub struct Attribute {
    /// The name of the attribute (e.g. the `class` in `<div class="test">`)
    pub name: QualName,
    /// The value of the attribute (e.g. the `"test"` in `<div class="test">`)
    pub value: AttrAtom,
}

#[derive(Clone, Debug)]
pub struct Attributes {
    inner: Vec<Attribute>,
}

impl Attributes {
    pub fn new(inner: Vec<Attribute>) -> Self {
        Self { inner }
    }

    pub fn get(&mut self, name: &QualName) -> Option<&Attribute> {
        self.inner.iter().find(|attr| attr.name == *name)
    }

    /// Set `name` to `value`, replacing any existing value.
    ///
    /// This used to `clear()` and `push_str()` into the existing `String`,
    /// reusing its allocation. An interned value cannot be edited in place, so
    /// it is replaced instead. That is not the regression it looks like: the
    /// old path still memcpy'd the bytes and only avoided the allocation when
    /// the new value happened to fit the old capacity, whereas interning
    /// usually finds the string already present and takes a refcount. A
    /// re-set to the value it already holds is now free, which is the common
    /// case when a framework rewrites `class` with an unchanged string.
    pub fn set(&mut self, name: QualName, value: &str) {
        let existing_attr = self.inner.iter_mut().find(|a| a.name == name);
        if let Some(existing_attr) = existing_attr {
            existing_attr.value = AttrAtom::from(value);
        } else {
            self.push(Attribute {
                name: name.clone(),
                value: AttrAtom::from(value),
            });
        }
    }

    pub fn remove(&mut self, name: &QualName) -> Option<Attribute> {
        let idx = self.inner.iter().position(|attr| attr.name == *name);
        idx.map(|idx| self.inner.remove(idx))
    }
}

impl Deref for Attributes {
    type Target = Vec<Attribute>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl DerefMut for Attributes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

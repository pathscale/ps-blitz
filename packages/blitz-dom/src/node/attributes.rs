use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, RwLock, Weak};

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
/// sets of attributes").
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

/// Identity and retained contents of an attribute exposed as an Attr.
///
/// Elements hold weak handles. A retained Attr owns its handle and continues
/// to hold its last value after removal. Cloning an element creates new
/// attribute identities.
#[derive(Debug)]
pub struct AttributeNode {
    pub name: QualName,
    pub value: AttrAtom,
    pub attached: bool,
}

#[derive(Debug)]
pub struct Attributes {
    inner: Vec<Attribute>,
    handles: Mutex<Vec<Weak<RwLock<AttributeNode>>>>,
}

impl Clone for Attributes {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        for handle in self.handles.get_mut().unwrap().iter().filter_map(Weak::upgrade) {
            handle.write().unwrap().attached = false;
        }
    }
}

fn same_expanded_name(left: &QualName, right: &QualName) -> bool {
    left.ns == right.ns && left.local == right.local
}

impl Attributes {
    pub fn new(inner: Vec<Attribute>) -> Self {
        Self {
            inner,
            handles: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&mut self, name: &QualName) -> Option<&Attribute> {
        self.inner
            .iter()
            .find(|attr| same_expanded_name(&attr.name, name))
    }

    /// Get the stable identity of an existing attribute.
    pub fn attribute_node(&self, name: &QualName) -> Option<Arc<RwLock<AttributeNode>>> {
        let attr = self
            .inner
            .iter()
            .find(|attr| same_expanded_name(&attr.name, name))?;
        let mut handles = self.handles.lock().unwrap();
        handles.retain(|handle| handle.strong_count() != 0);
        for handle in handles.iter().filter_map(Weak::upgrade) {
            let matches = {
                let node = handle.read().unwrap();
                node.attached && same_expanded_name(&node.name, &attr.name)
            };
            if matches {
                return Some(handle);
            }
        }
        let handle = Arc::new(RwLock::new(AttributeNode {
            name: attr.name.clone(),
            value: attr.value.clone(),
            attached: true,
        }));
        handles.push(Arc::downgrade(&handle));
        Some(handle)
    }

    /// Detach an identity without removing the backing attribute.
    ///
    /// setAttributeNode uses this before replacing the value, so the returned
    /// old Attr retains the old value rather than the replacement's value.
    pub fn detach_attribute_node(&self, name: &QualName) {
        self.handles.lock().unwrap().retain(|weak| {
            let Some(handle) = weak.upgrade() else {
                return false;
            };
            let mut node = handle.write().unwrap();
            if same_expanded_name(&node.name, name) {
                node.attached = false;
                false
            } else {
                true
            }
        });
    }

    /// Bind the identity supplied to setAttributeNode after the mutation.
    pub fn bind_attribute_node(&self, handle: &Arc<RwLock<AttributeNode>>) {
        let name = handle.read().unwrap().name.clone();
        self.detach_attribute_node(&name);
        handle.write().unwrap().attached = true;
        self.handles.lock().unwrap().push(Arc::downgrade(handle));
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
        let existing_attr = self
            .inner
            .iter_mut()
            .find(|attr| same_expanded_name(&attr.name, &name));
        let value = AttrAtom::from(value);
        if let Some(existing_attr) = existing_attr {
            existing_attr.name = name.clone();
            existing_attr.value = value.clone();
        } else {
            self.inner.push(Attribute {
                name: name.clone(),
                value: value.clone(),
            });
        }
        self.handles.get_mut().unwrap().retain(|weak| {
            let Some(handle) = weak.upgrade() else {
                return false;
            };
            let mut node = handle.write().unwrap();
            if node.attached && same_expanded_name(&node.name, &name) {
                node.name = name.clone();
                node.value = value.clone();
            }
            true
        });
    }

    pub fn remove(&mut self, name: &QualName) -> Option<Attribute> {
        let idx = self
            .inner
            .iter()
            .position(|attr| same_expanded_name(&attr.name, name))?;
        self.detach_attribute_node(name);
        Some(self.inner.remove(idx))
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


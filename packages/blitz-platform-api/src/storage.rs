//! The storage provider this crate can supply without depending on anything:
//! an in-memory one.
//!
//! A persistent provider is the embedder's, because persistence is a policy
//! question this crate has no standing to answer. Where the profile directory
//! is, what the quota is, what happens when the store is corrupt on load: all
//! of those belong to whatever is embedding Blitz. See the README.

use std::collections::HashMap;
use std::sync::Mutex;

use blitz_traits::platform::{OriginKey, StorageError, StorageProvider};

/// Origin-scoped key/value storage that lives and dies with the process.
///
/// Correct, just not durable. It is the right provider for a test, for a
/// private-browsing mode, and for an embedder that has not wired a real one
/// yet, and it is what the storage semantics are asserted against.
///
/// **Note what it is not: a fallback that silently loses data.** chuzz's
/// current JavaScript shim is an in-memory `localStorage` that a page cannot
/// distinguish from a real one, so a site's settings vanish on reload with no
/// diagnostic. That is a property of the shim rather than of in-memory storage,
/// but an embedder choosing this deliberately should still know it is choosing
/// it.
#[derive(Default)]
pub struct MemoryStorage {
    /// Keyed by origin first, so `clear` is one removal rather than a scan, and
    /// so the isolation is visible in the type rather than living in a key
    /// format everything has to agree about.
    origins: Mutex<HashMap<String, HashMap<String, String>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many origins hold anything. For tests that want to assert isolation
    /// rather than infer it.
    pub fn origin_count(&self) -> usize {
        self.origins.lock().unwrap().len()
    }
}

impl StorageProvider for MemoryStorage {
    fn get(&self, origin: &OriginKey, key: &str) -> Option<String> {
        self.origins
            .lock()
            .unwrap()
            .get(origin.as_str())?
            .get(key)
            .cloned()
    }

    fn set(&self, origin: &OriginKey, key: &str, value: &str) -> Result<(), StorageError> {
        self.origins
            .lock()
            .unwrap()
            .entry(origin.as_str().to_owned())
            .or_default()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove(&self, origin: &OriginKey, key: &str) {
        if let Some(entries) = self.origins.lock().unwrap().get_mut(origin.as_str()) {
            entries.remove(key);
        }
    }

    fn clear(&self, origin: &OriginKey) {
        self.origins.lock().unwrap().remove(origin.as_str());
    }
}

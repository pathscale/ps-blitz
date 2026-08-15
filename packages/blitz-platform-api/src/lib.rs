//! Runtime-agnostic host platform APIs: `fetch`, and per-origin storage.
//!
//! The third crate on the same split that produced `blitz-dom-api`. The DOM
//! facade took DOM operations and removed the runtime; this takes the platform
//! APIs and removes both the runtime *and* the transport, so what is left is
//! the part every binding would otherwise write again.
//!
//! ```text
//!   blitz-traits         FetchProvider, StorageProvider, OriginKey
//!         |                  the embedder implements these
//!   blitz-platform-api   this crate: origin scoping, the in-flight table,
//!         |              the completion queue, the counters
//!   blitz-wasm           binds it for a WebAssembly guest
//!   blitz-script         can bind the same thing for JavaScript
//! ```
//!
//! # Why this is not inside `blitz-wasm`
//!
//! Because JavaScript has no `fetch()` either. `blitz-script`'s [`fetch`
//! module][script-fetch] is synchronous `<script src>` loading and nothing
//! else, and chuzz's `WEB_API_SHIM` supplies an in-memory `localStorage` and a
//! deliberately non-conformant `URL`. Written inside the wasm binding, all of
//! this would have to be written a second time for Boa. Written here, Boa's
//! binding is argument coercion over the same host.
//!
//! [script-fetch]: ../../blitz-script/src/fetch.rs
//!
//! # What is *not* here
//!
//! No HTTP. This crate never opens a socket, parses a header off the wire, or
//! names a client. `blitz-net` already ships `reqwest` with HTTP/2, cookies,
//! compression and a cacache disk cache, and it implements
//! [`FetchProvider`](blitz_traits::platform::FetchProvider) over that same
//! client. Anything here that looked like HTTP logic would be a second, worse
//! implementation of it.
//!
//! No runtime. Nothing here is `async`, nothing spawns, and nothing blocks.
//! [`PlatformHost::start_fetch`] hands the request to the provider and returns
//! an id; the provider answers on whatever thread it likes; the answer waits in
//! a queue until an embedder drains it.
//!
//! `tests/no_client_or_engine.rs` asserts both against the resolved dependency
//! graph, in the manner of `blitz-dom-api`'s `no_boa.rs`.
//!
//! # Borrow discipline, and why fetch completes the way a click dispatches
//!
//! **A completed fetch is delivered by draining a queue, never by a callback
//! that reaches into a document.**
//!
//! `blitz-wasm` already learned this on the event path: calling a guest from
//! inside `EventHandler::handle_event` would run guest code while the
//! `EventDriver` holds the document, and the guest's first act is to mutate the
//! DOM. Its answer was to queue listener ids during propagation and call the
//! guest afterwards, with the borrow gone.
//!
//! Fetch has the identical hazard from the other direction: a response arrives
//! on a network thread at a moment nothing knows about. So it takes the
//! identical answer, and this crate is built so the wrong version does not
//! compile. Nothing here can reach a document, because nothing here has ever
//! been given one: [`PlatformHost`] holds an origin, two providers and a table.
//! The completion handler holds a [`Weak`](std::sync::Weak) reference to that
//! table and nothing else.
//!
//! # Origin scoping
//!
//! **A [`PlatformHost`] is built for one origin and holds it for life.** Every
//! storage call on it is scoped to that origin, and there is no method that
//! takes an origin as an argument. A binding therefore cannot pass the wrong
//! one, in the same way `blitz-wasm`'s event handler cannot reach the guest.
//!
//! See [`OriginKey`](blitz_traits::platform::OriginKey) for why `file:` and
//! `data:` documents each get their own opaque origin rather than sharing one
//! bucket.

pub mod counters;
pub mod fetch;
pub mod storage;

pub use counters::PlatformCounters;
pub use fetch::{FetchState, RequestId};
pub use storage::MemoryStorage;

use std::sync::{Arc, Mutex, Weak};

use blitz_traits::platform::{
    FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse, OriginKey, StorageError,
    StorageProvider,
};

use crate::fetch::{InFlight, Slot};

/// Called when a fetch completes, so an embedder knows to drain the queue.
///
/// Without one, a response lands in the queue and nothing asks for it until
/// something else happens to wake the event loop, which in a GUI means "until
/// the user moves the mouse". That is not a slow fetch, it is a hung one.
pub type ReadyWaker = Arc<dyn Fn() + Send + Sync + 'static>;

/// The platform APIs available to one document, at one origin.
///
/// Construct with [`PlatformHost::new`], hold it for the life of the document,
/// and drain it with [`take_ready`](PlatformHost::take_ready).
pub struct PlatformHost {
    origin: OriginKey,
    fetch_provider: Arc<dyn FetchProvider>,
    storage_provider: Arc<dyn StorageProvider>,
    /// `Arc` because completion handlers hold `Weak`s to it, and those outlive
    /// this host whenever a request is still in flight at teardown.
    inflight: Arc<Mutex<InFlight>>,
    waker: Option<ReadyWaker>,
    counters: Mutex<PlatformCounters>,
}

impl PlatformHost {
    pub fn new(
        origin: OriginKey,
        fetch_provider: Arc<dyn FetchProvider>,
        storage_provider: Arc<dyn StorageProvider>,
    ) -> Self {
        Self {
            origin,
            fetch_provider,
            storage_provider,
            inflight: Arc::new(Mutex::new(InFlight::default())),
            waker: None,
            counters: Mutex::new(PlatformCounters::default()),
        }
    }

    /// Install the callback that says "a response is ready to drain".
    ///
    /// It runs on whatever thread the provider answered on, so it must do the
    /// minimum: set a flag, or ask a window for a frame. It must not take the
    /// document, and it must not call back into this host.
    pub fn with_waker(mut self, waker: ReadyWaker) -> Self {
        self.waker = Some(waker);
        self
    }

    /// The origin every storage call on this host is scoped to.
    pub fn origin(&self) -> &OriginKey {
        &self.origin
    }

    pub fn counters(&self) -> PlatformCounters {
        *self.counters.lock().unwrap()
    }

    // -- fetch ------------------------------------------------------------

    /// Begin a request. Returns immediately.
    ///
    /// The returned id names the request until [`release`](Self::release) is
    /// called on it. It is *not* a promise and it does not carry a result: the
    /// result arrives in the ready queue, which is the only place a completion
    /// is ever observed.
    pub fn start_fetch(&self, request: FetchRequest) -> RequestId {
        let sent = request.body.as_ref().map(|body| body.len()).unwrap_or(0);

        let id = {
            let mut inflight = self.inflight.lock().unwrap();
            inflight.begin()
        };

        {
            let mut counters = self.counters.lock().unwrap();
            counters.fetches_started += 1;
            counters.fetch_bytes_sent += sent as u64;
        }

        self.fetch_provider.fetch(
            request,
            Box::new(Completion {
                id,
                inflight: Arc::downgrade(&self.inflight),
                waker: self.waker.clone(),
            }),
        );

        id
    }

    /// Every request that has completed since the last call, and no others.
    ///
    /// **Call this with no document borrow held.** The whole point of the queue
    /// is that a caller chooses the moment, and the moment must be one where it
    /// is safe to run guest code. Each id remains valid, and its response
    /// readable, until [`release`](Self::release).
    pub fn take_ready(&self) -> Vec<RequestId> {
        let ready = {
            let mut inflight = self.inflight.lock().unwrap();
            std::mem::take(&mut inflight.ready)
        };

        if !ready.is_empty() {
            let received: u64 = ready
                .iter()
                .filter_map(|id| self.with_response(*id, |response| response.body.len() as u64))
                .sum();
            let mut counters = self.counters.lock().unwrap();
            counters.fetches_completed += ready.len() as u64;
            counters.fetch_bytes_received += received;
        }

        ready
    }

    /// What happened to a request.
    pub fn state(&self, id: RequestId) -> FetchState {
        let inflight = self.inflight.lock().unwrap();
        match inflight.slots.get(&id) {
            None => FetchState::Unknown,
            Some(Slot::Pending) => FetchState::Pending,
            Some(Slot::Done(answer)) => match answer.as_ref() {
                Ok(_) => FetchState::Response,
                Err(error) => FetchState::Failed(error.clone()),
            },
        }
    }

    /// Read something out of a completed response without cloning it.
    ///
    /// A closure rather than a returned reference because the response lives
    /// behind a lock, and handing a `&FetchResponse` out would mean handing the
    /// guard out with it. The `Option` is `None` when the id is unknown or the
    /// request did not produce a response.
    pub fn with_response<T>(
        &self,
        id: RequestId,
        read: impl FnOnce(&FetchResponse) -> T,
    ) -> Option<T> {
        let inflight = self.inflight.lock().unwrap();
        match inflight.slots.get(&id) {
            Some(Slot::Done(answer)) => answer.as_ref().as_ref().ok().map(read),
            _ => None,
        }
    }

    /// Forget a request.
    ///
    /// Valid at any point, including while still pending: the completion
    /// handler holds only an id and finds nothing to write into, so a late
    /// answer is dropped rather than resurrecting the entry. That is what makes
    /// tearing down a document with requests in flight safe.
    pub fn release(&self, id: RequestId) -> bool {
        let mut inflight = self.inflight.lock().unwrap();
        inflight.ready.retain(|ready| *ready != id);
        inflight.slots.remove(&id).is_some()
    }

    /// How many requests are known to this host, pending or completed.
    pub fn tracked_requests(&self) -> usize {
        self.inflight.lock().unwrap().slots.len()
    }

    // -- storage ----------------------------------------------------------
    //
    // No method here takes an origin. See the crate docs: the host holds one
    // and a binding has no way to name another.

    pub fn storage_get(&self, key: &str) -> Option<String> {
        let value = self.storage_provider.get(&self.origin, key);
        let mut counters = self.counters.lock().unwrap();
        counters.storage_reads += 1;
        counters.storage_bytes_read += value.as_ref().map(|v| v.len()).unwrap_or(0) as u64;
        value
    }

    pub fn storage_set(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let result = self.storage_provider.set(&self.origin, key, value);
        if result.is_ok() {
            let mut counters = self.counters.lock().unwrap();
            counters.storage_writes += 1;
            counters.storage_bytes_written += (key.len() + value.len()) as u64;
        }
        result
    }

    pub fn storage_remove(&self, key: &str) {
        self.storage_provider.remove(&self.origin, key);
        self.counters.lock().unwrap().storage_writes += 1;
    }

    pub fn storage_clear(&self) {
        self.storage_provider.clear(&self.origin);
        self.counters.lock().unwrap().storage_writes += 1;
    }
}

/// The handler handed to the provider for one request.
///
/// Holds an id, a `Weak` to the table, and a waker. **It cannot reach the
/// document, the guest, or the host**, because it was never given any of them.
/// Same technique as `blitz-wasm`'s `WasmEventHandler`, which is constructed
/// with three field references and therefore has no path to a guest export
/// whatever it intends.
struct Completion {
    id: RequestId,
    inflight: Weak<Mutex<InFlight>>,
    waker: Option<ReadyWaker>,
}

impl FetchHandler for Completion {
    fn complete(self: Box<Self>, result: Result<FetchResponse, FetchError>) {
        let Some(inflight) = self.inflight.upgrade() else {
            // The host is gone. Dropping the response is the whole answer.
            return;
        };

        {
            let mut inflight = inflight.lock().unwrap();
            // `get_mut` rather than `insert`: a released request must stay
            // released. Re-inserting here would resurrect an entry the caller
            // has already forgotten, and it would never be drained.
            let Some(slot) = inflight.slots.get_mut(&self.id) else {
                return;
            };
            *slot = Slot::Done(Box::new(result));
            inflight.ready.push(self.id);
        }

        if let Some(waker) = &self.waker {
            waker();
        }
    }
}

#[cfg(test)]
mod tests;

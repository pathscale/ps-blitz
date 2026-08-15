//! What the platform APIs moved, counted where this crate can see it.
//!
//! # These are not boundary bytes, and the distinction matters
//!
//! `blitz-wasm`'s [`Counters`] count bytes crossing the *guest* boundary: read
//! out of, or written into, wasm linear memory. The numbers here count bytes
//! crossing the *network and storage* boundary: what a request body carried,
//! what a response body brought back, what a storage value weighed.
//!
//! For one fetch they are usually close and never guaranteed equal. A guest
//! that starts a request and never reads the body moved 40 KB here and zero
//! there. A guest that reads the same body twice moved 40 KB here and 80 KB
//! there. Adding them would produce a number answering no question, which is
//! why they are separate types in separate crates rather than more fields on
//! one struct.
//!
//! Both exist because the brief asks for fetch bytes to be attributable
//! separately from DOM bytes in both directions. This half answers "what did
//! the platform move"; the binding's half answers "what did that cost at the
//! boundary".
//!
//! # No timing
//!
//! Same reason `blitz-wasm` gives: a duration measured on one machine, in one
//! build profile, is not evidence. A byte count is the same everywhere. A fetch
//! duration is additionally dominated by the network, which is the one part of
//! the system no design decision here changes.
//!
//! [`Counters`]: ../../blitz-wasm/src/counters.rs

/// Everything one [`PlatformHost`](crate::PlatformHost) has moved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlatformCounters {
    /// Requests handed to the provider.
    pub fetches_started: u64,
    /// Requests drained from the ready queue.
    ///
    /// Counts failures as well as responses: a request that could not be
    /// completed still completed, in the sense the queue means. The gap between
    /// this and `fetches_started` is the number still in flight, plus any
    /// released before they were drained.
    pub fetches_completed: u64,
    /// Request-body bytes handed to the provider.
    ///
    /// Counted at [`start_fetch`](crate::PlatformHost::start_fetch), so it
    /// counts what was submitted rather than what reached a server. A request
    /// that failed to connect still counted its body here, which is correct for
    /// the question "what did the guest ask us to send".
    pub fetch_bytes_sent: u64,
    /// Response-body bytes, counted once per request when it is drained.
    ///
    /// Once, not once per read: a guest reading the same body twice has moved
    /// twice the bytes across its own boundary, and that is the binding's
    /// counter to keep. Headers are not counted, because a provider may
    /// synthesise them (`data:` URLs) and a compressed transfer never carried
    /// the bytes the header map now holds.
    pub fetch_bytes_received: u64,

    pub storage_reads: u64,
    /// Every call that could change the store: `set`, `remove`, `clear`.
    ///
    /// A `remove` of an absent key and a `clear` of an empty origin are counted
    /// even though nothing changed, because this crate does not ask the
    /// provider whether anything did. It is a count of calls, not of edits.
    pub storage_writes: u64,
    /// Value bytes returned by `get`. A miss adds nothing.
    pub storage_bytes_read: u64,
    /// Key plus value bytes accepted by `set`. A rejected write adds nothing.
    pub storage_bytes_written: u64,
}

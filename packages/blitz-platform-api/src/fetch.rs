//! The in-flight table: what a request id names, and what states it passes
//! through.

use std::collections::HashMap;

use blitz_traits::platform::{FetchError, FetchResponse};

/// Names one request for as long as its host tracks it.
///
/// **Never reused**, for the reason `blitz-wasm` gives for handles and listener
/// ids: a stale id must be an error rather than a silent hit on whatever took
/// its place. A guest that releases a request and then reads it gets
/// [`FetchState::Unknown`], not another request's body.
///
/// A `u64` here. A binding that has to return this to a guest through a
/// negative-is-error `i32` has a narrower range to work with, and capping it is
/// that binding's business, not this crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Where a request has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchState {
    /// No request by that id. Either it never existed or it was released.
    Unknown,
    /// Started, not yet answered.
    Pending,
    /// A response arrived and is readable.
    ///
    /// **Including a 404.** A response is a response whatever its status; see
    /// [`FetchHandler::complete`](blitz_traits::platform::FetchHandler::complete).
    Response,
    /// No response will arrive.
    Failed(FetchError),
}

/// What the table holds per id.
///
/// The answer is boxed because a [`FetchResponse`] carries a `Url`, a
/// `HeaderMap` and a `Bytes`, and without the box every *pending* entry would
/// reserve room for all three. A page that starts a hundred requests holds a
/// hundred of these, and the whole point of the table is that pending is the
/// cheap state.
pub(crate) enum Slot {
    Pending,
    Done(Box<Result<FetchResponse, FetchError>>),
}

/// Every request one host knows about.
///
/// A `HashMap` rather than a `Vec` indexed by id, because ids are never reused:
/// a vector would keep growing across a long-lived document even as requests
/// are released.
#[derive(Default)]
pub(crate) struct InFlight {
    pub(crate) slots: HashMap<RequestId, Slot>,
    pub(crate) ready: Vec<RequestId>,
    next: u64,
}

impl InFlight {
    /// Allocate an id and mark it pending.
    pub(crate) fn begin(&mut self) -> RequestId {
        let id = RequestId(self.next);
        self.next += 1;
        self.slots.insert(id, Slot::Pending);
        id
    }
}

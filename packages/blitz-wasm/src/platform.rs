//! The platform imports: `fetch` and storage, bound over
//! [`blitz_platform_api::PlatformHost`].
//!
//! Everything here is the same conventions the DOM imports use, taken from
//! [`dom_abi::host`] rather than restated: negative is failure except
//! [`ABSENT`](Status::ABSENT), a reader returns the value's full byte length
//! and writes only if it fits, and nothing traps on a guest mistake.
//!
//! # Why this is a separate module and not more of `lib.rs`
//!
//! Because none of it needs the document. A platform import validates a
//! request id, talks to a `PlatformHost`, and copies bytes; it never reaches
//! `BaseDocument`. Registering these against a trait bound rather than against
//! the concrete [`Host`](crate::Host) makes that structural: the closures below
//! *cannot* touch a document, because the type they are given does not offer
//! one.
//!
//! # Reads: mechanism (b), unchanged
//!
//! [`OutBuffer`] and [`ReadOutcome`] come from `dom-abi`, so a guest reading a
//! response body and a guest reading an attribute use one protocol and one set
//! of edge cases. See [`OutBuffer`]'s documentation for why the guest supplies
//! the buffer rather than the host allocating one: the host-allocates variant
//! would call `alloc` *into* the guest from inside a host function, which is
//! the one thing this binding is built to prevent.
//!
//! # Completion: the event path's shape, reused exactly
//!
//! A response arrives on a network thread. It is put in a queue and nothing
//! else happens. Later, with no document borrow live, an embedder calls
//! [`dispatch_fetch_completions`], which drains the queue and calls the guest
//! once per completed request.
//!
//! That is `dispatch_dom_event`'s design, and it is reused rather than
//! reinvented because the hazard is the same one: guest code must not run while
//! the document is borrowed, and a guest's first act on a completed fetch is to
//! put the result in the DOM.

use blitz_platform_api::{FetchState, PlatformHost, RequestId as PlatformRequestId};
use blitz_traits::platform::{Bytes, FetchRequest, Method, Url};
use dom_abi::host::{MAX_ID, OutBuffer, Status};
pub use dom_abi::platform::RequestId;
use std::collections::HashMap;
use wasmi::{Caller, Extern, Instance, Linker, Store};

use crate::MODULE;

/// What a store's data must offer for the platform imports to be registered.
///
/// A trait rather than the concrete [`Host`](crate::Host) so that the closures
/// below have no path to a document, and so an embedder with its own store type
/// can bind these without owning a `BaseDocument` at all.
pub trait HasPlatform: 'static {
    /// The platform host, if one was installed.
    fn platform(&self) -> Option<&PlatformHost>;
    /// The binding's own request table and counters.
    fn platform_state(&mut self) -> &mut PlatformState;
}

/// Which platform import a counter update belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformOp {
    FetchNew,
    FetchHeader,
    FetchBody,
    FetchSend,
    FetchStatus,
    FetchReadBody,
    FetchReadHeader,
    FetchReadUrl,
    FetchRelease,
    StorageGet,
    StorageSet,
    StorageRemove,
    StorageClear,
    /// The outbound one: a host call into the guest's `fetch_complete` export.
    FetchComplete,
}

/// Counters for one platform import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlatformOpCounters {
    pub calls: u64,
    /// Bytes read **out of** guest linear memory by this operation.
    pub bytes_in: u64,
    /// Bytes written **into** guest linear memory by this operation.
    ///
    /// Counts bytes actually written. A read whose buffer was too small wrote
    /// nothing and adds nothing here, even though it returned a length, which
    /// is what makes the retry visible in the numbers: a guest that always
    /// guesses too small shows two calls and one write.
    pub bytes_out: u64,
}

/// Every platform counter for one instance.
///
/// # Separate from [`Counters`](crate::Counters), deliberately
///
/// The brief asks for fetch bytes to be attributable separately from DOM bytes
/// in both directions, and two structs is the way to make that impossible to
/// get wrong. A single struct with more fields would have a `total_bytes`
/// method that silently answered a different question the day a response body
/// landed in it: 40 KB of JSON and a 3-byte text update are not the same
/// measurement, and the existing counters document at length why a true number
/// telling a false story is the failure mode worth designing against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlatformCounters {
    pub fetch_new: PlatformOpCounters,
    pub fetch_header: PlatformOpCounters,
    pub fetch_body: PlatformOpCounters,
    pub fetch_send: PlatformOpCounters,
    pub fetch_status: PlatformOpCounters,
    pub fetch_read_body: PlatformOpCounters,
    pub fetch_read_header: PlatformOpCounters,
    pub fetch_read_url: PlatformOpCounters,
    pub fetch_release: PlatformOpCounters,
    pub storage_get: PlatformOpCounters,
    pub storage_set: PlatformOpCounters,
    pub storage_remove: PlatformOpCounters,
    pub storage_clear: PlatformOpCounters,
    /// Calls the host made into the guest's `fetch_complete` export. Its byte
    /// counts are structurally zero: the argument is a request id, and there is
    /// no pointer in the signature for anything else to travel through.
    pub fetch_complete: PlatformOpCounters,
    /// The last failing status returned to the guest, or `None`.
    ///
    /// [`ABSENT`](Status::ABSENT) is not recorded. A guest polling for an
    /// optional storage key would otherwise leave this permanently set to
    /// something that never went wrong.
    pub last_error: Option<Status>,
    /// The last negative status the guest's `fetch_complete` returned.
    pub last_guest_status: Option<i32>,
}

impl PlatformCounters {
    fn slot(&mut self, op: PlatformOp) -> &mut PlatformOpCounters {
        match op {
            PlatformOp::FetchNew => &mut self.fetch_new,
            PlatformOp::FetchHeader => &mut self.fetch_header,
            PlatformOp::FetchBody => &mut self.fetch_body,
            PlatformOp::FetchSend => &mut self.fetch_send,
            PlatformOp::FetchStatus => &mut self.fetch_status,
            PlatformOp::FetchReadBody => &mut self.fetch_read_body,
            PlatformOp::FetchReadHeader => &mut self.fetch_read_header,
            PlatformOp::FetchReadUrl => &mut self.fetch_read_url,
            PlatformOp::FetchRelease => &mut self.fetch_release,
            PlatformOp::StorageGet => &mut self.storage_get,
            PlatformOp::StorageSet => &mut self.storage_set,
            PlatformOp::StorageRemove => &mut self.storage_remove,
            PlatformOp::StorageClear => &mut self.storage_clear,
            PlatformOp::FetchComplete => &mut self.fetch_complete,
        }
    }

    /// Every call the guest made into the host. `fetch_complete` is excluded,
    /// because it goes the other way.
    pub fn total_calls(&self) -> u64 {
        [
            self.fetch_new,
            self.fetch_header,
            self.fetch_body,
            self.fetch_send,
            self.fetch_status,
            self.fetch_read_body,
            self.fetch_read_header,
            self.fetch_read_url,
            self.fetch_release,
            self.storage_get,
            self.storage_set,
            self.storage_remove,
            self.storage_clear,
        ]
        .iter()
        .map(|op| op.calls)
        .sum()
    }

    /// Bytes the guest sent across the boundary for platform calls.
    pub fn total_bytes_in(&self) -> u64 {
        self.every_op().map(|op| op.bytes_in).sum()
    }

    /// Bytes the host wrote into guest memory for platform calls.
    ///
    /// The number the DOM counters have no equivalent for, because until
    /// readers existed nothing travelled this way.
    pub fn total_bytes_out(&self) -> u64 {
        self.every_op().map(|op| op.bytes_out).sum()
    }

    /// Bytes moved for `fetch` alone, in either direction.
    pub fn fetch_bytes(&self) -> u64 {
        [
            self.fetch_new,
            self.fetch_header,
            self.fetch_body,
            self.fetch_send,
            self.fetch_status,
            self.fetch_read_body,
            self.fetch_read_header,
            self.fetch_read_url,
            self.fetch_release,
            self.fetch_complete,
        ]
        .iter()
        .map(|op| op.bytes_in + op.bytes_out)
        .sum()
    }

    /// Bytes moved for storage alone, in either direction.
    pub fn storage_bytes(&self) -> u64 {
        [
            self.storage_get,
            self.storage_set,
            self.storage_remove,
            self.storage_clear,
        ]
        .iter()
        .map(|op| op.bytes_in + op.bytes_out)
        .sum()
    }

    fn every_op(&self) -> impl Iterator<Item = &PlatformOpCounters> {
        [
            &self.fetch_new,
            &self.fetch_header,
            &self.fetch_body,
            &self.fetch_send,
            &self.fetch_status,
            &self.fetch_read_body,
            &self.fetch_read_header,
            &self.fetch_read_url,
            &self.fetch_release,
            &self.storage_get,
            &self.storage_set,
            &self.storage_remove,
            &self.storage_clear,
            &self.fetch_complete,
        ]
        .into_iter()
    }
}

/// A request the guest is still building, or one that has been sent.
///
/// Two id spaces meet here, and they are not the same thing.
/// [`RequestId`](dom_abi::platform::RequestId) is what the *guest* holds, from
/// `fetch_new` to `fetch_release`. [`PlatformRequestId`] is what
/// `blitz-platform-api` issues, and only at send time. Handing the second one
/// straight through would mean the id a guest holds changed halfway through the
/// request's life, so this table maps between them.
enum Request {
    /// Built but not sent. Mutable by `fetch_header` and `fetch_body`.
    ///
    /// Boxed so a *sent* request, which is the state most entries spend their
    /// life in, does not reserve room for a URL, a header map and a body it no
    /// longer owns.
    Draft(Box<FetchRequest>),
    /// Sent. The platform host owns the outcome from here.
    Sent(PlatformRequestId),
}

/// The binding's own state: which requests exist, and what they cost.
#[derive(Default)]
pub struct PlatformState {
    requests: HashMap<RequestId, Request>,
    /// Maps the platform host's ids back to the guest's, for completion.
    sent: HashMap<PlatformRequestId, RequestId>,
    next: u32,
    counters: PlatformCounters,
}

impl PlatformState {
    pub fn counters(&self) -> &PlatformCounters {
        &self.counters
    }

    /// How many requests the guest currently holds an id for.
    pub fn live_requests(&self) -> usize {
        self.requests.len()
    }

    fn record_call(&mut self, op: PlatformOp) {
        self.counters.slot(op).calls += 1;
    }

    fn record_in(&mut self, op: PlatformOp, bytes: usize) {
        self.counters.slot(op).bytes_in += bytes as u64;
    }

    fn record_out(&mut self, op: PlatformOp, bytes: usize) {
        self.counters.slot(op).bytes_out += bytes as u64;
    }

    /// Record and return a failing status.
    ///
    /// [`Status::ABSENT`] is deliberately not recorded; see
    /// [`PlatformCounters::last_error`].
    fn fail(&mut self, status: Status) -> i32 {
        if status.is_failure() {
            self.counters.last_error = Some(status);
        }
        status.raw()
    }

    fn new_request(&mut self, request: FetchRequest) -> Result<RequestId, Status> {
        if self.next >= MAX_ID {
            return Err(Status::ERR_TOO_MANY_REQUESTS);
        }
        let id = RequestId(self.next);
        self.next += 1;
        self.requests.insert(id, Request::Draft(Box::new(request)));
        Ok(id)
    }

    fn draft_mut(&mut self, id: RequestId) -> Result<&mut FetchRequest, Status> {
        match self.requests.get_mut(&id) {
            Some(Request::Draft(request)) => Ok(request.as_mut()),
            Some(Request::Sent(_)) => Err(Status::ERR_ALREADY_SENT),
            None => Err(Status::ERR_BAD_REQUEST),
        }
    }

    fn platform_id(&self, id: RequestId) -> Result<PlatformRequestId, Status> {
        match self.requests.get(&id) {
            Some(Request::Sent(platform)) => Ok(*platform),
            Some(Request::Draft(_)) => Err(Status::ERR_REQUEST_PENDING),
            None => Err(Status::ERR_BAD_REQUEST),
        }
    }
}

// -- memory helpers -------------------------------------------------------

/// Copy a UTF-8 string out of guest linear memory.
///
/// The borrow of guest memory ends when this returns, so a caller holds an
/// owned `String` by the time it touches anything else. Same function and same
/// reason as `lib.rs`'s `read_string`, generic over the store's data type.
fn read_string<T>(caller: &Caller<'_, T>, ptr: i32, len: i32) -> Result<String, Status> {
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).map_err(|_| Status::ERR_BAD_UTF8)
}

fn read_bytes<T>(caller: &Caller<'_, T>, ptr: i32, len: i32) -> Result<Vec<u8>, Status> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(Status::ERR_BAD_MEMORY)?;
    let start = usize::try_from(ptr).map_err(|_| Status::ERR_BAD_MEMORY)?;
    let len = usize::try_from(len).map_err(|_| Status::ERR_BAD_MEMORY)?;
    let end = start.checked_add(len).ok_or(Status::ERR_BAD_MEMORY)?;

    let data = memory.data(caller);
    data.get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or(Status::ERR_BAD_MEMORY)
}

/// Deliver `bytes` through the guest's own buffer, and return the length it
/// would have needed.
///
/// This is [`OutBuffer`]'s protocol in one function, so that no import
/// implements it a second time slightly differently:
///
/// - the return value is always the value's **full** length, fitting or not;
/// - bytes are written only when they fit, so nothing is ever half a string;
/// - `cap == 0` is legal and is how a guest asks for a length alone.
///
/// `bytes` is owned rather than borrowed because writing needs `&mut Caller`,
/// which cannot coexist with a borrow of the host that produced the value. That
/// costs one allocation per read, which is the same allocation the DOM readers
/// already pay and for the same borrow-discipline reason.
///
/// # The whole declared buffer is validated, not just the part written
///
/// A guest that passes `(ptr, cap)` naming a region running off the end of
/// linear memory is wrong the moment it says so. Checking only the bytes about
/// to be written would let that call *succeed* for every value short enough to
/// fit inside the real memory, and fail later on the first long one — which is
/// the same shape of bug [`ReadOutcome::TooSmall`] exists to prevent: it
/// works in every test and breaks on the value that arrives in production.
///
/// So the bounds check happens before the fit check, and a bad buffer is
/// [`ERR_BAD_MEMORY`](Status::ERR_BAD_MEMORY) deterministically on the first
/// call, whatever the value's length.
///
/// `cap == 0` is included in that, and the reason is worth stating because it
/// is the case that looks exempt. An empty region is how a guest asks for a
/// length with nowhere to put the value, so the *capacity* is legal — but the
/// pointer still has to name a place in memory. Skipping the check for
/// `cap == 0` would make the answer depend on the stored value: a wild pointer
/// would come back `OK` for a value that does not fit (the early return below
/// never looks at it) and `ERR_BAD_MEMORY` for an empty one (`get_mut` does),
/// which is the same "works on the values that arrive, breaks on the other
/// one" shape this whole comment exists to argue against. The end of the
/// declared region is `ptr + cap`, which for `cap == 0` is `ptr` itself, so
/// one unconditional check covers both.
fn write_out<T>(caller: &mut Caller<'_, T>, out: OutBuffer, bytes: &[u8]) -> Result<i32, Status> {
    let len = i32::try_from(bytes.len()).map_err(|_| Status::ERR_BAD_MEMORY)?;

    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(Status::ERR_BAD_MEMORY)?;
    let start = out.ptr as usize;

    let declared_end = start
        .checked_add(out.cap as usize)
        .ok_or(Status::ERR_BAD_MEMORY)?;
    if declared_end > memory.data(&*caller).len() {
        return Err(Status::ERR_BAD_MEMORY);
    }

    if bytes.len() > out.cap as usize {
        // Nothing written. The guest resizes to `len` and calls again, and the
        // second call is sized from this answer so it cannot come back short.
        return Ok(len);
    }

    let end = start
        .checked_add(bytes.len())
        .ok_or(Status::ERR_BAD_MEMORY)?;
    let data = memory.data_mut(&mut *caller);
    let target = data.get_mut(start..end).ok_or(Status::ERR_BAD_MEMORY)?;
    target.copy_from_slice(bytes);
    Ok(len)
}

fn out_buffer(ptr: i32, cap: i32) -> Result<OutBuffer, Status> {
    Ok(OutBuffer {
        ptr: u32::try_from(ptr).map_err(|_| Status::ERR_BAD_MEMORY)?,
        cap: u32::try_from(cap).map_err(|_| Status::ERR_BAD_MEMORY)?,
    })
}

fn request_id(raw: i32) -> Result<RequestId, Status> {
    u32::try_from(raw)
        .map(RequestId)
        .map_err(|_| Status::ERR_BAD_REQUEST)
}

/// Run `body`, and turn any early `Status` into a recorded failing return.
///
/// Every import is "do some fallible steps, and if any fails record it and
/// return its code". Written out at each of the thirteen call sites that is
/// thirteen chances to forget the recording.
fn guard<T: HasPlatform>(
    caller: &mut Caller<'_, T>,
    op: PlatformOp,
    body: impl FnOnce(&mut Caller<'_, T>) -> Result<i32, Status>,
) -> i32 {
    caller.data_mut().platform_state().record_call(op);
    match body(caller) {
        Ok(value) => value,
        Err(status) => caller.data_mut().platform_state().fail(status),
    }
}

/// The platform host, or [`Status::ERR_NO_PLATFORM`].
fn platform<'a, T: HasPlatform>(caller: &'a Caller<'_, T>) -> Result<&'a PlatformHost, Status> {
    caller.data().platform().ok_or(Status::ERR_NO_PLATFORM)
}

// -- the imports ----------------------------------------------------------

/// Register the platform imports on `linker`, under the [`MODULE`] name.
///
/// The same module the DOM imports use, so a guest still imports exactly one
/// namespace and `the_guest_imports_only_the_blitz_module` stays true. A second
/// namespace would be a second convention to keep straight for no gain.
pub fn add_platform_to_linker<T: HasPlatform>(linker: &mut Linker<T>) -> Result<(), wasmi::Error> {
    // === fetch_new(method_atom_ptr, method_len, url_ptr, url_len) -> request | error ===
    //
    // The method crosses copied rather than interned. There are nine of them
    // and a guest uses two, so interning would spend an atom to save four
    // bytes once.
    linker.func_wrap(
        MODULE,
        "fetch_new",
        |mut caller: Caller<'_, T>,
         method_ptr: i32,
         method_len: i32,
         url_ptr: i32,
         url_len: i32|
         -> i32 {
            guard(&mut caller, PlatformOp::FetchNew, |caller| {
                let method_text = read_string(caller, method_ptr, method_len)?;
                let url_text = read_string(caller, url_ptr, url_len)?;
                let copied = method_text.len() + url_text.len();

                let method =
                    Method::try_from(method_text.as_str()).map_err(|_| Status::ERR_BAD_HEADER)?;
                let url = Url::parse(&url_text).map_err(|_| Status::ERR_BAD_URL)?;

                let state = caller.data_mut().platform_state();
                state.record_in(PlatformOp::FetchNew, copied);
                let id = state.new_request(FetchRequest::get(url).method(method))?;
                i32::try_from(id.0).map_err(|_| Status::ERR_TOO_MANY_REQUESTS)
            })
        },
    )?;

    // === fetch_header(request, name_ptr, name_len, value_ptr, value_len) -> OK | error ===
    linker.func_wrap(
        MODULE,
        "fetch_header",
        |mut caller: Caller<'_, T>,
         request: i32,
         name_ptr: i32,
         name_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> i32 {
            guard(&mut caller, PlatformOp::FetchHeader, |caller| {
                let id = request_id(request)?;
                let name = read_string(caller, name_ptr, name_len)?;
                let value = read_string(caller, value_ptr, value_len)?;
                let copied = name.len() + value.len();

                let name: blitz_traits::platform::http::HeaderName =
                    name.parse().map_err(|_| Status::ERR_BAD_HEADER)?;
                let value: blitz_traits::platform::http::HeaderValue =
                    value.parse().map_err(|_| Status::ERR_BAD_HEADER)?;

                let state = caller.data_mut().platform_state();
                state.record_in(PlatformOp::FetchHeader, copied);
                state.draft_mut(id)?.headers.insert(name, value);
                Ok(Status::OK.raw())
            })
        },
    )?;

    // === fetch_body(request, ptr, len) -> OK | error ===
    linker.func_wrap(
        MODULE,
        "fetch_body",
        |mut caller: Caller<'_, T>, request: i32, ptr: i32, len: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchBody, |caller| {
                let id = request_id(request)?;
                let body = read_bytes(caller, ptr, len)?;
                let copied = body.len();

                let state = caller.data_mut().platform_state();
                state.record_in(PlatformOp::FetchBody, copied);
                state.draft_mut(id)?.body = Some(Bytes::from(body));
                Ok(Status::OK.raw())
            })
        },
    )?;

    // === fetch_send(request) -> OK | error ===
    //
    // Returns the instant the request is handed over. The answer arrives
    // through `fetch_complete`, never through this return value.
    linker.func_wrap(
        MODULE,
        "fetch_send",
        |mut caller: Caller<'_, T>, request: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchSend, |caller| {
                let id = request_id(request)?;

                // Take the draft out before touching the platform host, so
                // nothing holds a borrow of the state across the call.
                let draft = match caller.data_mut().platform_state().requests.remove(&id) {
                    Some(Request::Draft(request)) => request,
                    Some(sent @ Request::Sent(_)) => {
                        caller.data_mut().platform_state().requests.insert(id, sent);
                        return Err(Status::ERR_ALREADY_SENT);
                    }
                    None => return Err(Status::ERR_BAD_REQUEST),
                };

                // Every failure after the take has to put the draft back.
                // `ERR_NO_PLATFORM` means "this embedding has no platform host
                // yet", which is not the guest's fault and invites a retry — and
                // a retry needs the method, URL, headers and body still to
                // exist. Dropping the draft here would answer a retryable error
                // while destroying the state the retry needs, and turn every
                // later call on the id into `ERR_BAD_REQUEST`.
                let platform_id = match platform(caller) {
                    Ok(platform) => platform.start_fetch(*draft),
                    Err(status) => {
                        caller
                            .data_mut()
                            .platform_state()
                            .requests
                            .insert(id, Request::Draft(draft));
                        return Err(status);
                    }
                };

                let state = caller.data_mut().platform_state();
                state.requests.insert(id, Request::Sent(platform_id));
                state.sent.insert(platform_id, id);
                Ok(Status::OK.raw())
            })
        },
    )?;

    // === fetch_status(request) -> http status | error ===
    //
    // A non-negative answer is the HTTP status, so 404 comes back as 404. The
    // whole reason the platform layer exists is that the resource loader turns
    // that into an error and drops it.
    linker.func_wrap(
        MODULE,
        "fetch_status",
        |mut caller: Caller<'_, T>, request: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchStatus, |caller| {
                let id = request_id(request)?;
                let platform_id = caller.data_mut().platform_state().platform_id(id)?;
                match platform(caller)?.state(platform_id) {
                    FetchState::Response => {
                        let status = platform(caller)?
                            .with_response(platform_id, |response| response.status.as_u16())
                            .ok_or(Status::ERR_FETCH)?;
                        Ok(i32::from(status))
                    }
                    FetchState::Pending => Err(Status::ERR_REQUEST_PENDING),
                    FetchState::Failed(_) => Err(Status::ERR_FETCH),
                    FetchState::Unknown => Err(Status::ERR_BAD_REQUEST),
                }
            })
        },
    )?;

    // === fetch_read_body(request, out_ptr, out_cap) -> len | error ===
    linker.func_wrap(
        MODULE,
        "fetch_read_body",
        |mut caller: Caller<'_, T>, request: i32, out_ptr: i32, out_cap: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchReadBody, |caller| {
                let id = request_id(request)?;
                let out = out_buffer(out_ptr, out_cap)?;
                let platform_id = caller.data_mut().platform_state().platform_id(id)?;

                let found =
                    platform(caller)?.with_response(platform_id, |response| response.body.to_vec());
                let body = match found {
                    Some(body) => body,
                    None => return Err(pending_or_failed(caller, platform_id)),
                };

                let written = write_out(caller, out, &body)?;
                if body.len() <= out.cap as usize {
                    caller
                        .data_mut()
                        .platform_state()
                        .record_out(PlatformOp::FetchReadBody, body.len());
                }
                Ok(written)
            })
        },
    )?;

    // === fetch_read_header(request, name_ptr, name_len, out_ptr, out_cap) -> len | ABSENT | error ===
    linker.func_wrap(
        MODULE,
        "fetch_read_header",
        |mut caller: Caller<'_, T>,
         request: i32,
         name_ptr: i32,
         name_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> i32 {
            guard(&mut caller, PlatformOp::FetchReadHeader, |caller| {
                let id = request_id(request)?;
                let out = out_buffer(out_ptr, out_cap)?;
                let name = read_string(caller, name_ptr, name_len)?;
                let copied = name.len();
                let platform_id = caller.data_mut().platform_state().platform_id(id)?;
                caller
                    .data_mut()
                    .platform_state()
                    .record_in(PlatformOp::FetchReadHeader, copied);

                let found = platform(caller)?.with_response(platform_id, |response| {
                    response
                        .headers
                        .get(name.as_str())
                        .map(|value| value.as_bytes().to_vec())
                });
                let value = match found {
                    Some(value) => value,
                    None => return Err(pending_or_failed(caller, platform_id)),
                };

                // A header that is not there is `ABSENT`, not an empty value.
                // `Content-Length: 0` and no `Content-Length` are different
                // facts and a guest must be able to tell them apart.
                let Some(value) = value else {
                    return Ok(Status::ABSENT.raw());
                };

                let written = write_out(caller, out, &value)?;
                if value.len() <= out.cap as usize {
                    caller
                        .data_mut()
                        .platform_state()
                        .record_out(PlatformOp::FetchReadHeader, value.len());
                }
                Ok(written)
            })
        },
    )?;

    // === fetch_read_url(request, out_ptr, out_cap) -> len | error ===
    //
    // The URL the response came from, after redirects. A guest resolving
    // relative links out of a body needs the one it landed on.
    linker.func_wrap(
        MODULE,
        "fetch_read_url",
        |mut caller: Caller<'_, T>, request: i32, out_ptr: i32, out_cap: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchReadUrl, |caller| {
                let id = request_id(request)?;
                let out = out_buffer(out_ptr, out_cap)?;
                let platform_id = caller.data_mut().platform_state().platform_id(id)?;

                let found = platform(caller)?.with_response(platform_id, |response| {
                    response.url.as_str().as_bytes().to_vec()
                });
                let url = match found {
                    Some(url) => url,
                    None => return Err(pending_or_failed(caller, platform_id)),
                };

                let written = write_out(caller, out, &url)?;
                if url.len() <= out.cap as usize {
                    caller
                        .data_mut()
                        .platform_state()
                        .record_out(PlatformOp::FetchReadUrl, url.len());
                }
                Ok(written)
            })
        },
    )?;

    // === fetch_release(request) -> OK | error ===
    //
    // Valid at any point, including while still in flight: the platform host
    // drops a late answer rather than resurrecting the entry.
    linker.func_wrap(
        MODULE,
        "fetch_release",
        |mut caller: Caller<'_, T>, request: i32| -> i32 {
            guard(&mut caller, PlatformOp::FetchRelease, |caller| {
                let id = request_id(request)?;
                let entry = caller.data_mut().platform_state().requests.remove(&id);
                match entry {
                    None => Err(Status::ERR_BAD_REQUEST),
                    Some(Request::Draft(_)) => Ok(Status::OK.raw()),
                    Some(Request::Sent(platform_id)) => {
                        if let Ok(platform) = platform(caller) {
                            platform.release(platform_id);
                        }
                        caller.data_mut().platform_state().sent.remove(&platform_id);
                        Ok(Status::OK.raw())
                    }
                }
            })
        },
    )?;

    // === storage_get(key_ptr, key_len, out_ptr, out_cap) -> len | ABSENT | error ===
    linker.func_wrap(
        MODULE,
        "storage_get",
        |mut caller: Caller<'_, T>,
         key_ptr: i32,
         key_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> i32 {
            guard(&mut caller, PlatformOp::StorageGet, |caller| {
                let out = out_buffer(out_ptr, out_cap)?;
                let key = read_string(caller, key_ptr, key_len)?;
                let copied = key.len();
                caller
                    .data_mut()
                    .platform_state()
                    .record_in(PlatformOp::StorageGet, copied);

                // The origin is not an argument and cannot be. See
                // `PlatformHost`: it holds one for life.
                let Some(value) = platform(caller)?.storage_get(&key) else {
                    return Ok(Status::ABSENT.raw());
                };

                let written = write_out(caller, out, value.as_bytes())?;
                if value.len() <= out.cap as usize {
                    caller
                        .data_mut()
                        .platform_state()
                        .record_out(PlatformOp::StorageGet, value.len());
                }
                Ok(written)
            })
        },
    )?;

    // === storage_set(key_ptr, key_len, value_ptr, value_len) -> OK | error ===
    linker.func_wrap(
        MODULE,
        "storage_set",
        |mut caller: Caller<'_, T>,
         key_ptr: i32,
         key_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> i32 {
            guard(&mut caller, PlatformOp::StorageSet, |caller| {
                let key = read_string(caller, key_ptr, key_len)?;
                let value = read_string(caller, value_ptr, value_len)?;
                let copied = key.len() + value.len();
                caller
                    .data_mut()
                    .platform_state()
                    .record_in(PlatformOp::StorageSet, copied);

                platform(caller)?
                    .storage_set(&key, &value)
                    .map_err(|_| Status::ERR_STORAGE)?;
                Ok(Status::OK.raw())
            })
        },
    )?;

    // === storage_remove(key_ptr, key_len) -> OK ===
    linker.func_wrap(
        MODULE,
        "storage_remove",
        |mut caller: Caller<'_, T>, key_ptr: i32, key_len: i32| -> i32 {
            guard(&mut caller, PlatformOp::StorageRemove, |caller| {
                let key = read_string(caller, key_ptr, key_len)?;
                let copied = key.len();
                caller
                    .data_mut()
                    .platform_state()
                    .record_in(PlatformOp::StorageRemove, copied);
                platform(caller)?.storage_remove(&key);
                Ok(Status::OK.raw())
            })
        },
    )?;

    // === storage_clear() -> OK ===
    linker.func_wrap(
        MODULE,
        "storage_clear",
        |mut caller: Caller<'_, T>| -> i32 {
            guard(&mut caller, PlatformOp::StorageClear, |caller| {
                platform(caller)?.storage_clear();
                Ok(Status::OK.raw())
            })
        },
    )?;

    Ok(())
}

/// Why there is no response: still running, or finished without one.
///
/// Called only when [`PlatformHost::with_response`] answered `None`, so the
/// request is known and one of these is true.
fn pending_or_failed<T: HasPlatform>(caller: &Caller<'_, T>, id: PlatformRequestId) -> Status {
    match caller.data().platform().map(|platform| platform.state(id)) {
        Some(FetchState::Pending) => Status::ERR_REQUEST_PENDING,
        Some(FetchState::Unknown) => Status::ERR_BAD_REQUEST,
        Some(FetchState::Failed(_)) => Status::ERR_FETCH,
        Some(FetchState::Response) => Status::ERR_FETCH,
        None => Status::ERR_NO_PLATFORM,
    }
}

/// What one drain did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Completed {
    /// Requests the platform host reported ready.
    pub drained: usize,
    /// Requests the guest was actually called for.
    ///
    /// Lower than `drained` when the guest released a request before the
    /// completion was delivered, which is legal and is not an error: a guest
    /// that stopped caring should not be called back.
    pub delivered: usize,
    /// Calls where the guest returned a negative status.
    pub failed: usize,
}

/// Deliver every completed fetch to the guest.
///
/// **Call this with no document borrow held.** It calls guest code, and guest
/// code mutates the DOM. This is the fetch counterpart of
/// [`dispatch_dom_event`](crate::dispatch_dom_event) and it exists for the same
/// reason: the point at which an answer *arrives* is chosen by the network, and
/// the point at which the guest is *told* has to be chosen by the embedder.
///
/// The guest export is `fetch_complete(request_id: u32) -> i32`. A guest
/// without one is not an error: an embedder may drive the platform imports from
/// host code with no guest callback at all, and the completions simply stay
/// readable until released.
///
/// The guest's contract is the same as `dispatch`'s: `fetch_complete` must
/// *complete*, leaving the guest settled before it returns, because the host
/// takes the document back the instant it does.
pub fn dispatch_fetch_completions<T: HasPlatform>(
    store: &mut Store<T>,
    instance: &Instance,
) -> Result<Completed, wasmi::Error> {
    // Phase 1: take the completions. Nothing here calls the guest.
    let ready = match store.data().platform() {
        Some(platform) => platform.take_ready(),
        None => return Ok(Completed::default()),
    };

    let mut completed = Completed {
        drained: ready.len(),
        ..Completed::default()
    };
    if ready.is_empty() {
        return Ok(completed);
    }

    // Phase 2: the guest. The borrow above ended at the statement boundary.
    let Some(export) = instance
        .get_typed_func::<u32, i32>(&mut *store, "fetch_complete")
        .ok()
    else {
        return Ok(completed);
    };

    for platform_id in ready {
        // Re-checked at delivery time rather than trusted from the drain, so a
        // request the guest released while others were being delivered is not
        // announced to it afterwards. Same rule as a listener removed by a
        // handler queued ahead of it.
        let Some(id) = store
            .data_mut()
            .platform_state()
            .sent
            .get(&platform_id)
            .copied()
        else {
            continue;
        };
        if !store.data_mut().platform_state().requests.contains_key(&id) {
            continue;
        }

        store
            .data_mut()
            .platform_state()
            .record_call(PlatformOp::FetchComplete);
        completed.delivered += 1;

        let status = export.call(&mut *store, id.0)?;
        if status < 0 {
            completed.failed += 1;
            store.data_mut().platform_state().counters.last_guest_status = Some(status);
        }
    }

    Ok(completed)
}

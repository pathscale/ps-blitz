# The platform ABI: `fetch` and storage

Companion to [ABI.md](ABI.md), which covers the DOM imports. Everything stated
there about handles, error returns and reentrancy holds here unchanged; this
document covers only what is new.

A separate file rather than more of `ABI.md` because the two are separate
surfaces with separate versions, and because `ABI.md` is being edited by the
change that adds the DOM readers. Merging them is a tidy-up for after both land.

Numbers here are asserted by `tests/platform.rs`, not quoted from a run.

## Where the conventions come from

`dom-abi`, not from this file. `Status`, `MAX_ID`, `OutBuffer` and
`ReadOutcome` are compiled against by both sides, which is the point of that
crate: an error code that lives in a markdown table, as an `i32` in the host,
and as another `i32` in the guest bindings is three places to get it wrong and
only one of them fails a build.

So this document explains and does not define. If it disagrees with
`dom_abi::host`, `dom_abi::host` is right.

## The imports

Thirteen, in the same `blitz` module as the DOM imports. One namespace, so
`the_guest_imports_only_the_blitz_module` stays true and a guest has one thing
to import rather than two.

| Import | Signature | Returns |
| --- | --- | --- |
| `fetch_new` | `(method_ptr, method_len, url_ptr, url_len) -> i32` | request id, or error |
| `fetch_header` | `(request, name_ptr, name_len, value_ptr, value_len) -> i32` | `OK`, or error |
| `fetch_body` | `(request, ptr, len) -> i32` | `OK`, or error |
| `fetch_send` | `(request) -> i32` | `OK`, or error |
| `fetch_status` | `(request) -> i32` | HTTP status, or error |
| `fetch_read_body` | `(request, out_ptr, out_cap) -> i32` | byte length, or error |
| `fetch_read_header` | `(request, name_ptr, name_len, out_ptr, out_cap) -> i32` | byte length, `ABSENT`, or error |
| `fetch_read_url` | `(request, out_ptr, out_cap) -> i32` | byte length, or error |
| `fetch_release` | `(request) -> i32` | `OK`, or error |
| `storage_get` | `(key_ptr, key_len, out_ptr, out_cap) -> i32` | byte length, `ABSENT`, or error |
| `storage_set` | `(key_ptr, key_len, value_ptr, value_len) -> i32` | `OK`, or error |
| `storage_remove` | `(key_ptr, key_len) -> i32` | `OK`, or error |
| `storage_clear` | `() -> i32` | `OK`, or error |

### The export

| Export | Signature | Returns |
| --- | --- | --- |
| `fetch_complete` | `(request_id: u32) -> i32` | `OK`, or the guest's own status |

Optional. A guest without one is not an error: an embedder may drive these
imports from host code with no guest callback at all, and completions simply
stay readable until released.

Where present, the contract is `dispatch`'s: **it must complete**, leaving the
guest settled before it returns, because the host takes the document back the
instant it does.

## `fetch_status` returns the status

`404` comes back as `404`. This is the entire reason the platform layer exists.

`blitz-net`'s `NetProvider` implementation — the one that loads stylesheets and
images — turns any non-2xx into a `ProviderError`, logs it, and drops it on the
floor. That is right for an image: the bytes decode or they do not. It makes
`fetch()` impossible, because a guest could never observe a 404 at all.

So `FetchState::Response` and `FetchError` are different things all the way down
the stack, and they arrive at the guest as different things: a status, or
`ERR_FETCH`.

## Status codes

DOM codes are `-1` to `-9` and belong to `dom_abi::host::Status`. Platform codes
start at **`-20`**, leaving `-10` to `-19` free so the DOM range can grow
contiguously without ever colliding.

| Code | Meaning |
| --- | --- |
| `-20` | `ERR_BAD_REQUEST` — no such request id, or it was released |
| `-21` | `ERR_REQUEST_PENDING` — not finished; wait for `fetch_complete` |
| `-22` | `ERR_FETCH` — finished with no response at all |
| `-23` | `ERR_TOO_MANY_REQUESTS` |
| `-24` | `ERR_BAD_URL` |
| `-25` | `ERR_BAD_HEADER` — bad method, header name, or header value |
| `-26` | `ERR_STORAGE` — the write was refused |
| `-27` | `ERR_NO_PLATFORM` — no providers installed on this instance |
| `-28` | `ERR_ALREADY_SENT` |

**These belong in `dom-abi` and are in `src/platform.rs` only until it lands.**
Adding constants to a crate somebody else is actively writing is how two people
pick the same number. Moving them is a rename with no value change.

`ERR_NO_PLATFORM` rather than a panic: an embedder that binds these imports and
installs no providers has made a mistake that belongs entirely to the embedding,
and killing the guest for it tells nobody anything.

## Reads

Mechanism (b), from `dom_abi::host::OutBuffer`, identical to the DOM readers.
The guest passes `(out_ptr, out_cap)`; the host returns the value's **full**
length whether or not it fit, and writes the bytes only if they fit. A guest
that gets `len > cap` resizes to `len` and calls again, and the second call is
sized from the host's own answer so it cannot come back short.

Two details this binding adds, both asserted:

**The whole declared buffer is bounds-checked, not just the bytes written.** A
guest passing a `(ptr, cap)` that runs off the end of linear memory gets
`ERR_BAD_MEMORY` on the first call, whatever the value's length. Checking only
the region about to be written would let that call succeed for every value short
enough to fit inside real memory and fail later on the first long one, which is
the same shape of bug `ReadOutcome::TooSmall` exists to prevent. `cap == 0` names
an empty region and stays legal, because it is how a guest asks for a length
alone.

**A too-small read counts no bytes out.** It wrote nothing, so the counter says
nothing was written even though a length was returned. That makes the retry
visible in the numbers rather than double-counted.

## `ABSENT` applies to storage and headers alike

`storage_get` on a key that was never set is `ABSENT`, and a key holding `""` is
a successful read of length zero. `fetch_read_header` behaves the same way, and
it matters for the same reason: no `Content-Length` and `Content-Length: 0` are
different facts.

`ABSENT` is not recorded in `last_error`. A guest polling an optional key would
otherwise leave the slot permanently set to something that never went wrong.

## Completion: the event path's shape, reused

**The host queues completions and calls the guest only after the document is
released**, exactly as it does for events.

A response arrives on a network thread at a moment nothing knows about. It goes
into a queue and nothing else happens. Later, with no document borrow live, the
embedder calls `dispatch_fetch_completions`, which drains the queue and calls
`fetch_complete` once per completed request.

This is `dispatch_dom_event`'s design rather than a new one, because it is the
same hazard: guest code must not run while the document is borrowed, and a
guest's first act on a completed fetch is to put the result in the DOM.

Two pieces of semantics are honoured, matching the listener rules:

- **A request released before its completion is delivered is not announced.**
  Ids are re-checked at delivery time, not trusted from the drain.
- **A guest that reports a failure does not stop the drain.** The remaining
  completions are still delivered, and the count of failures is on the counters.

## Handles and ids

Request ids are the binding's own, not the platform layer's.

A guest builds a request before it is sent, and the platform host only issues
its own id at send time. Handing the platform id straight through would mean the
id a guest holds changes halfway through the request's life. So `fetch_new`
issues an id, `fetch_send` binds it to a platform request, and the guest sees one
id from `fetch_new` to `fetch_release`.

Never reused, capped at `MAX_ID`, and a forged one is `ERR_BAD_REQUEST` rather
than a trap — the same three rules handles and listener ids follow, for the same
reasons.

`fetch_release` is valid at any point, including while a request is still in
flight. The platform host drops a late answer rather than resurrecting the
entry, which is what makes tearing down a page with requests outstanding safe.

## Counters

`PlatformCounters`, and deliberately **not** more fields on `Counters`.

The brief asks for fetch bytes to be attributable separately from DOM bytes in
both directions. Two structs make that impossible to get wrong. One struct would
have a `total_bytes` method that silently began answering a different question
the day a response body landed in it: 40 KB of JSON and a 3-byte text update are
not the same measurement, and `counters.rs` already documents at length why a
true number telling a false story is the failure mode worth designing against.

New here: `bytes_out`. Until readers existed, nothing crossed this way, so the
DOM counters have no equivalent. `fetch_bytes()` and `storage_bytes()` answer
the two questions the brief asks, and `fetch_complete`'s byte counts are
structurally zero — the argument is a request id and there is no pointer in the
signature for anything else to travel through.

## Origin scoping

**No storage import takes an origin, and none can.** `PlatformHost` is built for
one origin and holds it for life, so a binding has no way to name another. The
security property is therefore structural rather than a rule somebody has to
follow at thirteen call sites.

`file:` and `data:` documents each get a fresh opaque origin, so two local HTML
files cannot read each other's stored state. See `blitz-platform-api`'s README.

## Deviations, and why

- **`fetch_new` takes the method copied, not interned.** There are nine methods
  and a guest uses two. Interning would spend a permanent atom to save four
  bytes once.
- **Header names are copied too**, on both the request and the read side. They
  look like the closed set atoms are for, but a guest reading a header it was
  told about by another header, or by a body, would be interning something
  data-derived — and atoms are never released. Copying is the safe default;
  interning them is a later optimisation with a measurement behind it.
- **No streaming.** A response body is complete before the guest is told. A
  guest wanting progressive delivery needs a different shape, and nothing in
  scope wants one.
- **No abort.** `blitz_traits::net::AbortSignal` exists and `blitz-net` already
  honours it for resource loads. `fetch_release` stops the guest caring, but
  does not stop the request. Additive.
- **No redirect or credentials mode.** Redirects follow `reqwest`'s default and
  the shared cookie jar is always sent.
- **The binding is generic over the store payload**, not written against `Host`.
  A platform import cannot reach a document because the type it is given does
  not offer one, which is the same technique the event handler uses to guarantee
  it cannot reach a guest export. `tests/platform.rs` runs the whole suite
  against a store payload with no document in it at all, which is the evidence
  that the independence is real rather than documented.

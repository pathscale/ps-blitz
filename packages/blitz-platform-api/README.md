# `blitz-platform-api`

The host platform APIs a guest expects to find, written once: `fetch`, and
per-origin storage.

This is the third crate on the split that produced `blitz-dom-api`. That one
took the DOM operations and removed the runtime. This one takes the platform
APIs and removes **both** the runtime and the transport, so what is left is the
part every binding would otherwise write for itself.

```text
  blitz-traits         FetchProvider, StorageProvider, OriginKey
        |                  the embedder implements these
  blitz-platform-api   this crate: origin scoping, the in-flight table,
        |              the completion queue, the counters
  blitz-wasm           binds it for a WebAssembly guest
  blitz-script         can bind the same thing for JavaScript
```

## Why it is not inside `blitz-wasm`

Because JavaScript has no `fetch()` either.

`blitz-script/src/fetch.rs` is synchronous `<script src>` loading and nothing
more: a `ScriptFetcher` trait over `file:` and `data:` URLs, because classic
scripts must execute in document order and block the parser. It is not a
`fetch()` and was never meant to be one. Meanwhile chuzz's `WEB_API_SHIM`
supplies an in-memory `localStorage`, an in-memory `sessionStorage`, and a
`URL` its own comment describes as not WHATWG-conformant.

So there are two runtimes with the same hole. Written inside the wasm binding,
all of this would have to be written a second time for Boa, and the second one
would differ. Written here, Boa's binding is argument coercion over the same
host.

## What is deliberately absent

**No HTTP client.** `blitz-net` already ships `reqwest` configured with HTTP/2,
a cookie jar, brotli/gzip/deflate/zstd, and an `http-cache`/cacache disk cache,
plus a per-host semaphore holding the browser's six-concurrent-requests-per-
origin cap. It implements `FetchProvider` over that same client, so a guest's
`fetch` shares the connection pool, the cookies, the cache and the concurrency
budget with the page's own resource loads. A second client would have given the
guest its own cookie jar and its own six connections per host.

**No scripting engine.** This crate sits below every runtime binding.

`tests/no_client_or_engine.rs` asserts both against the resolved dependency
graph rather than against the manifest, because the way either would actually
arrive is a feature flag enabled three crates away. Same technique and the same
reasoning as `blitz-dom-api`'s `no_boa.rs`, including a self-test of the
detector: `http` is in this graph legitimately and must not be mistaken for an
HTTP client, exactly as `keyboard-types` must not be mistaken for Boa.

The resolved graph today is `blitz-traits` plus what `blitz-traits` already
pulls: `http`, `url`, `bytes`, `serde`, `keyboard-types`, `smol_str`,
`bitflags`, `cursor-icon`, `atomic_refcell`. Nothing else.

## Two rules the design is built around

**A completed fetch is delivered by draining a queue, never by a callback that
reaches into a document.**

`blitz-wasm` already learned this on the event path. Calling a guest from inside
`EventHandler::handle_event` would run guest code while the `EventDriver` holds
the document, and a guest's first act on an event is to mutate the DOM. Its
answer was to queue listener ids during propagation and call the guest
afterwards, with the borrow gone. A response arriving on a network thread has
the identical hazard from the other direction, so it takes the identical answer.

As there, the rule is enforced by construction rather than remembered: nothing
in this crate can reach a document because nothing here has ever been given one,
and the completion handler holds a `Weak` to the request table and nothing else.
That `Weak` is also what makes tearing down a page with requests in flight safe:
the answer arrives, finds nothing to write into, and is dropped.

**A `PlatformHost` is built for one origin and holds it for life.** No storage
method takes an origin, so a binding cannot pass the wrong one. Origin scoping
is a security property, and the way it fails in practice is not that somebody
argues against it, it is that one call site passes the wrong value.

`file:` and `data:` documents each get their own **opaque** origin, minted fresh
per document, so two local HTML files cannot read each other's stored state.
That is deliberate and it is not a corner case: `blitz-net`'s `NetProvider`
`file:` handler reads whatever path it is handed, and one hole of that shape is
enough. (This crate does not fix that hole. It is in `NetProvider::fetch`'s
resource-loading path, it predates this work, and fixing it means deciding a
local-file access policy, which is a separate change with its own argument.)

## Deferred

### WebSocket

**Not built. It is the one item in scope that is a genuinely new dependency,**
which is why it is a decision rather than an omission: nothing in the Blitz
graph speaks WebSocket, so this cannot be routed through something that already
exists the way `fetch` is routed through `blitz-net`'s client.

The stack is settled even though the code is not. `endpoint-libs` already runs a
WebSocket client in this codebase and its pin is the one to match:

```toml
tokio-tungstenite = { version = "0.29.0", default-features = false, features = [
    "rustls-tls-webpki-roots",
    "connect",
] }
rustls = { version = "0.23", default-features = false, features = ["ring", "logging", "std"] }
```

Its `ws-client` feature is already carved out as a client that pulls no server,
no hyper and no axum, which is exactly the shape wanted here. Matching the pin
means one tungstenite and one rustls across both repos.

**The open question is TLS, and it should be answered before the code is
written rather than after.** `blitz-net` builds `reqwest` with `native-tls`.
Adding rustls-backed tungstenite puts two TLS stacks in the binary: two root
stores, two sets of protocol bugs, two things to audit, and two answers to "does
this build trust this certificate". The three ways out are to accept that, to
move `blitz-net` to `rustls-tls` so both share one, or to take tungstenite's
`native-tls` feature and diverge from the `endpoint-libs` pin. Unwinding a TLS
choice later touches every dependent, which is why this is not being decided by
whoever writes the first socket.

### `history` / `pushState`

**Not exposed.** Unlike WebSocket this needs no new dependency and almost no new
code, which is precisely what makes it the more dangerous of the two.

chuzz's `browser.rs` already owns all of it: tabs, a `history: Vec<Request>` per
tab, a `current` index, and the `can_go_back` / `can_go_forward` predicates
derived from them. Exposing `pushState` is wiring.

The reason not to wire it yet is that **a page's session history and the
browser's navigation stack are currently one structure**. A guest calling
`pushState` would be writing directly into the thing the back button reads and
the address bar renders. Every misbehaviour a page can have then becomes a
misbehaviour of the browser chrome: a loop that pushes on every frame makes the
back button unusable, and a page that rewrites its own entry can make the
address bar disagree with what is on screen.

The fix is to separate per-page session history from the browser's own
navigation stack, so a guest writes to the first and the second is derived. That
is a design, not a patch, and it belongs to whoever owns `browser.rs` rather
than to this crate.

### Blobs

**Not built, and the split is deliberate.** Storage here is the *data* layer:
string keys to string values, per origin, which is the `localStorage` model and
what a guest storing preferences, tokens or serialised state actually needs. A
guest with structure serialises it, exactly as it would against a browser, and
nothing in this crate looks inside a value.

Blobs are a different problem wearing the same word: binary payloads with a
lifetime, a quota, and a handle that outlives the call that made it. chuzz's
shim currently fakes `URL.createObjectURL` with a token backing nothing, which
is the right amount of effort for a page that only creates and revokes one. A
real blob layer starts when something needs to read one.

## Also not here, and smaller

These are gaps rather than decisions, listed so nobody has to rediscover them:

- **No abort.** `FetchRequest` carries no `AbortSignal`, though
  `blitz_traits::net` defines one and `blitz-net` already honours it for
  resource loads. Additive when a caller needs it.
- **No `key(n)` or `length` on storage.** The brief scopes storage to get, set,
  remove and clear. `localStorage` has both of the others and a guest
  enumerating its own keys will want them; adding them means deciding an
  iteration order, which the specification does not fix.
- **No redirect or credentials mode.** `fetch()` has both. `blitz-net` follows
  redirects with `reqwest`'s default policy and sends the shared cookie jar,
  and there is currently no way for a caller to ask for anything else.
- **No quota.** `StorageError::QuotaExceeded` exists in the trait because it is
  the one storage failure guests actually handle, and no provider raises it yet.

## Persistence is the embedder's

`MemoryStorage` here is correct and not durable, which makes it right for tests,
for a private-browsing mode, and for an embedder that has not wired a real store
yet.

A persistent provider belongs to whoever is embedding Blitz, because every
question it raises is theirs: where the profile directory is, what the quota is,
and what happens when the store will not load. That last one has a right answer
worth writing down in advance: **a storage file that cannot be read must lose
the data, not stop the browser.** An embedded store is entitled to refuse a torn
file rather than serve invented rows, and a browser that will not open because a
preferences file is damaged is a worse failure than a site that lost its
preferences.

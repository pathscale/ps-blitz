//! Abstractions of the host platform APIs a guest expects to find: fetching,
//! and per-origin storage.
//!
//! These sit beside [`crate::net`] rather than inside it, and the distinction
//! is worth stating because it is not obvious from the names.
//!
//! [`NetProvider`](crate::net::NetProvider) loads *resources the document
//! needs*: a stylesheet, an image, a script. Its completion callback is
//! [`NetHandler::bytes`](crate::net::NetHandler), which takes a resolved URL
//! and some bytes, and that is the whole answer. There is no status code and
//! there are no response headers, because a caller loading an image has nothing
//! to do with either: the bytes decode or they do not.
//!
//! A `fetch()` caller is the opposite. A 404 is not a failure to be logged and
//! dropped, it is the answer, and so are the headers. So this is a second trait
//! rather than a wider `NetProvider`: an embedder that only ever loads page
//! resources keeps implementing the small one, and the two can share a client.
//! `blitz-net` implements both over the same `reqwest` client, the same
//! connection pool, the same cookie jar and the same disk cache.
//!
//! Nothing here performs IO or names an HTTP client. The implementations live
//! with the embedder.

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, StatusCode};
use std::sync::atomic::{AtomicU64, Ordering};
pub use url::Url;

/// A type that performs `fetch`-style requests on behalf of a guest.
///
/// Asynchronous by construction: [`fetch`](FetchProvider::fetch) hands the
/// request over and returns immediately, and the answer arrives through the
/// handler on whatever thread the implementation chooses. Callers must not
/// assume the handler runs later, or on another thread: an implementation
/// serving a `data:` URL may answer before `fetch` returns.
pub trait FetchProvider: Send + Sync + 'static {
    fn fetch(&self, request: FetchRequest, handler: Box<dyn FetchHandler>);
}

/// Receives the result of one [`FetchProvider::fetch`].
///
/// Takes `self: Box<Self>` so that a handler can be consumed by completing,
/// which makes "completed twice" unrepresentable rather than merely wrong.
/// Same shape as [`NetHandler`](crate::net::NetHandler).
pub trait FetchHandler: Send + Sync + 'static {
    /// Called exactly once.
    ///
    /// **A response with a non-success status is `Ok`, not `Err`.** A 404 has a
    /// status, headers and usually a body, all of which the caller asked for.
    /// [`FetchError`] is for the cases where there is no response at all.
    fn complete(self: Box<Self>, result: Result<FetchResponse, FetchError>);
}

/// A request, loosely <https://fetch.spec.whatwg.org/#requests>.
///
/// Narrower than [`Request`](crate::net::Request) on purpose: no form bodies,
/// because a guest that wants a multipart body can encode one, and no abort
/// signal, because nothing in the current scope cancels. Both are additive
/// later; see the README.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    /// `None` is distinct from `Some(empty)`: the first sends no body at all,
    /// the second sends a zero-length one with a `Content-Length: 0`.
    pub body: Option<Bytes>,
}

impl FetchRequest {
    /// A GET with no headers and no body.
    pub fn get(url: Url) -> Self {
        Self {
            url,
            method: Method::GET,
            headers: HeaderMap::new(),
            body: None,
        }
    }

    /// The same request with a method.
    pub fn method(mut self, method: Method) -> Self {
        self.method = method;
        self
    }

    /// The same request with a body.
    pub fn body(mut self, body: Bytes) -> Self {
        self.body = Some(body);
        self
    }
}

/// A response, loosely <https://fetch.spec.whatwg.org/#responses>.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FetchResponse {
    /// The URL the response actually came from, after any redirects.
    ///
    /// Not the requested URL. A guest resolving relative links out of a
    /// response body needs the one it landed on.
    pub url: Url,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl FetchResponse {
    /// A response with the given status and no headers or body.
    ///
    /// A constructor rather than a struct literal because the type is
    /// `#[non_exhaustive]`, and it is `#[non_exhaustive]` because every
    /// provider lives outside this crate: a field added here later would
    /// otherwise break all of them at once. Build with this and the setters,
    /// and a new field arrives with a default instead of a compile error.
    pub fn new(url: Url, status: StatusCode) -> Self {
        Self {
            url,
            status,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn body(mut self, body: Bytes) -> Self {
        self.body = body;
        self
    }
}

/// Why there is no response.
///
/// Note what is *not* here: an HTTP status. A server that answered is a
/// [`FetchResponse`] whatever it answered with. These are the cases where
/// nothing answered.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The request could not be completed: DNS, TLS, connection, timeout, a
    /// malformed response, or a local read that failed.
    ///
    /// One variant rather than a taxonomy because `fetch()` itself exposes one
    /// (`TypeError`), and a guest cannot act differently on a DNS failure than
    /// on a TLS one. The string is for a human reading a log.
    Network(String),
    /// The URL's scheme is not one this provider serves.
    UnsupportedScheme(String),
    /// The request was rejected before it was sent: an unparseable header, a
    /// method the provider will not issue.
    InvalidRequest(String),
    /// Refused by policy rather than by the network. A provider that enforces
    /// an allowlist, or refuses to let an opaque origin reach the network,
    /// answers with this.
    Blocked(String),
    /// There is no provider installed. See [`DummyFetchProvider`].
    NoProvider,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(detail) => write!(f, "network error: {detail}"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported URL scheme: {scheme}"),
            Self::InvalidRequest(detail) => write!(f, "invalid request: {detail}"),
            Self::Blocked(reason) => write!(f, "blocked: {reason}"),
            Self::NoProvider => write!(f, "no fetch provider is installed"),
        }
    }
}

impl std::error::Error for FetchError {}

/// A [`FetchProvider`] that answers every request with [`FetchError::NoProvider`].
///
/// **It answers rather than doing nothing, which is where it differs from
/// [`DummyNetProvider`](crate::net::DummyNetProvider), and the difference is
/// deliberate.** A dropped resource load leaves an image unpainted, which is
/// visible and survivable. A dropped `fetch` leaves the guest waiting on an
/// answer that will never come, and a guest awaiting a promise that never
/// settles is indistinguishable from a hang. So the no-op provider here fails
/// fast instead of going quiet.
#[derive(Default)]
pub struct DummyFetchProvider;

impl FetchProvider for DummyFetchProvider {
    fn fetch(&self, _request: FetchRequest, handler: Box<dyn FetchHandler>) {
        handler.complete(Err(FetchError::NoProvider));
    }
}

/// The security principal a piece of stored state belongs to.
///
/// **Storage is keyed by this and never by a URL.** Two pages on one origin
/// share storage; two pages on different origins must not see each other's,
/// and that is a security property rather than a nicety.
///
/// Derived through [`Url::origin`], which is Servo's implementation of the
/// WHATWG origin concept, so the awkward cases are already decided: `blob:`
/// unwraps to its inner origin, and `file:` and `data:` are *opaque*.
///
/// # Opaque origins
///
/// [`Url::origin`] mints a **fresh** opaque origin on every call for a `file:`
/// URL, so a document must derive its key once, at construction, and hold it.
/// Deriving twice would give one document two identities and lose its own
/// writes.
///
/// That is exactly the intended behaviour and not a wrinkle to route around.
/// Collapsing every `file:` page into one shared bucket would mean any local
/// HTML file could read any other's saved state, which is the same class of
/// mistake as `blitz-net`'s `file:` handler reading any path it is handed.
/// One such hole is enough.
///
/// [`is_persistable`](OriginKey::is_persistable) is the flag a disk-backed
/// provider must check: an opaque origin is unique to one instance of one
/// document, so writing it to disk stores a row that can never be read again.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginKey {
    key: String,
    persistable: bool,
}

/// Distinguishes one opaque origin from the next within a process.
static NEXT_OPAQUE: AtomicU64 = AtomicU64::new(0);

impl OriginKey {
    /// The origin of a document loaded from `url`.
    ///
    /// Call once per document and keep the result. See the type's docs for why
    /// calling twice is a bug for `file:` and `data:` URLs.
    pub fn for_document(url: &Url) -> Self {
        let origin = url.origin();
        if origin.is_tuple() {
            Self {
                key: origin.ascii_serialization(),
                persistable: true,
            }
        } else {
            Self::opaque()
        }
    }

    /// A fresh opaque origin, shared with nothing.
    ///
    /// The serialisation of every opaque origin is the string `null`, so it
    /// cannot be used to tell two of them apart. This appends a
    /// process-unique counter so that a provider keying an in-memory map on
    /// [`as_str`](OriginKey::as_str) isolates them from each other, which
    /// `null` alone would not.
    pub fn opaque() -> Self {
        let nth = NEXT_OPAQUE.fetch_add(1, Ordering::Relaxed);
        Self {
            key: format!("null:{nth}"),
            persistable: false,
        }
    }

    /// A stable string for this origin, suitable as a map or table key.
    ///
    /// Unique per origin, including across opaque ones. Not a URL, and not to
    /// be parsed back into one.
    pub fn as_str(&self) -> &str {
        &self.key
    }

    /// Whether state under this origin may be written to disk.
    ///
    /// False for opaque origins. A provider that persists must check this and
    /// keep the rest in memory; see the type's docs.
    pub fn is_persistable(&self) -> bool {
        self.persistable
    }
}

/// Per-origin key/value storage, the model behind `localStorage`.
///
/// Synchronous, because the API it backs is. Values are strings, because the
/// API it backs stores strings: a caller with structure serialises it, exactly
/// as it would against a browser, and no implementation here looks inside a
/// value.
///
/// **Every method takes an [`OriginKey`], and an implementation must scope by
/// it.** This is the whole security contract of the trait and it is not
/// optional. An implementation that ignores the argument compiles, works in a
/// single-origin test, and leaks every site's data to every other.
pub trait StorageProvider: Send + Sync + 'static {
    fn get(&self, origin: &OriginKey, key: &str) -> Option<String>;
    fn set(&self, origin: &OriginKey, key: &str, value: &str) -> Result<(), StorageError>;
    fn remove(&self, origin: &OriginKey, key: &str);
    /// Removes everything under `origin`, and nothing under any other.
    fn clear(&self, origin: &OriginKey);
}

/// Why a write did not happen.
///
/// Reads, removes and clears do not fail: an absent key is `None` and removing
/// what is not there is a no-op, which is what the storage API specifies.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The origin is at its limit. `localStorage` surfaces this as a
    /// `QuotaExceededError`, which is the one storage failure guests handle.
    QuotaExceeded,
    /// The backing store refused the write.
    Backend(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuotaExceeded => write!(f, "storage quota exceeded for this origin"),
            Self::Backend(detail) => write!(f, "storage backend error: {detail}"),
        }
    }
}

impl std::error::Error for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn same_origin_urls_share_a_key() {
        let a = OriginKey::for_document(&url("https://example.com/one?x=1#frag"));
        let b = OriginKey::for_document(&url("https://example.com/two"));
        assert_eq!(a, b);
        assert!(a.is_persistable());
    }

    #[test]
    fn scheme_port_and_host_all_separate_origins() {
        let https = OriginKey::for_document(&url("https://example.com/"));
        let http = OriginKey::for_document(&url("http://example.com/"));
        let port = OriginKey::for_document(&url("https://example.com:8443/"));
        let host = OriginKey::for_document(&url("https://other.example.com/"));
        assert_ne!(https, http);
        assert_ne!(https, port);
        assert_ne!(https, host);
    }

    /// The property that stops one local page reading another's state. Two
    /// *identical* file URLs must still be different origins.
    #[test]
    fn every_file_url_gets_its_own_opaque_origin() {
        let one = OriginKey::for_document(&url("file:///home/user/page.html"));
        let same_again = OriginKey::for_document(&url("file:///home/user/page.html"));
        let other = OriginKey::for_document(&url("file:///home/user/other.html"));

        assert_ne!(one, same_again);
        assert_ne!(one, other);
        assert!(!one.is_persistable());
        assert!(!same_again.is_persistable());
    }

    #[test]
    fn data_urls_are_opaque_too() {
        let key = OriginKey::for_document(&url("data:text/html,<p>hi"));
        assert!(!key.is_persistable());
    }

    /// `ascii_serialization` answers `null` for every opaque origin, so a
    /// provider keying on it alone would merge them all into one bucket. This
    /// is the assertion that the counter is doing its job.
    #[test]
    fn opaque_keys_are_distinguishable_from_each_other() {
        let one = OriginKey::opaque();
        let two = OriginKey::opaque();
        assert_ne!(one.as_str(), two.as_str());
        assert_ne!(one.as_str(), "null");
    }

    /// A `blob:` URL carries its origin inside it, and that origin is the one
    /// that owns any state reached through it.
    #[test]
    fn a_blob_url_takes_the_origin_it_was_minted_from() {
        let blob = OriginKey::for_document(&url("blob:https://example.com/uuid-goes-here"));
        let page = OriginKey::for_document(&url("https://example.com/index.html"));
        assert_eq!(blob, page);
    }

    #[test]
    fn the_dummy_provider_answers_rather_than_going_quiet() {
        use std::sync::{Arc, Mutex};

        struct Record(Arc<Mutex<Option<Result<FetchResponse, FetchError>>>>);
        impl FetchHandler for Record {
            fn complete(self: Box<Self>, result: Result<FetchResponse, FetchError>) {
                *self.0.lock().unwrap() = Some(result);
            }
        }

        let seen = Arc::new(Mutex::new(None));
        DummyFetchProvider.fetch(
            FetchRequest::get(url("https://example.com/")),
            Box::new(Record(seen.clone())),
        );

        let answer = seen.lock().unwrap().take();
        assert_eq!(
            answer.expect("the dummy must answer").unwrap_err(),
            FetchError::NoProvider
        );
    }
}

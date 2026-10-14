//! Abstractions of networking so that custom networking implementations can be provided

pub use bytes::Bytes;
pub use http::{self, HeaderMap, HeaderValue, Method};
use serde::{
    Serialize,
    ser::{SerializeSeq, SerializeTuple},
};
use std::sync::{Arc, Mutex};
use std::{ops::Deref, path::PathBuf};
pub use url::Url;
pub use crate::resource_policy::{Credentials, Initiator, ResourcePolicy, ResourceScope};

/// A cookie jar used by network providers to supply request cookies and store
/// cookies received in response headers.
pub trait CookieJar: Send + Sync + 'static {
    /// Returns the `Cookie` header to send with a request to `url`, if present.
    fn cookies_header(&self, url: &Url) -> Option<HeaderValue>;

    /// Stores cookies from the response headers received from `url`.
    fn store_from_response(&self, url: &Url, headers: &HeaderMap);
}

/// A type that fetches resources for a Document.
///
/// This may be over the network via http(s), via the filesystem, or some other method.
pub trait NetProvider: Send + Sync + 'static {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>);

    /// Bind a child browsing context. srcdoc inherits the existing principal.
    fn for_child_document(&self, _url: Url) -> Option<Arc<dyn NetProvider>> {
        None
    }

    /// The native principal held by a document-bound provider.
    fn resource_initiator(&self) -> Initiator {
        Initiator::Unknown
    }

    /// Whether this provider is a no-op (e.g. `DummyNetProvider`) that will never
    /// deliver resources. When true, callers must NOT register resources as
    /// "pending critical" — doing so blocks painting forever, since the
    /// completion callback never fires. Used by integrations that feed a
    /// pre-rendered DOM and perform no sub-fetches (e.g. aginxbrowser).
    fn is_noop(&self) -> bool {
        false
    }
}

/// A type that parses raw bytes from a network request into a Data and then calls
/// the NetCallack with the result.
pub trait NetHandler: Send + Sync + 'static {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes);
}

/// A callback which gets called every time a network request completes
// Q: Should we use std::task::Waker for this?
pub trait NetWaker: Send + Sync + 'static {
    fn wake(&self, client_id: usize);
}

impl<F: Fn(usize) + Send + Sync + 'static> NetWaker for F {
    fn wake(&self, doc_id: usize) {
        self(doc_id)
    }
}

/// The destination of a request, as a Fetch metadata token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestDestination {
    #[default]
    Empty,
    Document,
    Iframe,
    Style,
    Script,
    Image,
    Font,
    Audio,
    Video,
    Track,
    Manifest,
}

impl RequestDestination {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Document => "document",
            Self::Iframe => "iframe",
            Self::Style => "style",
            Self::Script => "script",
            Self::Image => "image",
            Self::Font => "font",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Track => "track",
            Self::Manifest => "manifest",
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone)]
/// A request type loosely representing <https://fetch.spec.whatwg.org/#requests>
pub struct Request {
    pub url: Url,
    pub method: Method,
    pub content_type: Option<String>,
    pub headers: HeaderMap,
    pub body: Body,
    pub signal: Option<AbortSignal>,
    pub destination: RequestDestination,
    /// Browser-issued principal. Constructors fail closed until it is supplied.
    pub initiator: Initiator,
    pub credentials: Credentials,
}
impl Request {
    /// A get request to the specified Url and an empty body
    pub fn get(url: Url) -> Self {
        Self {
            url,
            method: Method::GET,
            content_type: None,
            headers: HeaderMap::new(),
            body: Body::Empty,
            signal: None,
            destination: RequestDestination::default(),
            initiator: Initiator::default(),
            credentials: Credentials::default(),
        }
    }

    pub fn initiator(mut self, initiator: Initiator) -> Self {
        self.initiator = initiator;
        self
    }

    pub fn destination(mut self, d: RequestDestination) -> Self {
        self.destination = d;
        self
    }

    pub fn signal(mut self, signal: AbortSignal) -> Self {
        self.signal = Some(signal);
        self
    }
}

#[derive(Debug, Clone)]
pub enum Body {
    Bytes(Bytes),
    Form(FormData),
    Empty,
}

/// A list of form entries used for form submission
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormData(pub Vec<Entry>);
impl FormData {
    /// Creates a new empty FormData
    pub fn new() -> Self {
        FormData(Vec::new())
    }
}
impl Serialize for FormData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq_serializer = serializer.serialize_seq(Some(self.len()))?;
        for entry in &self.0 {
            seq_serializer.serialize_element(entry)?;
        }
        seq_serializer.end()
    }
}
impl Deref for FormData {
    type Target = Vec<Entry>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A single form entry consisting of a name and value
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: EntryValue,
}
impl Serialize for Entry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut serializer = serializer.serialize_tuple(2)?;
        serializer.serialize_element(&self.name)?;
        match &self.value {
            EntryValue::String(s) => serializer.serialize_element(s)?,
            EntryValue::File(p) => serializer.serialize_element(p.to_str().unwrap_or_default())?,
            EntryValue::EmptyFile => serializer.serialize_element("")?,
        }
        serializer.end()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryValue {
    String(String),
    File(PathBuf),
    EmptyFile,
}
impl AsRef<str> for EntryValue {
    fn as_ref(&self) -> &str {
        match self {
            EntryValue::String(s) => s,
            EntryValue::File(p) => p.to_str().unwrap_or_default(),
            EntryValue::EmptyFile => "",
        }
    }
}

impl From<&str> for EntryValue {
    fn from(value: &str) -> Self {
        EntryValue::String(value.to_string())
    }
}
impl From<PathBuf> for EntryValue {
    fn from(value: PathBuf) -> Self {
        EntryValue::File(value)
    }
}

/// A default noop NetProvider
#[derive(Default)]
pub struct DummyNetProvider;
impl NetProvider for DummyNetProvider {
    fn fetch(&self, _doc_id: usize, _request: Request, _handler: Box<dyn NetHandler>) {}
    fn is_noop(&self) -> bool {
        true
    }
}

/// The AbortController interface represents a controller object that
/// allows you to abort one or more Web requests as and when desired via
/// `AbortController`.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AbortController>
#[derive(Debug, Default)]
pub struct AbortController {
    pub signal: AbortSignal,
}

impl AbortController {
    /// The abort() method of the AbortController interface aborts
    /// an asynchronous operation before it has completed.
    /// This is able to abort fetch requests.
    ///
    /// <https://developer.mozilla.org/en-US/docs/Web/API/AbortController/abort>
    pub fn abort(self) {
        let waiters = {
            let mut state = self
                .signal
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.aborted = true;
            std::mem::take(&mut state.waiters)
        };
        for (_, waker) in waiters {
            waker.wake();
        }
    }
}

/// The AbortSignal interface represents a signal object that allows you to
/// communicate that an operation should be aborted via `AbortController`.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AbortSignal>
#[derive(Debug, Default, Clone)]
pub struct AbortSignal(Arc<Mutex<AbortState>>);

#[derive(Debug, Default)]
struct AbortState {
    aborted: bool,
    next: u64,
    waiters: std::collections::BTreeMap<u64, std::task::Waker>,
}

/// A reactive cancellation future, with one registration per operation.
///
/// Dropping this future unregisters its waker. It does not abort the signal.
pub struct AbortWaiter {
    signal: AbortSignal,
    id: u64,
}

impl std::future::Future for AbortWaiter {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let mut state = self
            .signal
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.aborted {
            std::task::Poll::Ready(())
        } else {
            state.waiters.insert(self.id, cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

impl Drop for AbortWaiter {
    fn drop(&mut self) {
        self.signal
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .waiters
            .remove(&self.id);
    }
}

impl AbortSignal {
    /// The aborted read-only property returns a value that indicates whether
    /// the asynchronous operations the signal is communicating with has been aborted.
    pub fn aborted(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .aborted
    }

    /// Completes when the issuer aborts, waking every pending waiter.
    ///
    /// A signal that is already aborted completes on the first poll. A
    /// signal can be shared by requests and WebSocket upgrade attempts.
    pub fn cancelled(&self) -> AbortWaiter {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .expect("abort waiter identifier exhausted");
        AbortWaiter {
            signal: self.clone(),
            id,
        }
    }
}

//! Networking (HTTP, filesystem, Data URIs) for Blitz
//!
//! Provides implementations of [`blitz_traits::net::NetProvider`], which loads
//! the resources a document needs, and of
//! [`blitz_traits::platform::FetchProvider`], which serves a guest's `fetch()`.
//!
//! Both use one native HTTP/1.1 client, sharing its connection pool, cookie
//! jar, compression codecs, private cache and six connection slots per origin.
//! The `http2` feature is reserved for the subsequent HTTP/2 transport.
//!
//! Creating a native provider starts nagoya's process background runtime and
//! the house client's shared reactor if absent. Multiple providers reuse them.
//! Embedders do not need to enter an executor: callback requests use
//! `nagoya::spawn`, and async methods also work under `nagoya::block_on`.
//!
//! Cookie persistence belongs to the embedder's cookie jar. Disk caching is
//! attached explicitly with a profile directory; no global cache path is used.
//! On wasm32, local URL handling remains available, but this native socket
//! transport does not provide HTTP.

#[cfg(not(target_arch = "wasm32"))]
mod h1;

use blitz_traits::net::{
    AbortSignal, Body, Bytes, CookieJar as CookieStorage, NetHandler, NetProvider, NetWaker, Request,
    http::{HeaderValue, header::CONTENT_TYPE},
};
use blitz_traits::platform::{
    FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse, HeaderMap, StatusCode,
};
use data_url::DataUrl;
use std::{marker::PhantomData, pin::Pin, sync::Arc, task::Poll};

/// Cookie storage supplied to [`Provider::with_user_agent_and_cookie_provider`].
///
/// This house trait keeps embedders independent of the HTTP client. The provider
/// consults the jar for each request and stores cookies from every response,
/// including intermediate redirects and error responses.
#[cfg(feature = "cookies")]
pub use blitz_traits::net::CookieJar;

#[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
pub use h1::cache::PrivateCache;

/// The `User-Agent` sent when a caller does not choose one.
///
/// Kept as it was so that existing consumers see no change in what servers
/// send them. It is worth knowing that this string is a 2020 Firefox on Linux,
/// and that a large share of the popular web serves a degraded page, a
/// challenge stub or nothing at all to a client it does not recognise. A
/// consumer that wants the page a current browser would get has to say so with
/// [`Provider::with_user_agent`].
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0";

#[cfg(not(target_arch = "wasm32"))]
type Client = h1::H1Client;

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct Client;

#[cfg(target_arch = "wasm32")]
impl Client {
    fn new(
        user_agent: &str,
        _cookies: Option<Arc<dyn CookieStorage>>,
    ) -> Result<Self, FetchError> {
        HeaderValue::from_str(user_agent)
            .map_err(|error| FetchError::InvalidRequest(error.to_string()))?;
        Ok(Self)
    }

    async fn fetch(&self, _request: FetchRequest) -> Result<FetchResponse, FetchError> {
        Err(FetchError::Network(
            "the native HTTP transport is unavailable on wasm32".to_owned(),
        ))
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn(fut: impl Future + 'static) {
    wasm_bindgen_futures::spawn_local(async move {
        fut.await;
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn<F>(fut: F)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    nagoya::spawn(fut);
}

/// Document and script networking over one house client.
///
/// Native construction initializes the shared runtime and reactor. Supply a
/// cookie jar for cookie storage and attach a profile cache before sharing the
/// provider with documents.
pub struct Provider {
    client: Client,
    waker: Arc<dyn NetWaker>,
    user_agent: Arc<str>,
    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    cache_manager: Option<Arc<PrivateCache>>,
}
impl Provider {
    pub fn new(waker: Option<Arc<dyn NetWaker>>) -> Self {
        Self::with_user_agent(waker, DEFAULT_USER_AGENT)
    }

    /// A provider that identifies itself as `user_agent`.
    ///
    /// The string is sent verbatim on document, subresource and script
    /// requests alike, because a server that decides what to serve from the
    /// `User-Agent` will do so on every one of them, and a client that agreed
    /// with itself on only some would fetch a page assembled for two different
    /// browsers.
    pub fn with_user_agent(waker: Option<Arc<dyn NetWaker>>, user_agent: &str) -> Self {
        Self::with_cookie_jar(waker, user_agent, None)
    }

    /// A provider that uses an embedder-owned cookie jar and identity.
    ///
    /// The jar supplies cookies for each request and receives cookies from
    /// intermediate redirects, error responses, and final responses.
    #[cfg(feature = "cookies")]
    pub fn with_user_agent_and_cookie_provider<C>(
        waker: Option<Arc<dyn NetWaker>>,
        user_agent: &str,
        cookie_provider: Arc<C>,
    ) -> Self
    where
        C: CookieJar,
    {
        Self::with_cookie_jar(waker, user_agent, Some(cookie_provider))
    }

    fn with_cookie_jar(
        waker: Option<Arc<dyn NetWaker>>,
        user_agent: &str,
        cookies: Option<Arc<dyn CookieStorage>>,
    ) -> Self {
        let client = Client::new(user_agent, cookies)
            .expect("failed to initialize the house HTTP client");
        let waker = waker.unwrap_or(Arc::new(DummyNetWaker));
        Self {
            client,
            waker,
            user_agent: Arc::from(user_agent),
            #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
            cache_manager: None,
        }
    }

    /// Attach a cache already opened for this profile.
    ///
    /// Keep one open cache instance per profile. Document and script requests
    /// share it, and cached Set-Cookie headers are never replayed.
    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub fn with_cache(mut self, cache: Arc<PrivateCache>) -> Self {
        self.client = self.client.with_cache(cache.clone());
        self.cache_manager = Some(cache);
        self
    }

    /// Open persisted WorkTable metadata and bodies under the profile directory.
    ///
    /// Opening runs on a nagoya worker. Failure leaves the caller free to build
    /// a provider without a cache instead.
    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub async fn with_cache_directory(
        self,
        profile_directory: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Self> {
        let directory = profile_directory.as_ref().to_owned();
        let cache = nagoya::spawn(async move { PrivateCache::open(directory).await })
            .await
            .ok_or_else(|| std::io::Error::other("cache opener was cancelled"))??;
        Ok(self.with_cache(cache))
    }

    pub fn shared(waker: Option<Arc<dyn NetWaker>>) -> Arc<dyn NetProvider> {
        Arc::new(Self::new(waker))
    }
    pub fn shared_with_user_agent(
        waker: Option<Arc<dyn NetWaker>>,
        user_agent: &str,
    ) -> Arc<dyn NetProvider> {
        Arc::new(Self::with_user_agent(waker, user_agent))
    }
    /// What this provider identifies itself as.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
    pub fn is_empty(&self) -> bool {
        Arc::strong_count(&self.waker) == 1
    }
    pub fn count(&self) -> usize {
        Arc::strong_count(&self.waker) - 1
    }

    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub async fn clear_cache(&self) {
        if let Some(cache) = &self.cache_manager {
            let cache = cache.clone();
            let result = nagoya::spawn(async move { cache.clear().await }).await;
            if let Some(Err(e)) = result {
                #[cfg(feature = "tracing")]
                tracing::error!("Failed to clear HTTP cache: {:?}", e);
                #[cfg(not(feature = "tracing"))]
                let _ = e;
            }
        }
    }
}
impl Provider {
    async fn fetch_inner(
        client: Client,
        request: Request,
    ) -> Result<(String, Bytes), ProviderError> {
        let response = Self::fetch_response_inner(client, request).await?;
        Ok((response.url.to_string(), response.body))
    }

    #[allow(clippy::type_complexity)]
    pub fn fetch_with_callback(
        &self,
        request: Request,
        callback: Box<dyn FnOnce(Result<(String, Bytes), ProviderError>) + Send + Sync + 'static>,
    ) {
        #[cfg(feature = "tracing")]
        let url = request.url.to_string();

        let client = self.client.clone();
        spawn(async move {
            let result = Self::fetch_inner(client, request).await;

            #[cfg(feature = "tracing")]
            if let Err(e) = &result {
                tracing::error!(url = url.as_str(), error = ?e, "Fetching");
            } else {
                tracing::info!(url = url.as_str(), "Success fetching");
            }

            callback(result);
        });
    }

    pub async fn fetch_async(&self, request: Request) -> Result<(String, Bytes), ProviderError> {
        #[cfg(feature = "tracing")]
        let url = request.url.to_string();

        let result = Self::fetch_inner(self.client.clone(), request).await;

        #[cfg(feature = "tracing")]
        if let Err(e) = &result {
            tracing::error!(url = url.as_str(), error = ?e, "Fetching");
        } else {
            tracing::info!(url = url.as_str(), "Success fetching");
        }

        result
    }

    /// Fetch, keeping the response metadata that [`Provider::fetch_async`]
    /// discards.
    ///
    /// HTTP error statuses remain errors on this document-loading API.
    /// `FetchProvider` instead delivers every HTTP response, including errors.
    /// A `data:` URL supplies its own Content-Type; a `file:` URL has none.
    pub async fn fetch_response_async(
        &self,
        request: Request,
    ) -> Result<FetchResponse, ProviderError> {
        Self::fetch_response_inner(self.client.clone(), request).await
    }

    async fn fetch_response_inner(
        client: Client,
        request: Request,
    ) -> Result<FetchResponse, ProviderError> {
        let url = request.url.clone();
        match url.scheme() {
            "data" => {
                // Scoped so the borrow of `url` ends before it is moved into
                // the response.
                let (body, headers) = {
                    let data_url = DataUrl::process(url.as_str())?;
                    let decoded = data_url.decode_to_vec()?;
                    let mut headers = HeaderMap::new();
                    if let Ok(value) = data_url.mime_type().to_string().parse() {
                        headers.insert(CONTENT_TYPE, value);
                    }
                    (Bytes::from(decoded.0), headers)
                };
                Ok(FetchResponse::new(url, StatusCode::OK)
                    .headers(headers)
                    .body(body))
            }
            "file" => {
                let file_content = std::fs::read(url.path())?;
                Ok(FetchResponse::new(url, StatusCode::OK).body(Bytes::from(file_content)))
            }
            "http" | "https" => {
                let mut wire_request = FetchRequest::get(request.url).method(request.method);
                wire_request.headers = request.headers;
                let content_type = request.content_type.or_else(|| {
                    wire_request
                        .headers
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned)
                });
                if let Some(content_type) = &content_type {
                    let value = HeaderValue::from_str(content_type)
                        .map_err(|error| ProviderError::InvalidRequest(error.to_string()))?;
                    wire_request.headers.insert(CONTENT_TYPE, value);
                }
                wire_request.body = encode_body(
                    request.body,
                    content_type.as_deref(),
                    &mut wire_request.headers,
                )?;
                let response = client
                    .fetch(wire_request)
                    .await
                    .map_err(|error| ProviderError::Network(error.to_string()))?;

                if !response.status.is_success() {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        url = response.url.as_str(),
                        status = response.status.as_u16(),
                        "HTTP error status"
                    );
                    return Err(ProviderError::HttpStatus {
                        status: response.status,
                        url: response.url.to_string(),
                    });
                }
                Ok(response)
            }
            scheme => Err(ProviderError::UnsupportedScheme(scheme.to_owned())),
        }
    }
}

/// The `fetch()` path.
///
/// Separate from [`Provider::fetch_inner`] rather than layered on it, and the
/// reason is the whole point of the trait: `fetch_inner` returns
/// `(String, Bytes)` and turns any non-success status into a `ProviderError`
/// that [`NetProvider::fetch`] then logs and drops. A caller loading an image
/// wants exactly that. A `fetch()` caller wants the 404.
impl Provider {
    async fn platform_fetch_inner(
        client: Client,
        request: FetchRequest,
    ) -> Result<FetchResponse, FetchError> {
        match request.url.scheme() {
            "data" => Self::platform_fetch_data(request),
            "file" => Self::platform_fetch_file(request),
            "http" | "https" => client
                .fetch(request)
                .await
                .map_err(|error| FetchError::Network(error.to_string())),
            scheme => Err(FetchError::UnsupportedScheme(scheme.to_owned())),
        }
    }

    /// A `data:` URL, answered as a synthetic 200.
    ///
    /// The status and the `Content-Type` are invented, because a data URL has
    /// no server to supply them, and inventing them is what the fetch
    /// specification requires: a `data:` response is a 200 whose content type
    /// is the one encoded in the URL.
    fn platform_fetch_data(request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let data_url = DataUrl::process(request.url.as_str())
            .map_err(|err| FetchError::InvalidRequest(format!("{err:?}")))?;
        let mime = data_url.mime_type().to_string();
        let (body, _) = data_url
            .decode_to_vec()
            .map_err(|err| FetchError::InvalidRequest(format!("{err:?}")))?;

        let mut headers = HeaderMap::new();
        if let Ok(value) = mime.parse() {
            headers.insert(CONTENT_TYPE, value);
        }

        Ok(FetchResponse::new(request.url, StatusCode::OK)
            .headers(headers)
            .body(Bytes::from(body)))
    }

    /// A `file:` URL, answered as a synthetic 200.
    ///
    /// Goes through [`Url::to_file_path`] rather than `Url::path`, which is
    /// what [`Provider::fetch_inner`] uses. `path` hands back the URL's
    /// percent-encoded path component as a string, so a file whose name
    /// contains a space or a `#` is looked up under the wrong name, and on
    /// Windows the leading slash makes it wrong outright. `to_file_path`
    /// decodes and refuses a URL that does not name a local path.
    ///
    /// **This is not access control, and it is not claiming to be.** Whether a
    /// document may read local files at all is an origin question, and origins
    /// are not visible here; see `blitz-platform-api`, which holds the origin
    /// and is where such a policy belongs.
    fn platform_fetch_file(request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let path = request.url.to_file_path().map_err(|()| {
            FetchError::InvalidRequest(format!("not a local path: {}", request.url))
        })?;

        let body = std::fs::read(path).map_err(|err| FetchError::Network(err.to_string()))?;

        Ok(FetchResponse::new(request.url, StatusCode::OK).body(Bytes::from(body)))
    }
}

impl FetchProvider for Provider {
    fn fetch(&self, request: FetchRequest, handler: Box<dyn FetchHandler>) {
        let client = self.client.clone();

        #[cfg(feature = "tracing")]
        let url = request.url.to_string();

        spawn(async move {
            let result = Self::platform_fetch_inner(client, request).await;

            #[cfg(feature = "tracing")]
            match &result {
                Ok(response) => tracing::info!(
                    url = url.as_str(),
                    status = response.status.as_u16(),
                    "fetch complete"
                ),
                Err(error) => tracing::error!(url = url.as_str(), error = ?error, "fetch failed"),
            }

            handler.complete(result);
        });
    }
}

impl NetProvider for Provider {
    fn fetch(&self, doc_id: usize, mut request: Request, handler: Box<dyn NetHandler>) {
        let client = self.client.clone();

        #[cfg(feature = "tracing")]
        tracing::info!(url = request.url.as_str(), "Fetching");

        let waker = self.waker.clone();
        spawn(async move {
            #[cfg(feature = "tracing")]
            let url = request.url.to_string();

            let signal = request.signal.take();
            let result = if let Some(signal) = signal {
                AbortFetch::new(
                    signal,
                    Box::pin(async move { Self::fetch_inner(client, request).await }),
                )
                .await
            } else {
                Self::fetch_inner(client, request).await
            };

            waker.wake(doc_id);

            match result {
                Ok((response_url, bytes)) => {
                    handler.bytes(response_url, bytes);
                    #[cfg(feature = "tracing")]
                    tracing::info!(url = url.as_str(), "Success fetching");
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(url = url.as_str(), error = ?e, "Error fetching");
                    #[cfg(not(feature = "tracing"))]
                    let _ = e;
                }
            };
        });
    }
}

struct AbortFetch<F, T> {
    signal: AbortSignal,
    future: F,
    _rt: PhantomData<T>,
}

impl<F, T> AbortFetch<F, T> {
    fn new(signal: AbortSignal, future: F) -> Self {
        Self {
            signal,
            future,
            _rt: PhantomData,
        }
    }
}

impl<F, T> Future for AbortFetch<F, T>
where
    F: Future + Unpin + 'static,
    F::Output: Into<Result<T, ProviderError>> + 'static,
    T: Unpin,
{
    type Output = Result<T, ProviderError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.signal.aborted() {
            return Poll::Ready(Err(ProviderError::Abort));
        }

        match Pin::new(&mut self.future).poll(cx) {
            Poll::Ready(output) => Poll::Ready(output.into()),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Debug)]
pub enum ProviderError {
    Abort,
    Io(std::io::Error),
    DataUrl(data_url::DataUrlError),
    DataUrlBase64(data_url::forgiving_base64::InvalidBase64),
    Network(String),
    InvalidRequest(String),
    UnsupportedScheme(String),
    HttpStatus {
        status: StatusCode,
        url: String,
    },
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Abort => write!(f, "request aborted"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::DataUrl(e) => write!(f, "data url error: {e:?}"),
            Self::DataUrlBase64(e) => write!(f, "data url base64 error: {e:?}"),
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::InvalidRequest(e) => write!(f, "invalid request: {e}"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported URL scheme: {scheme}"),
            Self::HttpStatus { status, url } => write!(f, "HTTP {status} for {url}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<std::io::Error> for ProviderError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<data_url::DataUrlError> for ProviderError {
    fn from(value: data_url::DataUrlError) -> Self {
        Self::DataUrl(value)
    }
}

impl From<data_url::forgiving_base64::InvalidBase64> for ProviderError {
    fn from(value: data_url::forgiving_base64::InvalidBase64) -> Self {
        Self::DataUrlBase64(value)
    }
}

fn encode_body(
    body: Body,
    content_type: Option<&str>,
    headers: &mut HeaderMap,
) -> Result<Option<Bytes>, ProviderError> {
    match body {
        Body::Bytes(bytes) => Ok(Some(bytes)),
        Body::Empty => Ok(None),
        Body::Form(form) => {
            let mime = content_type
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .unwrap_or("");
            if mime.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
                let mut encoded = url::form_urlencoded::Serializer::new(String::new());
                for entry in &form.0 {
                    encoded.append_pair(&entry.name, entry.value.as_ref());
                }
                return Ok(Some(Bytes::from(encoded.finish())));
            }
            if mime.eq_ignore_ascii_case("multipart/form-data") {
                #[cfg(all(feature = "multipart", not(target_arch = "wasm32")))]
                {
                    return encode_multipart(form, headers).map(Some);
                }
                #[cfg(not(all(feature = "multipart", not(target_arch = "wasm32"))))]
                {
                    let _ = headers;
                    return Err(ProviderError::InvalidRequest(
                        "multipart requires the native multipart feature".to_owned(),
                    ));
                }
            }
            Err(ProviderError::InvalidRequest(
                "unsupported form Content-Type".to_owned(),
            ))
        }
    }
}

#[cfg(all(feature = "multipart", not(target_arch = "wasm32")))]
fn encode_multipart(
    form: blitz_traits::net::FormData,
    headers: &mut HeaderMap,
) -> Result<Bytes, ProviderError> {
    use blitz_traits::net::{Entry, EntryValue};
    use psc_nanoid::{Nanoid, alphabet::Base64UrlAlphabet};
    use std::io::Read;

    let boundary = Nanoid::<21, Base64UrlAlphabet>::new().to_string();
    let mut encoded = Vec::new();
    for Entry { name, value } in form.0 {
        let mut head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
            multipart_parameter(&name),
        );
        let body = match value {
            EntryValue::String(value) => Bytes::from(value),
            EntryValue::File(path) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                head.push_str(&format!(
                    "; filename=\"{}\"\r\nContent-Type: application/octet-stream",
                    multipart_parameter(&name),
                ));
                let file = std::fs::File::open(path)?;
                let mut body = Vec::new();
                file.take(h1::MAX_BODY_BYTES as u64 + 1)
                    .read_to_end(&mut body)?;
                if body.len() > h1::MAX_BODY_BYTES {
                    return Err(ProviderError::InvalidRequest(
                        "multipart file exceeds the body limit".to_owned(),
                    ));
                }
                Bytes::from(body)
            }
            EntryValue::EmptyFile => {
                head.push_str("; filename=\"\"\r\nContent-Type: application/octet-stream");
                Bytes::new()
            }
        };
        head.push_str("\r\n\r\n");
        let required = head
            .len()
            .checked_add(body.len())
            .and_then(|length| length.checked_add(2))
            .and_then(|length| length.checked_add(encoded.len()))
            .ok_or_else(|| ProviderError::InvalidRequest("multipart body is too large".to_owned()))?;
        if required > h1::MAX_BODY_BYTES {
            return Err(ProviderError::InvalidRequest(
                "multipart body exceeds the body limit".to_owned(),
            ));
        }
        encoded.extend_from_slice(head.as_bytes());
        encoded.extend_from_slice(&body);
        encoded.extend_from_slice(b"\r\n");
    }
    let end = format!("--{boundary}--\r\n");
    if end.len() > h1::MAX_BODY_BYTES.saturating_sub(encoded.len()) {
        return Err(ProviderError::InvalidRequest(
            "multipart body exceeds the body limit".to_owned(),
        ));
    }
    encoded.extend_from_slice(end.as_bytes());
    let content_type = HeaderValue::from_str(&format!("multipart/form-data; boundary={boundary}"))
        .map_err(|error| ProviderError::InvalidRequest(error.to_string()))?;
    headers.insert(CONTENT_TYPE, content_type);
    Ok(Bytes::from(encoded))
}

#[cfg(all(feature = "multipart", not(target_arch = "wasm32")))]
fn multipart_parameter(value: &str) -> String {
    value
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22")
}

struct DummyNetWaker;
impl NetWaker for DummyNetWaker {
    fn wake(&self, _client_id: usize) {}
}

//! Stable 0.6 API over the policy-aware implementation.

use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};
use blitz_traits::platform::{
    FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse, StatusCode,
};
use std::sync::Arc;

/// The browser's document and script network provider.
pub struct Provider(crate::core::Provider);

impl Provider {
    /// Bind a browser-issued principal before handing networking to a document.
    pub fn for_scope(
        self: &Arc<Self>,
        initiator: blitz_traits::net::Initiator,
    ) -> Arc<crate::ScopedProvider> {
        Arc::new(crate::ScopedProvider::new(Arc::clone(self), initiator))
    }

    pub fn new(waker: Option<Arc<dyn NetWaker>>) -> Self {
        Self(crate::core::Provider::new(waker))
    }

    pub fn with_user_agent(waker: Option<Arc<dyn NetWaker>>, user_agent: &str) -> Self {
        Self(crate::core::Provider::with_user_agent(waker, user_agent))
    }

    #[cfg(feature = "cookies")]
    pub fn with_user_agent_and_cookie_provider<C>(
        waker: Option<Arc<dyn NetWaker>>,
        user_agent: &str,
        cookie_provider: Arc<C>,
    ) -> Self
    where
        C: crate::CookieJar,
    {
        Self(crate::core::Provider::with_user_agent_and_cookie_provider(
            waker,
            user_agent,
            cookie_provider,
        ))
    }

    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub fn with_cache(self, cache: Arc<crate::PrivateCache>) -> Self {
        Self(self.0.with_cache(cache))
    }

    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub async fn with_cache_directory(
        self,
        directory: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Self> {
        self.0.with_cache_directory(directory).await.map(Self)
    }

    pub fn shared(waker: Option<Arc<dyn NetWaker>>) -> Arc<dyn NetProvider> {
        crate::core::Provider::shared(waker)
    }

    pub fn shared_with_user_agent(
        waker: Option<Arc<dyn NetWaker>>,
        user_agent: &str,
    ) -> Arc<dyn NetProvider> {
        crate::core::Provider::shared_with_user_agent(waker, user_agent)
    }

    pub fn user_agent(&self) -> &str {
        self.0.user_agent()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn count(&self) -> usize {
        self.0.count()
    }

    #[cfg(all(feature = "cache", not(target_arch = "wasm32")))]
    pub async fn clear_cache(&self) {
        self.0.clear_cache().await;
    }

    #[allow(clippy::type_complexity)]
    pub fn fetch_with_callback(
        &self,
        request: Request,
        callback: Box<dyn FnOnce(Result<(String, Bytes), ProviderError>) + Send + Sync + 'static>,
    ) {
        self.0.fetch_with_callback(
            request,
            Box::new(move |result| {
                callback(result.map_err(ProviderError::from));
            }),
        );
    }

    pub async fn fetch_async(&self, request: Request) -> Result<(String, Bytes), ProviderError> {
        self.0
            .fetch_async(request)
            .await
            .map_err(ProviderError::from)
    }

    pub async fn fetch_response_async(
        &self,
        request: Request,
    ) -> Result<FetchResponse, ProviderError> {
        self.0
            .fetch_response_async(request)
            .await
            .map_err(ProviderError::from)
    }

    pub async fn fetch_platform_async(
        &self,
        request: FetchRequest,
    ) -> Result<FetchResponse, FetchError> {
        self.0.fetch_platform_async(request).await
    }
}

impl FetchProvider for Provider {
    fn fetch(&self, request: FetchRequest, handler: Box<dyn FetchHandler>) {
        FetchProvider::fetch(&self.0, request, handler);
    }
}

impl NetProvider for Provider {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        NetProvider::fetch(&self.0, doc_id, request, handler);
    }
}

/// Preserve the exhaustive public 0.6 error vocabulary. Policy diagnostics
/// appear in Network; the platform API retains its existing typed Blocked.
#[derive(Debug)]
pub enum ProviderError {
    Abort,
    Io(std::io::Error),
    DataUrl(data_url::DataUrlError),
    DataUrlBase64(data_url::forgiving_base64::InvalidBase64),
    Network(String),
    TlsPolicy(crate::TlsPolicyError),
    InvalidRequest(String),
    UnsupportedScheme(String),
    HttpStatus { status: StatusCode, url: String },
}

impl From<crate::core::ProviderError> for ProviderError {
    fn from(error: crate::core::ProviderError) -> Self {
        use crate::core::ProviderError as Core;
        match error {
            Core::Abort => Self::Abort,
            Core::Io(error) => Self::Io(error),
            Core::DataUrl(error) => Self::DataUrl(error),
            Core::DataUrlBase64(error) => Self::DataUrlBase64(error),
            Core::Network(error) => Self::Network(error),
            Core::Blocked(reason) => Self::Network(format!("blocked: {reason}")),
            Core::TlsPolicy(reason) => Self::TlsPolicy(reason),
            Core::InvalidRequest(error) => Self::InvalidRequest(error),
            Core::UnsupportedScheme(scheme) => Self::UnsupportedScheme(scheme),
            Core::HttpStatus { status, url } => Self::HttpStatus { status, url },
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Abort => write!(formatter, "request aborted"),
            Self::Io(error) => write!(formatter, "io error: {error}"),
            Self::DataUrl(error) => write!(formatter, "data url error: {error:?}"),
            Self::DataUrlBase64(error) => write!(formatter, "data url base64 error: {error:?}"),
            Self::Network(error) => write!(formatter, "network error: {error}"),
            Self::TlsPolicy(reason) => write!(formatter, "TLS policy: {reason}"),
            Self::InvalidRequest(error) => write!(formatter, "invalid request: {error}"),
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported URL scheme: {scheme}")
            }
            Self::HttpStatus { status, url } => write!(formatter, "HTTP {status} for {url}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<std::io::Error> for ProviderError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<data_url::DataUrlError> for ProviderError {
    fn from(error: data_url::DataUrlError) -> Self {
        Self::DataUrl(error)
    }
}

impl From<data_url::forgiving_base64::InvalidBase64> for ProviderError {
    fn from(error: data_url::forgiving_base64::InvalidBase64) -> Self {
        Self::DataUrlBase64(error)
    }
}

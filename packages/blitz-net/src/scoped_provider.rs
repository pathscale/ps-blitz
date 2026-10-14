use blitz_traits::net::{Bytes, Initiator, NetHandler, NetProvider, Request, Url};
use blitz_traits::platform::{
    FetchError, FetchHandler, FetchProvider, FetchRequest, FetchResponse,
};
use std::sync::Arc;

use crate::{Provider, ProviderError};

/// Native binding for one document or trusted browser context. Request fields
/// cannot replace this principal. Clones share typed scope and provider Arcs.
pub struct ScopedProvider {
    provider: Arc<Provider>,
    initiator: Initiator,
}

impl ScopedProvider {
    pub(crate) fn new(provider: Arc<Provider>, initiator: Initiator) -> Self {
        Self {
            provider,
            initiator,
        }
    }

    pub fn initiator(&self) -> Initiator {
        self.initiator.clone()
    }

    pub async fn fetch_async(
        &self,
        mut request: Request,
    ) -> Result<(String, Bytes), ProviderError> {
        request.initiator = self.initiator.clone();
        self.provider.fetch_async(request).await
    }

    pub async fn fetch_response_async(
        &self,
        mut request: Request,
    ) -> Result<FetchResponse, ProviderError> {
        request.initiator = self.initiator.clone();
        self.provider.fetch_response_async(request).await
    }

    pub async fn fetch_platform_async(
        &self,
        mut request: FetchRequest,
    ) -> Result<FetchResponse, FetchError> {
        request.initiator = self.initiator.clone();
        self.provider.fetch_platform_async(request).await
    }
}

impl NetProvider for ScopedProvider {
    fn fetch(&self, doc_id: usize, mut request: Request, handler: Box<dyn NetHandler>) {
        request.initiator = self.initiator.clone();
        NetProvider::fetch(self.provider.as_ref(), doc_id, request, handler);
    }

    fn resource_initiator(&self) -> Initiator {
        self.initiator()
    }

    fn for_child_document(&self, url: Url) -> Option<Arc<dyn NetProvider>> {
        Some(Arc::new(Self::new(
            Arc::clone(&self.provider),
            self.initiator.child(url),
        )))
    }
}

impl FetchProvider for ScopedProvider {
    fn fetch(&self, mut request: FetchRequest, handler: Box<dyn FetchHandler>) {
        request.initiator = self.initiator.clone();
        FetchProvider::fetch(self.provider.as_ref(), request, handler);
    }
}
